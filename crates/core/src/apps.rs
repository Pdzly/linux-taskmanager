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
    /// `Icon=` from the desktop entry: a theme icon name or an absolute path.
    pub icon: Option<Arc<str>>,
}

#[derive(Clone, Debug)]
struct Entry {
    name: Arc<str>,
    icon: Option<Arc<str>>,
}

/// Installed `.desktop` entries by id.
pub struct Registry {
    apps: HashMap<String, Entry>,
    /// Program file name from `Exec=` -> icon, to give background processes an icon.
    by_program: HashMap<String, Arc<str>>,
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
        let mut by_program = HashMap::new();
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
                let Ok(text) = std::fs::read_to_string(&path) else { continue };
                let Some(name) = parse_desktop_name(&text) else { continue };
                let icon: Option<Arc<str>> = desktop_value(&text, "Icon").filter(|i| !i.is_empty()).map(Arc::from);
                if let (Some(icon), Some(program)) = (&icon, desktop_value(&text, "Exec").and_then(|e| program_name(&e))) {
                    by_program.entry(program).or_insert_with(|| icon.clone());
                }
                apps.insert(id.to_owned(), Entry { name: Arc::from(name), icon });
            }
        }
        Self { apps, by_program, scanned: Instant::now() }
    }

    /// Icon of the app with this desktop id (for App history, which stores ids).
    pub fn icon_for_id(&self, id: &str) -> Option<Arc<str>> {
        self.apps.get(id).and_then(|e| e.icon.clone())
    }

    /// Icon of the desktop app whose `Exec=` runs this program ("nautilus" -> its icon).
    pub fn icon_for_program(&self, program: &str) -> Option<Arc<str>> {
        self.by_program.get(program).cloned()
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
        let app = |id: &str, e: &Entry| App { id: Arc::from(id), name: e.name.clone(), icon: e.icon.clone() };
        for c in candidates {
            if let Some((id, e)) = self.apps.get_key_value(c.as_str()) {
                return Some(app(id, e));
            }
        }
        // KDE scopes may use a short name ("dolphin") for "org.kde.dolphin".
        for c in candidates {
            let suffix = format!(".{c}");
            if let Some((id, e)) = self.apps.iter().find(|(id, _)| id.ends_with(&suffix)) {
                return Some(app(id, e));
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

/// The unlocalized value of `key` in the `[Desktop Entry]` group.
pub fn desktop_value(text: &str, key: &str) -> Option<String> {
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim().to_owned());
        }
    }
    None
}

/// File name of the program an `Exec=` line starts ("/usr/bin/nautilus --new-window" ->
/// "nautilus"). None for launchers like `env`, `flatpak` or `sh`, which say nothing
/// about the process that ends up running.
pub fn program_name(exec: &str) -> Option<String> {
    let exec = exec.trim();
    let first = match exec.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => exec.split_whitespace().next()?,
    };
    let name = first.rsplit('/').next()?;
    let launcher = matches!(name, "env" | "flatpak" | "sh" | "bash" | "snap" | "gtk-launch" | "pkexec" | "sudo");
    (!name.is_empty() && !launcher).then(|| name.to_owned())
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
        // KDE Plasma: random hex suffixes on scopes, UUID instances on services.
        assert_eq!(candidate_ids(&format!("{base}app-org.kde.dolphin-3f2b9c1d0e8a4b5c9d7e6f5a4b3c2d1e.scope"))[0], "org.kde.dolphin");
        assert_eq!(candidate_ids(&format!("{base}app-org.kde.konsole@0a1b2c3d4e5f.service"))[0], "org.kde.konsole");
        assert_eq!(candidate_ids(&format!("{base}app-firefox@autostart.service"))[0], "firefox");
        assert!(candidate_ids(&format!("{base}vte-spawn-1234.scope")).is_empty());
        assert!(candidate_ids("/system.slice/cron.service").is_empty());
    }

    #[test]
    fn program_names_from_exec() {
        assert_eq!(program_name("/usr/bin/nautilus --new-window %U").as_deref(), Some("nautilus"));
        assert_eq!(program_name("\"/opt/Brave Browser/brave\"").as_deref(), Some("brave"));
        assert_eq!(program_name("env GDK_BACKEND=x11 foo"), None);
        assert_eq!(program_name("flatpak run org.x.Y"), None);
    }

    #[test]
    fn desktop_name_ignores_actions() {
        let text = "[Desktop Entry]\nType=Application\nName[de]=Datei\nName=Files\n[Desktop Action new]\nName=New Window\n";
        assert_eq!(parse_desktop_name(text).as_deref(), Some("Files"));
    }
}
