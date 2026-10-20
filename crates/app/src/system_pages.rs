//! Rows for the Services, Startup apps and Users pages.

use std::collections::HashMap;

use taskmonitor_core::app_usage::AppUsageLog;
use taskmonitor_core::services::Service;
use taskmonitor_core::startup::StartupEntry;
use taskmonitor_core::users::Session;
use taskmonitor_core::{Process, Snapshot};

use crate::processes::{initials, mem_text, tone};
use crate::{HistoryRow, ServiceRow, StartupRow, UserRow};

pub fn service_rows(services: &[Service], filter: &str) -> (Vec<ServiceRow>, String) {
    let filter = filter.to_lowercase();
    let running = services.iter().filter(|s| s.sub == "running").count();
    let rows = services
        .iter()
        .filter(|s| {
            filter.is_empty() || s.name.to_lowercase().contains(&filter) || s.description.to_lowercase().contains(&filter)
        })
        .map(|s| {
            let (status, tone) = match (s.active.as_str(), s.sub.as_str()) {
                ("failed", _) => ("Failed", 2),
                ("activating" | "reloading", _) => ("Starting", 3),
                ("deactivating", _) => ("Stopping", 3),
                ("active", "running") => ("Running", 1),
                ("active", _) => ("Exited", 0),
                _ => ("Stopped", 0),
            };
            let mut startup = s.enabled.replace('-', " ");
            if let Some(first) = startup.get(..1) {
                startup = first.to_uppercase() + &startup[1..];
            }
            ServiceRow {
                name: s.name.trim_end_matches(".service").into(),
                description: s.description.as_str().into(),
                status: status.into(),
                tone,
                startup: if startup.is_empty() { "—".into() } else { startup.into() },
                pid: if s.pid > 0 { s.pid.to_string().into() } else { "".into() },
                can_enable: matches!(s.enabled.as_str(), "enabled" | "disabled"),
            }
        })
        .collect();
    (rows, format!("{running} running of {}", services.len()))
}

pub fn startup_rows(entries: &[StartupEntry]) -> Vec<StartupRow> {
    entries
        .iter()
        .map(|e| StartupRow {
            id: e.id.as_str().into(),
            name: e.name.as_str().into(),
            command: e.command.as_str().into(),
            initials: initials(&e.name).into(),
            tone: tone(&e.name),
            enabled: e.enabled,
            source: if e.system { "System" } else { "Yours" }.into(),
        })
        .collect()
}

/// One row per session, with CPU and memory summed over that user's processes.
pub fn user_rows(sessions: &[Session], procs: &[Process], snap: &Snapshot) -> Vec<UserRow> {
    let mut per_user: HashMap<&str, (f32, u64, usize)> = HashMap::new();
    for p in procs {
        let e = per_user.entry(&p.user).or_default();
        e.0 += p.cpu;
        e.1 += p.memory;
        e.2 += 1;
    }
    let mem_total = snap.memory.total.max(1) as f64;
    sessions
        .iter()
        .map(|s| {
            let (cpu, mem, count) = per_user.get(s.user.as_str()).copied().unwrap_or_default();
            let kind = match s.kind.as_str() {
                "wayland" => "Wayland",
                "x11" => "X11",
                "tty" => "Terminal",
                other => other,
            };
            let place = if s.remote { "Remote".to_owned() } else if s.seat.is_empty() { "No seat".to_owned() } else { s.seat.clone() };
            let status = match s.state.as_str() {
                "active" => "Active",
                "online" => "Signed in (not in front)",
                "closing" => "Signing out",
                other => other,
            };
            UserRow {
                key: s.id.as_str().into(),
                user: s.user.as_str().into(),
                initials: initials(&s.user).into(),
                tone: 3,
                detail: format!("Session {} · {kind} · {place}", s.id).into(),
                status: status.into(),
                processes: count.to_string().into(),
                cpu: format!("{:.1}%", cpu * 100.0).into(),
                cpu_heat: (cpu * 4.0).clamp(0.0, 1.0),
                mem: mem_text(mem).into(),
                mem_heat: (mem as f64 / mem_total * 4.0).clamp(0.0, 1.0) as f32,
            }
        })
        .collect()
}

/// Apps by CPU time, most first.
pub fn history_rows(log: &AppUsageLog) -> Vec<HistoryRow> {
    let mut apps: Vec<_> = log.apps.values().collect();
    apps.sort_by(|a, b| b.cpu_secs.total_cmp(&a.cpu_secs));
    let max = apps.first().map_or(1.0, |a| a.cpu_secs.max(1.0));
    let duration = |secs: f64| {
        let s = secs as u64;
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    };
    apps.into_iter()
        .map(|a| HistoryRow {
            name: a.name.as_str().into(),
            initials: initials(&a.name).into(),
            tone: tone(&a.name),
            cpu: duration(a.cpu_secs).into(),
            cpu_heat: (a.cpu_secs / max) as f32,
            read: mem_text(a.read_bytes as u64).into(),
            write: mem_text(a.write_bytes as u64).into(),
            gpu: duration(a.gpu_secs).into(),
        })
        .collect()
}

/// "in the last 3 hours", relative to when tracking started.
pub fn since_text(since: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let ago = now.saturating_sub(since);
    let (n, unit) = match ago {
        0..3600 => (ago / 60, "minute"),
        3600..86_400 => (ago / 3600, "hour"),
        _ => (ago / 86_400, "day"),
    };
    match n.max(1) {
        1 => format!("in the last {unit}"),
        n => format!("in the last {n} {unit}s"),
    }
}
