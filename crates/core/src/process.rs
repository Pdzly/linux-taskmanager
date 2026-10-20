//! Per-process figures from `/proc/[pid]/*`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::time::Instant;

use crate::apps::{App, Registry};

/// Which Processes-tab section a process belongs to.
#[derive(Clone, Debug, PartialEq)]
pub enum Group {
    /// Launched as a desktop app.
    App(App),
    /// Another process of the current user.
    Background,
    /// Belongs to root or another user.
    System,
    /// Kernel thread (no user space).
    Kernel,
}

#[derive(Clone, Debug)]
pub struct Process {
    pub pid: u32,
    pub ppid: u32,
    pub name: Arc<str>,
    pub cmdline: Arc<str>,
    pub user: Arc<str>,
    pub state: char,
    /// Share of the whole machine (all cores), `0.0..=1.0`, like Windows.
    pub cpu: f32,
    /// Private resident memory in bytes (resident minus shared pages).
    pub memory: u64,
    pub read_bps: f64,
    pub write_bps: f64,
    pub threads: u32,
    pub nice: i32,
    /// Running under SCHED_IDLE (TaskMonitor's "Efficiency mode").
    pub efficiency: bool,
    /// Busiest GPU engine, `0.0..=1.0`. Filled in from [`crate::GpuSampler`].
    pub gpu: f32,
    pub group: Group,
}

/// Fields of `/proc/[pid]/stat` we use.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stat {
    pub state: char,
    pub ppid: u32,
    /// utime + stime, in clock ticks.
    pub ticks: u64,
    pub nice: i32,
    pub threads: u32,
    pub start_time: u64,
    pub efficiency: bool,
}

/// Parses `/proc/[pid]/stat`, returning (comm, fields).
/// comm can contain spaces and parentheses, so fields are read after the last ')'.
pub fn parse_stat(text: &str) -> Option<(&str, Stat)> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let comm = &text[open + 1..close];
    let f: Vec<&str> = text[close + 1..].split_ascii_whitespace().collect();
    // Index = field number - 3 (fields 1 and 2 are pid and comm).
    let num = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    Some((
        comm,
        Stat {
            state: f.first()?.chars().next()?,
            ppid: num(1) as u32,
            ticks: num(11) + num(12),
            nice: f.get(16).and_then(|v| v.parse().ok()).unwrap_or(0),
            threads: num(17) as u32,
            start_time: num(19),
            efficiency: num(38) == crate::actions::SCHED_IDLE as u64,
        },
    ))
}

/// `/proc/[pid]/statm`: (resident, shared) in pages.
pub fn parse_statm(text: &str) -> (u64, u64) {
    let mut f = text.split_ascii_whitespace().skip(1).map(|v| v.parse().unwrap_or(0));
    (f.next().unwrap_or(0), f.next().unwrap_or(0))
}

/// `/proc/[pid]/io`: bytes actually read from / written to storage.
pub fn parse_io(text: &str) -> (u64, u64) {
    let (mut read, mut write) = (0, 0);
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("read_bytes: ") {
            read = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("write_bytes: ") {
            write = v.trim().parse().unwrap_or(0);
        }
    }
    (read, write)
}

/// Display name: argv[0]'s file name (comm is cut at 15 bytes), else comm.
pub fn display_name(cmdline: &str, comm: &str) -> String {
    let argv0 = cmdline.split('\0').next().unwrap_or("");
    let base = argv0.rsplit('/').next().unwrap_or("");
    // Some programs rewrite argv[0] into a whole command line ("sshd: user@pts/0").
    if base.is_empty() || base.contains(' ') || base.len() < comm.len() {
        comm.to_owned()
    } else {
        base.to_owned()
    }
}

/// What we remember about a PID between ticks.
struct Entry {
    start_time: u64,
    name: Arc<str>,
    cmdline: Arc<str>,
    user: Arc<str>,
    group: Group,
    ticks: u64,
    io: Option<(u64, u64)>,
    /// `/proc/[pid]/io` is only readable for our own processes; don't retry once denied.
    io_denied: bool,
    seen: bool,
}

/// Samples every process. Static facts (name, cmdline, owner, app) are read once
/// per process and reused until the PID is recycled.
pub struct ProcessSampler {
    /// Seconds covered by the last `sample()` (its rates are averages over this span).
    pub last_interval: f64,
    buf: String,
    path: String,
    last: Instant,
    clk_tck: f64,
    page_size: u64,
    cpus: usize,
    my_uid: u32,
    entries: HashMap<u32, Entry>,
    users: HashMap<u32, Arc<str>>,
    apps: Registry,
}

impl Default for ProcessSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessSampler {
    pub fn new() -> Self {
        use nix::unistd::{SysconfVar, sysconf};
        let conf = |v, default| sysconf(v).ok().flatten().map_or(default, |n| n as u64);
        Self {
            last_interval: 0.0,
            buf: String::with_capacity(4096),
            path: String::with_capacity(32),
            last: Instant::now(),
            clk_tck: conf(SysconfVar::CLK_TCK, 100) as f64,
            page_size: conf(SysconfVar::PAGE_SIZE, 4096),
            cpus: std::thread::available_parallelism().map_or(1, |n| n.get()),
            my_uid: nix::unistd::getuid().as_raw(),
            entries: HashMap::new(),
            users: HashMap::new(),
            apps: Registry::load(),
        }
    }

