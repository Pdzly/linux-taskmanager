//! Runs process actions requested from the UI. Tries directly first; whatever the
//! kernel refuses is retried in one batch through `pkexec taskmonitor-helper`.

use std::path::PathBuf;
use std::process::Command;

use nix::errno::Errno;
use taskmonitor_core::actions::{self, Signal};
use taskmonitor_core::{Group, Process};

/// An action name from the UI ("end", "priority:-5", …), parsed.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    End,
    Kill,
    EndTree,
    Suspend,
    Resume,
    Efficiency(bool),
    Priority(i32),
    Affinity(Vec<bool>),
    OpenLocation,
}

impl Action {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "end" => Self::End,
            "kill" => Self::Kill,
            "tree" => Self::EndTree,
            "suspend" => Self::Suspend,
            "resume" => Self::Resume,
            "efficiency-on" => Self::Efficiency(true),
            "efficiency-off" => Self::Efficiency(false),
            "open" => Self::OpenLocation,
            _ => {
                if let Some(n) = s.strip_prefix("priority:") {
                    Self::Priority(n.parse().ok()?)
                } else {
                    let mask = s.strip_prefix("affinity:")?;
                    Self::Affinity(mask.chars().map(|c| c == '1').collect())
                }
            }
        })
    }

    fn verb(&self) -> &'static str {
        match self {
            Self::End | Self::EndTree => "Ended",
            Self::Kill => "Killed",
            Self::Suspend => "Suspended",
            Self::Resume => "Resumed",
            Self::Efficiency(true) => "Efficiency mode on for",
            Self::Efficiency(false) => "Efficiency mode off for",
            Self::Priority(_) => "Changed priority of",
            Self::Affinity(_) => "Changed affinity of",
            Self::OpenLocation => "Opened",
        }
    }

    fn apply(&self, pid: u32) -> Result<(), Errno> {
        match self {
            Self::End | Self::EndTree => actions::signal(pid, Signal::SIGTERM),
            Self::Kill => actions::signal(pid, Signal::SIGKILL),
            Self::Suspend => actions::signal(pid, Signal::SIGSTOP),
            Self::Resume => actions::signal(pid, Signal::SIGCONT),
            Self::Efficiency(on) => actions::set_efficiency(pid, *on),
            Self::Priority(n) => actions::set_nice(pid, *n),
            Self::Affinity(mask) => actions::set_affinity(pid, mask),
            Self::OpenLocation => Ok(()),
        }
    }

    /// Arguments for taskmonitor-helper (without the PIDs).
    fn helper_args(&self) -> Option<[String; 2]> {
        let s = |a: &str, b: String| Some([a.to_owned(), b]);
        match self {
            Self::End | Self::EndTree => s("signal", "TERM".into()),
            Self::Kill => s("signal", "KILL".into()),
            Self::Suspend => s("signal", "STOP".into()),
            Self::Resume => s("signal", "CONT".into()),
            Self::Efficiency(on) => s("efficiency", if *on { "on" } else { "off" }.into()),
            Self::Priority(n) => s("nice", n.to_string()),
            Self::Affinity(mask) => {
                let cpus: Vec<String> =
                    mask.iter().enumerate().filter(|(_, on)| **on).map(|(i, _)| i.to_string()).collect();
                s("affinity", cpus.join(","))
            }
            Self::OpenLocation => None,
        }
    }
}

/// The PIDs a table row stands for: one process, or every member of a group.
pub fn pids_for_key(key: &str, procs: &[Process]) -> Vec<u32> {
    if let Some(pid) = key.strip_prefix("pid:").and_then(|p| p.parse().ok()) {
        return vec![pid];
    }
    procs
        .iter()
        .filter(|p| match (&p.group, key.split_once(':')) {
            (Group::App(app), Some(("app", id))) => &*app.id == id,
            (Group::Background, Some(("bg", name))) | (Group::System, Some(("sys", name))) => &*p.name == name,
            _ => false,
        })
        .map(|p| p.pid)
        .collect()
}

/// Display name for a row key, for messages.
pub fn name_for_key(key: &str, procs: &[Process]) -> String {
    let pids = pids_for_key(key, procs);
    let Some(p) = procs.iter().find(|p| pids.contains(&p.pid)) else { return "process".into() };
    match &p.group {
        Group::App(app) if !key.starts_with("pid:") => app.name.to_string(),
        _ => p.name.to_string(),
    }
}

