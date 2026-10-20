//! systemd services over D-Bus. Starting, stopping and enabling go through systemd
//! itself, which asks polkit (and shows a password prompt) when needed.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use zbus::blocking::{Connection, Proxy};
use zbus::proxy::MethodFlags;
use zbus::zvariant::OwnedObjectPath;

const DEST: &str = "org.freedesktop.systemd1";
const PATH: &str = "/org/freedesktop/systemd1";
const MANAGER: &str = "org.freedesktop.systemd1.Manager";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bus {
    /// System services (root's systemd).
    System,
    /// The current user's `systemd --user` services.
    User,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Service {
    pub name: String,
    pub description: String,
    /// active, inactive, failed, activating, deactivating…
    pub active: String,
    /// running, exited, dead…
    pub sub: String,
    /// enabled, disabled, static, masked… ("" if no unit file).
    pub enabled: String,
    pub pid: u32,
}

type UnitRow = (String, String, String, String, String, String, OwnedObjectPath, u32, String, OwnedObjectPath);

pub struct Services {
    system: Option<Connection>,
    user: Option<Connection>,
    /// Unit-file states (enabled/disabled/…) per bus. systemd scans the disk to answer
    /// (~350 ms for the system bus), so they're cached and refreshed at most once a
    /// minute, or right after an enable/disable.
    file_states: std::cell::RefCell<HashMap<Bus, FileStates>>,
}

/// When fetched, and unit name -> state.
type FileStates = (Instant, HashMap<String, String>);
const FILE_STATES_TTL: Duration = Duration::from_secs(60);

impl Default for Services {
    fn default() -> Self {
        Self::new()
    }
}

impl Services {
    pub fn new() -> Self {
        Self { system: Connection::system().ok(), user: Connection::session().ok(), file_states: Default::default() }
    }

    fn conn(&self, bus: Bus) -> zbus::Result<&Connection> {
        match bus {
            Bus::System => self.system.as_ref(),
            Bus::User => self.user.as_ref(),
        }
        .ok_or_else(|| zbus::Error::Failure("D-Bus is not available".into()))
    }

    fn manager(&self, bus: Bus) -> zbus::Result<Proxy<'_>> {
        Proxy::new(self.conn(bus)?, DEST, PATH, MANAGER)
    }

    /// Every service: loaded ones with their state, plus installed but never-loaded ones.
    pub fn list(&self, bus: Bus) -> zbus::Result<Vec<Service>> {
        let manager = self.manager(bus)?;
        let patterns = vec!["*.service"];
        let units: Vec<UnitRow> = manager.call("ListUnitsByPatterns", &(Vec::<&str>::new(), &patterns))?;
        let stale = self.file_states.borrow().get(&bus).is_none_or(|(t, _)| t.elapsed() > FILE_STATES_TTL);
        if stale {
            let files: Vec<(String, String)> =
                manager.call("ListUnitFilesByPatterns", &(Vec::<&str>::new(), &patterns))?;
            let states = files
                .into_iter()
                .filter_map(|(path, state)| Some((path.rsplit('/').next()?.to_owned(), state)))
                .collect();
            self.file_states.borrow_mut().insert(bus, (Instant::now(), states));
        }
        let cache = self.file_states.borrow();
        let file_state = &cache[&bus].1;

        let pids = service_pids();
        let mut out: Vec<Service> = units
            .into_iter()
            // Template instances like getty@tty1 are listed; bare templates aren't runnable.
            .filter(|u| u.2 != "not-found")
            .map(|(name, description, _load, active, sub, ..)| Service {
                enabled: file_state.get(&name).cloned().unwrap_or_default(),
                pid: pids.get(&name).copied().unwrap_or(0),
                name,
                description,
                active,
                sub,
            })
            .collect();
        for (name, state) in file_state {
            if !name.ends_with("@.service") && !out.iter().any(|s| &s.name == name) {
                out.push(Service {
                    name: name.clone(),
                    active: "inactive".into(),
                    sub: "dead".into(),
                    enabled: state.clone(),
                    ..Default::default()
                });
            }
        }
        out.sort_by_key(|s| s.name.to_lowercase());
        Ok(out)
    }

    /// start / stop / restart / enable / disable. May show a polkit prompt.
    pub fn control(&self, bus: Bus, verb: &str, unit: &str) -> zbus::Result<()> {
        let manager = self.manager(bus)?;
        let flags = MethodFlags::AllowInteractiveAuth.into();
        match verb {
            "start" | "stop" | "restart" => {
                let method = match verb {
                    "start" => "StartUnit",
                    "stop" => "StopUnit",
                    _ => "RestartUnit",
                };
                manager.call_with_flags::<_, _, OwnedObjectPath>(method, flags, &(unit, "replace"))?;
            }
            "enable" => {
                manager.call_with_flags::<_, _, (bool, Vec<(String, String, String)>)>(
                    "EnableUnitFiles",
                    flags,
                    &(vec![unit], false, false),
                )?;
                manager.call_with_flags::<_, _, ()>("Reload", flags, &())?;
                self.file_states.borrow_mut().remove(&bus);
            }
            "disable" => {
                manager.call_with_flags::<_, _, Vec<(String, String, String)>>("DisableUnitFiles", flags, &(vec![unit], false))?;
                manager.call_with_flags::<_, _, ()>("Reload", flags, &())?;
                self.file_states.borrow_mut().remove(&bus);
            }
            _ => return Err(zbus::Error::Failure(format!("unknown action {verb}"))),
        }
        Ok(())
    }
}

/// Lowest PID in each service's cgroup, from `/proc/*/cgroup` (one file per process),
/// instead of one D-Bus call per running service (which took ~600 ms for 90 services).
fn service_pids() -> HashMap<String, u32> {
    let mut out: HashMap<String, u32> = HashMap::new();
    let Ok(dir) = std::fs::read_dir("/proc") else { return out };
    for entry in dir.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/cgroup")) else { continue };
        let Some(unit) = text.lines().find_map(|l| l.strip_prefix("0::")).and_then(|p| p.rsplit('/').next()) else {
            continue;
        };
        if unit.ends_with(".service") {
            let e = out.entry(unit.to_owned()).or_insert(pid);
            *e = (*e).min(pid);
        }
    }
    out
}