    pub fn sample(&mut self) -> Vec<Process> {
        let now = Instant::now();
        let secs = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.last_interval = secs;
        let capacity = secs * self.clk_tck * self.cpus as f64;

        let mut out = Vec::with_capacity(self.entries.len() + 16);
        let Ok(dir) = fs::read_dir("/proc") else { return out };
        for entry in dir.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            if let Some(p) = self.sample_pid(pid, secs, capacity) {
                out.push(p);
            }
        }
        self.entries.retain(|_, e| std::mem::take(&mut e.seen));
        out
    }

    fn sample_pid(&mut self, pid: u32, secs: f64, capacity: f64) -> Option<Process> {
        // The process may exit at any point; any failed read just drops it this tick.
        if !self.read(pid, "stat") {
            return None;
        }
        let (comm, stat) = parse_stat(&self.buf)?;
        let comm = comm.to_owned();

        let fresh = self.entries.get(&pid).is_none_or(|e| e.start_time != stat.start_time);
        if fresh {
            let entry = self.describe(pid, &comm, &stat);
            self.entries.insert(pid, entry);
        }

        let (kernel, io_denied) = self.entries.get(&pid).map(|e| (e.group == Group::Kernel, e.io_denied))?;
        // Kernel threads have no user memory or I/O accounting worth reading.
        let (resident, shared) = if !kernel && self.read(pid, "statm") { parse_statm(&self.buf) } else { (0, 0) };
        let io = if kernel || io_denied {
            None
        } else if self.read(pid, "io") {
            Some(parse_io(&self.buf))
        } else {
            self.entries.get_mut(&pid)?.io_denied = true;
            None
        };

        let e = self.entries.get_mut(&pid)?;
        e.seen = true;
        let cpu = if fresh || capacity <= 0.0 {
            0.0
        } else {
            (stat.ticks.saturating_sub(e.ticks) as f64 / capacity).clamp(0.0, 1.0) as f32
        };
        let rate = |now: u64, before: u64| if secs > 0.0 { now.saturating_sub(before) as f64 / secs } else { 0.0 };
        let (read_bps, write_bps) = match (io, e.io, fresh) {
            (Some((r, w)), Some((pr, pw)), false) => (rate(r, pr), rate(w, pw)),
            _ => (0.0, 0.0),
        };
        e.ticks = stat.ticks;
        e.io = io;

        Some(Process {
            pid,
            ppid: stat.ppid,
            name: e.name.clone(),
            cmdline: e.cmdline.clone(),
            user: e.user.clone(),
            state: stat.state,
            cpu,
            memory: resident.saturating_sub(shared) * self.page_size,
            read_bps,
            write_bps,
            threads: stat.threads,
            nice: stat.nice,
            efficiency: stat.efficiency,
            gpu: 0.0,
            group: e.group.clone(),
        })
    }

    /// Reads the static facts of a newly seen process.
    fn describe(&mut self, pid: u32, comm: &str, stat: &Stat) -> Entry {
        let kernel = pid == 2 || stat.ppid == 2;
        let cmdline = if !kernel && self.read(pid, "cmdline") { self.buf.trim_end_matches('\0').to_owned() } else { String::new() };
        let name = display_name(&cmdline, comm);
        let uid = fs::metadata(format!("/proc/{pid}")).map_or(0, |m| m.uid());
        let group = if kernel {
            Group::Kernel
        } else if self.read(pid, "cgroup")
            && let Some(app) = self.buf.lines().find_map(|l| l.strip_prefix("0::")).map(str::to_owned).and_then(|cg| self.apps.resolve(&cg))
        {
            Group::App(app)
        } else if uid == self.my_uid {
            Group::Background
        } else {
            Group::System
        };
        Entry {
            start_time: stat.start_time,
            name: Arc::from(name),
            cmdline: Arc::from(cmdline.replace('\0', " ")),
            user: self.user_name(uid),
            group,
            ticks: stat.ticks,
            io: None,
            io_denied: false,
            seen: false,
        }
    }

    fn user_name(&mut self, uid: u32) -> Arc<str> {
        self.users
            .entry(uid)
            .or_insert_with(|| {
                let name = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
                    .ok()
                    .flatten()
                    .map_or_else(|| uid.to_string(), |u| u.name);
                Arc::from(name)
            })
            .clone()
    }

    fn read(&mut self, pid: u32, file: &str) -> bool {
        self.path.clear();
        let _ = write!(self.path, "/proc/{pid}/{file}");
        self.buf.clear();
        fs::File::open(&self.path).and_then(|mut f| f.read_to_string(&mut self.buf)).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stat_with_awkward_comm() {
        let text = "1234 (Web Content (x)) S 1000 1234 1000 0 -1 4194560 100 0 0 0 250 50 0 0 20 5 31 0 98765 1000000 2000 18446744073709551615";
        let (comm, s) = parse_stat(text).unwrap();
        assert_eq!(comm, "Web Content (x)");
        assert_eq!(s, Stat { state: 'S', ppid: 1000, ticks: 300, nice: 5, threads: 31, start_time: 98765, efficiency: false });
    }

    #[test]
    fn parses_statm_and_io() {
        assert_eq!(parse_statm("50000 1200 300 10 0 900 0\n"), (1200, 300));
        let io = "rchar: 1\nwchar: 2\nsyscr: 3\nsyscw: 4\nread_bytes: 4096\nwrite_bytes: 8192\ncancelled_write_bytes: 0\n";
        assert_eq!(parse_io(io), (4096, 8192));
    }

    #[test]
    fn display_names() {
        assert_eq!(display_name("/usr/bin/gnome-text-editor\0--new-window", "gnome-text-edi"), "gnome-text-editor");
        assert_eq!(display_name("sshd: anuz@pts/0", "sshd"), "sshd");
        assert_eq!(display_name("", "kworker/0:1"), "kworker/0:1");
    }
}