/// Performs `action` on the processes behind `key` and returns a message for the user.
/// Blocks while the polkit password prompt is open, so call it off the sampler thread.
pub fn perform(action: &Action, key: &str, procs: &[Process]) -> String {
    let name = name_for_key(key, procs);
    let mut pids = pids_for_key(key, procs);
    if pids.is_empty() {
        return format!("{name} is no longer running");
    }
    if *action == Action::EndTree {
        pids = actions::with_descendants(&pids, procs);
    }
    if *action == Action::OpenLocation {
        return open_location(pids[0], &name);
    }

    let mut denied = Vec::new();
    let mut errors = Vec::new();
    for &pid in &pids {
        match action.apply(pid) {
            Ok(()) | Err(Errno::ESRCH) => {} // ESRCH: already gone, which is fine
            Err(Errno::EPERM | Errno::EACCES) => denied.push(pid),
            Err(e) => errors.push(format!("{pid}: {}", e.desc())),
        }
    }
    if !denied.is_empty() {
        match elevate(action, &denied) {
            Ok(()) => {}
            Err(e) => errors.push(e),
        }
    }
    if errors.is_empty() {
        format!("{} {name}", action.verb())
    } else {
        format!("Couldn't change {name}: {}", errors.join("; "))
    }
}

/// Retries through the helper as root. pkexec shows the system's password prompt.
fn elevate(action: &Action, pids: &[u32]) -> Result<(), String> {
    let Some(args) = action.helper_args() else { return Err("not permitted".into()) };
    let helper = helper_path().ok_or_else(|| {
        // An AppImage's files live on a per-user mount that root can't read, so the
        // helper can't be run from inside it.
        if std::env::var_os("APPIMAGE").is_some() {
            "this needs administrator rights, which the AppImage can't request; install the .deb, .rpm or AUR package for that"
        } else {
            "administrator helper not installed"
        }
    })?;
    let out = Command::new("pkexec")
        .arg(helper)
        .args(args)
        .args(pids.iter().map(u32::to_string))
        .output()
        .map_err(|e| format!("couldn't run pkexec: {e}"))?;
    match out.status.code() {
        Some(0) => Ok(()),
        // pkexec: 126 = dialog dismissed / not authorized, 127 = auth failed.
        Some(126 | 127) => Err("administrator authorization was cancelled".into()),
        _ => Err(String::from_utf8_lossy(&out.stderr).trim().trim_start_matches("taskmonitor-helper: ").to_owned()),
    }
}

/// Next to our own binary during development, else the installed location.
fn helper_path() -> Option<PathBuf> {
    let beside = std::env::current_exe().ok()?.with_file_name("taskmonitor-helper");
    let installed = PathBuf::from("/usr/libexec/taskmonitor-helper");
    [beside, installed].into_iter().find(|p| p.exists())
}

/// Shows the executable in the file manager, selected; falls back to opening its folder.
fn open_location(pid: u32, name: &str) -> String {
    let Some(exe) = actions::exe(pid) else { return format!("Can't see where {name} runs from") };
    let uri = format!("file://{exe}");
    let shown = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.FileManager1",
            "--object-path",
            "/org/freedesktop/FileManager1",
            "--method",
            "org.freedesktop.FileManager1.ShowItems",
            &format!("['{uri}']"),
            "",
        ])
        .output()
        .is_ok_and(|o| o.status.success());
    if !shown {
        let dir = std::path::Path::new(&exe).parent().map(|d| d.display().to_string()).unwrap_or_default();
        let _ = Command::new("xdg-open").arg(dir).spawn();
    }
    format!("Opened the location of {name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_actions() {
        assert_eq!(Action::parse("priority:-5"), Some(Action::Priority(-5)));
        assert_eq!(Action::parse("affinity:101"), Some(Action::Affinity(vec![true, false, true])));
        assert_eq!(Action::parse("bogus"), None);
        assert_eq!(
            Action::Affinity(vec![true, false, true]).helper_args(),
            Some(["affinity".to_owned(), "0,2".to_owned()])
        );
    }

    #[test]
    fn ends_own_process_by_key() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let procs = [Process {
            pid,
            ppid: std::process::id(),
            name: "sleep".into(),
            cmdline: "sleep 30".into(),
            user: "me".into(),
            state: 'S',
            cpu: 0.0,
            memory: 0,
            read_bps: 0.0,
            write_bps: 0.0,
            io_total: None,
            threads: 1,
            nice: 0,
            efficiency: false,
            gpu: 0.0,
            icon: None,
            group: Group::Background,
        }];
        assert_eq!(perform(&Action::End, "bg:sleep", &procs), "Ended sleep");
        assert!(!child.wait().unwrap().success());
    }
}
