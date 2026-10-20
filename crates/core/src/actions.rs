//! Things you can do to a process. Each call acts directly; if the kernel refuses
//! with `EPERM`, the caller can retry through the privileged helper.

use std::collections::HashSet;
use std::fs;

use nix::errno::Errno;
use nix::sched::{CpuSet, sched_getaffinity, sched_setaffinity};
pub use nix::sys::signal::Signal;
use nix::sys::signal::kill;
use nix::unistd::Pid;

use crate::process::Process;

/// Linux's scheduling policy for "only run when nothing else wants the CPU".
pub const SCHED_IDLE: i32 = 5;

pub fn signal(pid: u32, sig: Signal) -> Result<(), Errno> {
    kill(Pid::from_raw(pid as i32), sig)
}

/// Sets the nice value (-20 highest priority … 19 lowest). Raising priority needs root.
pub fn set_nice(pid: u32, nice: i32) -> Result<(), Errno> {
    // SAFETY: plain syscall wrapper with integer arguments.
    let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS, pid as libc::id_t, nice) };
    if rc == 0 { Ok(()) } else { Err(Errno::last()) }
}

/// Windows' "Efficiency mode": lowest priority plus the SCHED_IDLE policy, so the
/// process only gets CPU time nothing else wants. Turning it off restores normal scheduling.
pub fn set_efficiency(pid: u32, on: bool) -> Result<(), Errno> {
    let policy = if on { SCHED_IDLE } else { libc::SCHED_OTHER };
    let param = libc::sched_param { sched_priority: 0 };
    // SAFETY: `param` is a valid, initialized sched_param for the duration of the call.
    if unsafe { libc::sched_setscheduler(pid as libc::pid_t, policy, &param) } != 0 {
        return Err(Errno::last());
    }
    set_nice(pid, if on { 19 } else { 0 })
}

/// Allowed CPUs for a process, one bool per logical CPU.
pub fn affinity(pid: u32, cpus: usize) -> Result<Vec<bool>, Errno> {
    let set = sched_getaffinity(Pid::from_raw(pid as i32))?;
    Ok((0..cpus).map(|i| set.is_set(i).unwrap_or(false)).collect())
}

pub fn set_affinity(pid: u32, allowed: &[bool]) -> Result<(), Errno> {
    let mut set = CpuSet::new();
    for (i, _) in allowed.iter().enumerate().filter(|(_, on)| **on) {
        set.set(i)?;
    }
    sched_setaffinity(Pid::from_raw(pid as i32), &set)
}

/// `roots` plus every process descended from them.
pub fn with_descendants(roots: &[u32], procs: &[Process]) -> Vec<u32> {
    let mut out: Vec<u32> = roots.to_vec();
    let mut seen: HashSet<u32> = roots.iter().copied().collect();
    let mut i = 0;
    while i < out.len() {
        let parent = out[i];
        for p in procs.iter().filter(|p| p.ppid == parent) {
            if seen.insert(p.pid) {
                out.push(p.pid);
            }
        }
        i += 1;
    }
    out
}

/// Executable path, for "Open file location".
pub fn exe(pid: u32) -> Option<String> {
    fs::read_link(format!("/proc/{pid}/exe")).ok().map(|p| p.to_string_lossy().into_owned())
}

/// Facts for the Properties sheet, as (label, value) pairs.
pub fn info(pid: u32) -> Vec<(String, String)> {
    let read = |f: &str| fs::read_to_string(format!("/proc/{pid}/{f}")).unwrap_or_default();
    let link = |f: &str| {
        fs::read_link(format!("/proc/{pid}/{f}")).map_or_else(|e| format!("Not accessible ({e})"), |p| p.display().to_string())
    };
    let status = read("status");
    let field = |key: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix(':')))
            .map(|v| v.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    };
    let started = crate::process::parse_stat(&read("stat")).map(|(_, s)| s.start_time).unwrap_or(0);
    let clk = nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK).ok().flatten().unwrap_or(100) as u64;
    let boot_uptime = crate::system::parse_uptime(&fs::read_to_string("/proc/uptime").unwrap_or_default());
    let running_for = boot_uptime.saturating_sub(started / clk.max(1));
    vec![
        ("PID".into(), pid.to_string()),
        ("Parent PID".into(), field("PPid")),
        ("State".into(), field("State")),
        ("User ID".into(), field("Uid").split(' ').next().unwrap_or("").to_owned()),
        ("Executable".into(), link("exe")),
        ("Working directory".into(), link("cwd")),
        ("Command line".into(), read("cmdline").trim_end_matches('\0').replace('\0', " ")),
        ("Running for".into(), format!("{}h {}m {}s", running_for / 3600, running_for / 60 % 60, running_for % 60)),
        ("Threads".into(), field("Threads")),
        ("Memory (resident)".into(), field("VmRSS")),
        ("Memory (peak)".into(), field("VmHWM")),
        ("Swap".into(), field("VmSwap")),
        ("Open files".into(), fs::read_dir(format!("/proc/{pid}/fd")).map_or("Not accessible".into(), |d| d.count().to_string())),
        ("Control group".into(), read("cgroup").lines().find_map(|l| l.strip_prefix("0::")).unwrap_or("").to_owned()),
    ]
}

/// Starts a command line through `sh -c`, detached from TaskMonitor (own process
/// group, no inherited terminal), like Windows' "Run new task".
pub fn run(command: &str) -> std::io::Result<u32> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    Ok(child.id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::Group;

    fn p(pid: u32, ppid: u32) -> Process {
        Process {
            pid,
            ppid,
            name: "x".into(),
            cmdline: "".into(),
            user: "u".into(),
            state: 'S',
            cpu: 0.0,
            memory: 0,
            read_bps: 0.0,
            write_bps: 0.0,
            threads: 1,
            nice: 0,
            efficiency: false,
            gpu: 0.0,
            group: Group::Background,
        }
    }

    #[test]
    fn collects_whole_tree() {
        let procs = [p(1, 0), p(10, 1), p(11, 10), p(12, 11), p(20, 1)];
        let mut tree = with_descendants(&[10], &procs);
        tree.sort();
        assert_eq!(tree, [10, 11, 12]);
    }

    #[test]
    fn suspend_and_resume_own_child() {
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        signal(pid, Signal::SIGSTOP).unwrap();
        signal(pid, Signal::SIGCONT).unwrap();
        set_efficiency(pid, true).unwrap();
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        assert!(crate::process::parse_stat(&stat).unwrap().1.efficiency);
        signal(pid, Signal::SIGTERM).unwrap();
        child.wait().unwrap();
    }
}
