//! Maps running processes to desktop apps.
//!
//! GNOME, KDE and Flatpak start each app in its own systemd scope named after the
//! app's `.desktop` id (e.g. `app-gnome-org.gnome.Nautilus-4567.scope`), so a
//! process's cgroup path tells us which app it belongs to, without any window-system API.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub struct App {
    pub id: Arc<str>,
    pub name: Arc<str>,
}

/// Installed `.desktop` entries by id.
pub struct Registry {
    apps: HashMap<String, Arc<str>>,
    scanned: Instant,
}

/// Don't rescan the desktop directories more often than this when an unknown id shows up.
const RESCAN: Duration = Duration::from_secs(10);

impl Default for Registry {
    fn default() -> Self {
        Self::load()
    }
}

impl Registry {
    pub fn load() -> Self {
        let mut apps = HashMap::new();
        for dir in desktop_dirs() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "desktop") {
                    continue;
                }
                let Some(id) = path.file_stem().and_then(|s| s.to_str()) else { continue };
                // Earlier directories take precedence, as in the XDG spec.
                if apps.contains_key(id) {
                    continue;
                }
                if let Some(name) = std::fs::read_to_string(&path).ok().and_then(|t| parse_desktop_name(&t)) {
                    apps.insert(id.to_owned(), Arc::from(name));
                }
            }
        }
        Self { apps, scanned: Instant::now() }
    }

    /// The app a cgroup path belongs to, if it was launched as a desktop app.
    pub fn resolve(&mut self, cgroup: &str) -> Option<App> {
        let candidates = candidate_ids(cgroup);
        if candidates.is_empty() {
            return None;
        }
        if let Some(app) = self.lookup(&candidates) {
            return Some(app);
        }
        // Possibly installed after we started.
        if self.scanned.elapsed() > RESCAN {
            *self = Self::load();
            return self.lookup(&candidates);
        }
        None
    }

    fn lookup(&self, candidates: &[String]) -> Option<App> {
        for c in candidates {
            if let Some((id, name)) = self.apps.get_key_value(c.as_str()) {
                return Some(App { id: Arc::from(id.as_str()), name: name.clone() });
            }
        }
        // KDE scopes may use a short name ("dolphin") for "org.kde.dolphin".
        for c in candidates {
            let suffix = format!(".{c}");
            if let Some((id, name)) = self.apps.iter().find(|(id, _)| id.ends_with(&suffix)) {
                return Some(App { id: Arc::from(id.as_str()), name: name.clone() });
            }
        }
        None
    }
}

fn desktop_dirs() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let data_home = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{home}/.local/share"));
    let data_dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    std::iter::once(data_home.as_str())
        .chain(data_dirs.split(':'))
        .chain(["/var/lib/flatpak/exports/share", "/var/lib/snapd/desktop"])
        .filter(|d| !d.is_empty())
        .map(|d| PathBuf::from(d).join("applications"))
        .collect()
}

/// The unlocalized `Name=` of the `[Desktop Entry]` group.
pub fn parse_desktop_name(text: &str) -> Option<String> {
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry && let Some(name) = line.strip_prefix("Name=") {
            return Some(name.trim().to_owned());
        }
    }
    None
}

/// Desktop ids a cgroup path could stand for, most specific first.
pub fn candidate_ids(cgroup: &str) -> Vec<String> {
    for unit in cgroup.rsplit('/') {
        if let Some(rest) = unit.strip_prefix("snap.") {
            // snap.<snap>.<app>-<uuid>.scope -> desktop id <snap>_<app>
            let rest = strip_unit_suffix(rest);
            if let Some((snap, app)) = rest.split_once('.') {
                return vec![format!("{snap}_{app}"), snap.to_owned()];
            }
        }
        if let Some(rest) = unit.strip_prefix("app-") {
            let id = unescape(strip_unit_suffix(rest));
            let mut out = vec![id.clone()];
            // Drop a launcher prefix: app-gnome-<id>, app-flatpak-<id>, app-kde-<id>.
            if let Some((_, tail)) = id.split_once('-') {
                out.push(tail.to_owned());
            }
            return out;
        }
    }
    Vec::new()
}

/// "org.foo.Bar-1234.scope" -> "org.foo.Bar"; also drops "@instance" and ".slice".
fn strip_unit_suffix(unit: &str) -> &str {
    let unit = unit
        .strip_suffix(".scope")
        .or_else(|| unit.strip_suffix(".service"))
        .or_else(|| unit.strip_suffix(".slice"))
        .unwrap_or(unit);
    let unit = unit.split('@').next().unwrap_or(unit);
    // Trailing "-<pid or random hex>" added by the launcher.
    match unit.rsplit_once('-') {
        Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_hexdigit()) => head,
        _ => unit,
    }
}

/// systemd escapes '-' inside unit name components as "\x2d".
fn unescape(s: &str) -> String {
    s.replace("\\x2d", "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_from_real_cgroups() {
        let base = "/user.slice/user-1000.slice/user@1000.service/app.slice/";
        assert_eq!(
            candidate_ids(&format!("{base}app-gnome-com.anthropic.Claude-7841.scope")),
            ["gnome-com.anthropic.Claude", "com.anthropic.Claude"]
        );
        assert_eq!(
            candidate_ids(&format!("{base}app-flatpak-com.brave.Browser-278363502.scope")),
            ["flatpak-com.brave.Browser", "com.brave.Browser"]
        );
        assert_eq!(candidate_ids(&format!("{base}app-com.anthropic.Claude-7841.scope"))[0], "com.anthropic.Claude");
        assert_eq!(
            candidate_ids(&format!("{base}app-gnome-google\\x2dchrome-99.scope"))[1],
            "google-chrome"
        );
        assert_eq!(candidate_ids(&format!("{base}snap.firefox.firefox-1a2b.scope"))[0], "firefox_firefox");
        assert!(candidate_ids(&format!("{base}vte-spawn-1234.scope")).is_empty());
        assert!(candidate_ids("/system.slice/cron.service").is_empty());
    }

    #[test]
    fn desktop_name_ignores_actions() {
        let text = "[Desktop Entry]\nType=Application\nName[de]=Datei\nName=Files\n[Desktop Action new]\nName=New Window\n";
        assert_eq!(parse_desktop_name(text).as_deref(), Some("Files"));
    }
}
