//! Builds the Processes and Details tables from sampled processes.
//! Sort, filter and expanded groups live here (on the sampler thread), so changing
//! them can rebuild rows instantly from the last sample.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};

use slint::SharedString;
use taskmonitor_core::{Group, Process, Snapshot};

use crate::bridge::sync;
use crate::{DetailRow, MainWindow, Page, ProcRow, ProcTotals};

pub struct View {
    pub sort: SharedString,
    pub ascending: bool,
    pub filter: String,
    pub expanded: HashSet<SharedString>,
}

impl Default for View {
    fn default() -> Self {
        Self { sort: "name".into(), ascending: true, filter: String::new(), expanded: HashSet::new() }
    }
}

impl View {
    /// Clicking the sorted column flips direction; a new column starts with its natural one
    /// (names A→Z, numbers largest first).
    pub fn set_sort(&mut self, column: SharedString) {
        if column == self.sort {
            self.ascending = !self.ascending;
        } else {
            self.ascending = matches!(column.as_str(), "name" | "pid" | "user" | "status" | "cmd");
            self.sort = column;
        }
    }

    pub fn toggle(&mut self, key: SharedString) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
    }

    pub fn build(&self, page: Page, procs: &[Process], snap: &Snapshot, gpu_total: f32) -> Rows {
        let disk_bps: f64 = snap.disks.iter().map(|d| d.rates.read_bps + d.rates.write_bps).sum();
        let totals = ProcTotals {
            count: format!("{} processes", procs.len()).into(),
            cpu: format!("{:.0}%", snap.cpu_usage * 100.0).into(),
            mem: format!("{:.0}%", snap.memory.used() as f64 * 100.0 / snap.memory.total.max(1) as f64).into(),
            disk: format!("{} MB/s", mb(disk_bps)).into(),
            gpu: format!("{:.0}%", gpu_total * 100.0).into(),
        };
        let filter = self.filter.to_lowercase();
        let matches = |p: &Process| {
            filter.is_empty()
                || p.name.to_lowercase().contains(&filter)
                || p.pid.to_string() == filter
                || p.cmdline.to_lowercase().contains(&filter)
        };
        let ctx = Ctx { mem_total: snap.memory.total.max(1) as f64 };
        let mut rows = Rows { totals, sort: self.sort.clone(), ascending: self.ascending, ..Default::default() };
        match page {
            Page::Details => {
                let mut list: Vec<&Process> = procs.iter().filter(|p| matches(p)).collect();
                list.sort_by(|a, b| self.order(cmp_procs(a, b, &self.sort)));
                rows.details = list.into_iter().map(detail_row).collect();
            }
            Page::Processes => rows.procs = self.process_rows(procs, &matches, &ctx),
            _ => {}
        }
        rows
    }

    fn order(&self, o: Ordering) -> Ordering {
        if self.ascending { o } else { o.reverse() }
    }

    fn process_rows(&self, procs: &[Process], matches: &dyn Fn(&Process) -> bool, ctx: &Ctx) -> Vec<ProcRow> {
        // Section -> group key -> members. Apps group by app; others by process name.
        let mut sections: [BTreeMap<String, Vec<&Process>>; 3] = Default::default();
        for p in procs {
            let (section, key) = match &p.group {
                Group::App(app) => (0, format!("app:{}", app.id)),
                Group::Background => (1, format!("bg:{}", p.name)),
                Group::System => (2, format!("sys:{}", p.name)),
                Group::Kernel => continue,
            };
            sections[section].entry(key).or_default().push(p);
        }

        let mut rows = Vec::new();
        for (title, groups) in ["Apps", "Background processes", "System processes"].into_iter().zip(sections) {
            let mut items: Vec<Item> = groups
                .into_iter()
                .filter_map(|(key, members)| {
                    let name = match &members[0].group {
                        Group::App(app) => app.name.to_string(),
                        _ => members[0].name.to_string(),
                    };
                    let group_hit = !self.filter.is_empty() && name.to_lowercase().contains(&self.filter.to_lowercase());
                    let members: Vec<&Process> = members.into_iter().filter(|p| group_hit || matches(p)).collect();
                    (!members.is_empty()).then(|| Item::new(key, name, members))
                })
                .collect();
            if items.is_empty() {
                continue;
            }
            items.sort_by(|a, b| self.order(a.cmp(b, &self.sort)));
            rows.push(ProcRow {
                kind: 0,
                key: format!("section:{title}").into(),
                name: format!("{title} ({})", items.len()).into(),
                ..Default::default()
            });
            for item in items {
                let expanded = self.expanded.contains(item.key.as_str());
                let single = item.members.len() == 1;
                rows.push(ProcRow {
                    kind: 1,
                    key: if single { format!("pid:{}", item.members[0].pid).into() } else { item.key.as_str().into() },
                    count: if single { SharedString::new() } else { format!("({})", item.members.len()).into() },
                    expandable: !single,
                    expanded,
                    status: status_short(item.members.iter().map(|p| (p.state, p.efficiency))).into(),
                    suspended: item.members.iter().all(|p| p.state == 'T'),
                    efficiency: item.members.iter().all(|p| p.efficiency),
                    ..item_row(&item.name, item.cpu, item.mem, item.disk, item.gpu, ctx)
                });
                if expanded && !single {
                    let mut members = item.members;
                    members.sort_by(|a, b| self.order(cmp_procs(a, b, &self.sort)));
                    for p in members {
                        rows.push(ProcRow {
                            kind: 2,
                            key: format!("pid:{}", p.pid).into(),
                            count: format!("{}", p.pid).into(),
                            status: status_short([(p.state, p.efficiency)]).into(),
                            suspended: p.state == 'T',
                            efficiency: p.efficiency,
                            ..item_row(&p.name, p.cpu, p.memory, p.read_bps + p.write_bps, p.gpu, ctx)
                        });
                    }
                }
            }
        }
        rows
    }
}

