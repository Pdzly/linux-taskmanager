//! App history: cumulative resource use per desktop app, kept across restarts in a
//! small tab-separated file (no database needed for a few dozen rows).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::process::{Group, Process};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AppUsage {
    pub id: String,
    pub name: String,
    /// CPU time in seconds, summed over all of the app's processes.
    pub cpu_secs: f64,
    pub read_bytes: f64,
    pub write_bytes: f64,
    /// GPU busy time in seconds (busiest engine).
    pub gpu_secs: f64,
}

#[derive(Debug, PartialEq)]
pub struct AppUsageLog {
    /// Unix time tracking started.
    pub since: u64,
    pub apps: HashMap<String, AppUsage>,
}

impl Default for AppUsageLog {
    fn default() -> Self {
        Self { since: now(), apps: HashMap::new() }
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub fn default_path() -> PathBuf {
    let data = std::env::var("XDG_DATA_HOME")
        .unwrap_or_else(|_| format!("{}/.local/share", std::env::var("HOME").unwrap_or_default()));
    PathBuf::from(data).join("taskmonitor/app-history.tsv")
}

impl AppUsageLog {
    /// Adds one sample: each process's rates times the seconds they covered.
    /// `cpus` converts whole-machine CPU share into CPU-seconds.
    pub fn record(&mut self, procs: &[Process], secs: f64, cpus: usize) {
        if secs <= 0.0 {
            return;
        }
        for p in procs {
            let Group::App(app) = &p.group else { continue };
            let entry = self.apps.entry(app.id.to_string()).or_insert_with(|| AppUsage {
                id: app.id.to_string(),
                name: app.name.to_string(),
                ..Default::default()
            });
            entry.cpu_secs += p.cpu as f64 * cpus as f64 * secs;
            entry.read_bytes += p.read_bps * secs;
            entry.write_bytes += p.write_bps * secs;
            entry.gpu_secs += p.gpu as f64 * secs;
        }
    }

    pub fn load(path: &std::path::Path) -> Self {
        fs::read_to_string(path).ok().and_then(|t| Self::parse(&t)).unwrap_or_default()
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let since = lines.next()?.strip_prefix("# since ")?.trim().parse().ok()?;
        let mut apps = HashMap::new();
        for line in lines {
            let f: Vec<&str> = line.split('\t').collect();
            if let [id, name, cpu, read, write, gpu] = f[..] {
                let n = |v: &str| v.parse().unwrap_or(0.0);
                apps.insert(
                    id.to_owned(),
                    AppUsage {
                        id: id.to_owned(),
                        name: name.to_owned(),
                        cpu_secs: n(cpu),
                        read_bytes: n(read),
                        write_bytes: n(write),
                        gpu_secs: n(gpu),
                    },
                );
            }
        }
        Some(Self { since, apps })
    }

    pub fn to_text(&self) -> String {
        let mut out = format!("# since {}\n", self.since);
        let mut apps: Vec<_> = self.apps.values().collect();
        apps.sort_by(|a, b| a.id.cmp(&b.id));
        for a in apps {
            // Tabs/newlines can't appear in desktop ids, but names come from .desktop files.
            let name = a.name.replace(['\t', '\n'], " ");
            out += &format!(
                "{}\t{name}\t{:.1}\t{:.0}\t{:.0}\t{:.1}\n",
                a.id, a.cpu_secs, a.read_bytes, a.write_bytes, a.gpu_secs
            );
        }
        out
    }

    pub fn save(&self, path: &std::path::Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, self.to_text())?;
        fs::rename(tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::App;

    fn proc(cpu: f32, group: Group) -> Process {
        Process {
            pid: 1,
            ppid: 0,
            name: "x".into(),
            cmdline: "".into(),
            user: "u".into(),
            state: 'S',
            cpu,
            memory: 0,
            read_bps: 1000.0,
            write_bps: 0.0,
            io_total: None,
            threads: 1,
            nice: 0,
            efficiency: false,
            gpu: 0.5,
            icon: None,
            group,
        }
    }

    #[test]
    fn records_apps_only_and_round_trips() {
        let app = Group::App(App { id: "org.web".into(), name: "Web\tBrowser".into(), icon: None });
        let mut log = AppUsageLog { since: 42, apps: HashMap::new() };
        // 25% of an 8-CPU machine for 2 s = 4 CPU-seconds.
        log.record(&[proc(0.25, app.clone()), proc(0.9, Group::Background)], 2.0, 8);
        log.record(&[proc(0.25, app)], 2.0, 8);
        let web = &log.apps["org.web"];
        assert_eq!(log.apps.len(), 1);
        assert!((web.cpu_secs - 8.0).abs() < 1e-6);
        assert!((web.read_bytes - 4000.0).abs() < 1e-6);
        assert!((web.gpu_secs - 2.0).abs() < 1e-6);

        let back = AppUsageLog::parse(&log.to_text()).unwrap();
        assert_eq!(back.since, 42);
        assert_eq!(back.apps["org.web"].name, "Web Browser");
        assert!((back.apps["org.web"].cpu_secs - 8.0).abs() < 0.1);
    }
}