struct Ctx {
    mem_total: f64,
}

/// A group of processes shown as one row.
struct Item<'a> {
    key: String,
    name: String,
    members: Vec<&'a Process>,
    cpu: f32,
    mem: u64,
    disk: f64,
    gpu: f32,
}

impl<'a> Item<'a> {
    fn new(key: String, name: String, members: Vec<&'a Process>) -> Self {
        Self {
            cpu: members.iter().map(|p| p.cpu).sum(),
            mem: members.iter().map(|p| p.memory).sum(),
            disk: members.iter().map(|p| p.read_bps + p.write_bps).sum(),
            gpu: members.iter().map(|p| p.gpu).sum::<f32>().min(1.0),
            key,
            name,
            members,
        }
    }

    fn cmp(&self, other: &Self, column: &str) -> Ordering {
        match column {
            "cpu" => self.cpu.total_cmp(&other.cpu),
            "mem" => self.mem.cmp(&other.mem),
            "disk" => self.disk.total_cmp(&other.disk),
            "gpu" => self.gpu.total_cmp(&other.gpu),
            _ => cmp_names(&self.name, &other.name),
        }
    }
}

pub fn apply(ui: &MainWindow, rows: Rows) {
    ui.set_proc_totals(rows.totals);
    ui.set_sort_column(rows.sort);
    ui.set_sort_ascending(rows.ascending);
    sync(ui.get_proc_rows(), rows.procs, |m| ui.set_proc_rows(m));
    sync(ui.get_detail_rows(), rows.details, |m| ui.set_detail_rows(m));
}

#[derive(Default)]
pub struct Rows {
    pub procs: Vec<ProcRow>,
    pub details: Vec<DetailRow>,
    pub totals: ProcTotals,
    pub sort: SharedString,
    pub ascending: bool,
}

fn item_row(name: &str, cpu: f32, mem: u64, disk_bps: f64, gpu: f32, ctx: &Ctx) -> ProcRow {
    ProcRow {
        name: name.into(),
        initials: initials(name).into(),
        tone: tone(name),
        cpu: format!("{:.1}%", cpu * 100.0).into(),
        // A process using a quarter of the machine is "hot".
        cpu_heat: (cpu * 4.0).clamp(0.0, 1.0),
        mem: mem_text(mem).into(),
        mem_heat: (mem as f64 / ctx.mem_total * 8.0).clamp(0.0, 1.0) as f32,
        disk: format!("{} MB/s", mb(disk_bps)).into(),
        disk_heat: (disk_bps / (50.0 * 1024.0 * 1024.0)).clamp(0.0, 1.0) as f32,
        gpu: format!("{:.0}%", gpu * 100.0).into(),
        gpu_heat: (gpu * 2.0).clamp(0.0, 1.0),
        ..Default::default()
    }
}

fn detail_row(p: &Process) -> DetailRow {
    DetailRow {
        key: format!("pid:{}", p.pid).into(),
        name: p.name.as_ref().into(),
        pid: p.pid.to_string().into(),
        status: status_long(p.state).into(),
        user: p.user.as_ref().into(),
        cpu: format!("{:.1}", p.cpu * 100.0).into(),
        mem: mem_text(p.memory).into(),
        threads: p.threads.to_string().into(),
        nice: p.nice.to_string().into(),
        cmd: p.cmdline.as_ref().into(),
        suspended: p.state == 'T',
        efficiency: p.efficiency,
    }
}

fn cmp_procs(a: &Process, b: &Process, column: &str) -> Ordering {
    match column {
        "pid" => a.pid.cmp(&b.pid),
        "cpu" => a.cpu.total_cmp(&b.cpu),
        "mem" => a.memory.cmp(&b.memory),
        "disk" => (a.read_bps + a.write_bps).total_cmp(&(b.read_bps + b.write_bps)),
        "gpu" => a.gpu.total_cmp(&b.gpu),
        "threads" => a.threads.cmp(&b.threads),
        "nice" => a.nice.cmp(&b.nice),
        "user" => a.user.cmp(&b.user),
        "status" => status_long(a.state).cmp(status_long(b.state)),
        "cmd" => a.cmdline.cmp(&b.cmdline),
        _ => cmp_names(&a.name, &b.name),
    }
    .then(a.pid.cmp(&b.pid))
}

fn cmp_names(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

/// Only states worth calling out, as Windows leaves Status blank for normal processes.
/// A group shows a status only when every member has it (one unreaped child
/// shouldn't label a whole browser "Zombie").
fn status_short(states: impl IntoIterator<Item = (char, bool)>) -> &'static str {
    let label = |(s, efficiency): (char, bool)| match s {
        'T' | 't' => "Suspended",
        'Z' => "Zombie",
        _ if efficiency => "Efficiency mode",
        'D' => "Waiting on I/O",
        _ => "",
    };
    let mut states = states.into_iter().map(label);
    let first = states.next().unwrap_or("");
    if states.all(|s| s == first) { first } else { "" }
}

fn status_long(state: char) -> &'static str {
    match state {
        'R' => "Running",
        'S' => "Sleeping",
        'D' => "Waiting on I/O",
        'Z' => "Zombie",
        'T' => "Suspended",
        't' => "Traced",
        'I' => "Idle",
        'X' => "Dead",
        _ => "Unknown",
    }
}

fn mb(bps: f64) -> String {
    let v = bps / (1024.0 * 1024.0);
    if v < 0.05 { "0".into() } else { format!("{v:.1}") }
}

fn mem_text(bytes: u64) -> String {
    let mib = bytes as f64 / (1024.0 * 1024.0);
    if mib >= 1000.0 { format!("{:.1} GB", mib / 1024.0) } else { format!("{mib:.1} MB") }
}

/// "gnome-text-editor" -> "GT", "firefox" -> "Fi".
fn initials(name: &str) -> String {
    let mut words = name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty());
    let first = words.next().unwrap_or("?");
    let mut out: String = first.chars().take(1).flat_map(char::to_uppercase).collect();
    match words.next() {
        Some(second) => out.extend(second.chars().take(1).flat_map(char::to_uppercase)),
        None => out.extend(first.chars().skip(1).take(1)),
    }
    out
}

/// Stable badge colour per name.
fn tone(name: &str) -> i32 {
    (name.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) % 5) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_and_status() {
        assert_eq!(initials("gnome-text-editor"), "GT");
        assert_eq!(initials("firefox"), "Fi");
        let s = |c| (c, false);
        assert_eq!(status_short([s('T'), s('T')]), "Suspended");
        assert_eq!(status_short([s('S'), s('Z')]), "");
        assert_eq!(status_short([s('Z')]), "Zombie");
        assert_eq!(status_short([s('S'), s('R')]), "");
        assert_eq!(status_short([('S', true), ('R', true)]), "Efficiency mode");
    }

    fn proc(pid: u32, name: &str, cpu: f32, group: Group) -> Process {
        Process {
            pid,
            ppid: 1,
            name: name.into(),
            cmdline: format!("/usr/bin/{name}").into(),
            user: "me".into(),
            state: 'S',
            cpu,
            memory: 1 << 20,
            read_bps: 0.0,
            write_bps: 0.0,
            threads: 1,
            nice: 0,
            efficiency: false,
            gpu: 0.0,
            group,
        }
    }

    fn names(rows: &[ProcRow]) -> Vec<String> {
        rows.iter().map(|r| format!("{}{}", "  ".repeat(r.kind as usize), r.name)).collect()
    }

    #[test]
    fn groups_filters_and_expands() {
        let app = Group::App(taskmonitor_core::apps::App { id: "org.web".into(), name: "Web".into() });
        let procs = vec![
            proc(10, "web", 0.10, app.clone()),
            proc(11, "web-gpu", 0.30, app),
            proc(20, "bash", 0.0, Group::Background),
            proc(1, "systemd", 0.0, Group::System),
            proc(2, "kthreadd", 0.0, Group::Kernel),
        ];
        let snap = Snapshot::default();
        let mut v = View::default();

        let rows = v.build(Page::Processes, &procs, &snap, 0.0).procs;
        assert_eq!(
            names(&rows),
            ["Apps (1)", "  Web", "Background processes (1)", "  bash", "System processes (1)", "  systemd"]
        );
        assert_eq!(rows[1].cpu, "40.0%", "group sums its members");
        assert!(rows[1].expandable);

        v.toggle("app:org.web".into());
        v.set_sort("cpu".into());
        let rows = v.build(Page::Processes, &procs, &snap, 0.0).procs;
        assert_eq!(names(&rows)[1..4], ["  Web", "    web-gpu", "    web"], "children sorted by CPU, largest first");

        v.filter = "bas".into();
        let rows = v.build(Page::Processes, &procs, &snap, 0.0).procs;
        assert_eq!(names(&rows), ["Background processes (1)", "  bash"]);

        // Details lists every process, kernel threads included.
        v.filter.clear();
        assert_eq!(v.build(Page::Details, &procs, &snap, 0.0).details.len(), 5);
    }

    #[test]
    fn sort_toggles_and_defaults() {
        let mut v = View::default();
        v.set_sort("cpu".into());
        assert!(!v.ascending, "numbers start largest first");
        v.set_sort("cpu".into());
        assert!(v.ascending);
        v.set_sort("name".into());
        assert!(v.ascending);
    }
}
