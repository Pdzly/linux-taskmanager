//! Startup apps: XDG autostart entries (`~/.config/autostart`, `/etc/xdg/autostart`).
//! Disabling follows the XDG spec: a same-named file in the user's directory with
//! `Hidden=true` overrides the system entry, which is never touched.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct StartupEntry {
    /// File name without `.desktop`.
    pub id: String,
    pub name: String,
    pub command: String,
    pub enabled: bool,
    /// The entry comes from the system directory (possibly overridden by the user).
    pub system: bool,
}

fn user_dir() -> PathBuf {
    let config = std::env::var("XDG_CONFIG_HOME")
        .unwrap_or_else(|_| format!("{}/.config", std::env::var("HOME").unwrap_or_default()));
    PathBuf::from(config).join("autostart")
}

fn system_dirs() -> Vec<PathBuf> {
    std::env::var("XDG_CONFIG_DIRS")
        .unwrap_or_else(|_| "/etc/xdg".into())
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| PathBuf::from(d).join("autostart"))
        .collect()
}

/// Entries that apply to the current desktop, user files overriding system ones.
pub fn list() -> Vec<StartupEntry> {
    let desktops: Vec<String> =
        std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().split(':').map(str::to_owned).collect();
    let mut out: Vec<StartupEntry> = Vec::new();
    let dirs = std::iter::once((user_dir(), false)).chain(system_dirs().into_iter().map(|d| (d, true)));
    for (dir, system) in dirs {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else { continue };
            if path.extension().is_none_or(|e| e != "desktop") {
                continue;
            }
            if let Some(existing) = out.iter_mut().find(|e| e.id == id) {
                existing.system |= system;
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else { continue };
            if let Some(e) = parse(&id, &text, &desktops, system) {
                out.push(e);
            }
        }
    }
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

pub fn parse(id: &str, text: &str, desktops: &[String], system: bool) -> Option<StartupEntry> {
    let get = |key: &str| desktop_value(text, key);
    let shown_here = |key: &str, want: bool| {
        get(key).is_none_or(|list| list.split(';').any(|d| desktops.iter().any(|c| c == d)) == want)
    };
    if !shown_here("OnlyShowIn", true) || !shown_here("NotShowIn", false) {
        return None;
    }
    let enabled = get("Hidden").as_deref() != Some("true") && get("X-GNOME-Autostart-enabled").as_deref() != Some("false");
    Some(StartupEntry {
        id: id.to_owned(),
        name: get("Name").unwrap_or_else(|| id.to_owned()),
        command: get("Exec").unwrap_or_default(),
        enabled,
        system,
    })
}

/// Value of `key` in the `[Desktop Entry]` group (unlocalized).
fn desktop_value(text: &str, key: &str) -> Option<String> {
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

/// Sets `key=value` in the `[Desktop Entry]` group, replacing an existing line.
pub fn set_desktop_value(text: &str, key: &str, value: &str) -> String {
    let mut out = Vec::new();
    let (mut in_entry, mut done) = (false, false);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_entry && !done {
                out.push(format!("{key}={value}"));
                done = true;
            }
            in_entry = trimmed == "[Desktop Entry]";
        } else if in_entry && trimmed.split_once('=').is_some_and(|(k, _)| k.trim() == key) {
            if !done {
                out.push(format!("{key}={value}"));
                done = true;
            }
            continue;
        }
        out.push(line.to_owned());
    }
    if in_entry && !done {
        out.push(format!("{key}={value}"));
    }
    out.join("\n") + "\n"
}

pub fn set_enabled(id: &str, enabled: bool) -> io::Result<()> {
    let user_file = user_dir().join(format!("{id}.desktop"));
    let source = if user_file.exists() {
        user_file.clone()
    } else {
        system_dirs()
            .into_iter()
            .map(|d| d.join(format!("{id}.desktop")))
            .find(|p| p.exists())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "startup entry not found"))?
    };
    let text = fs::read_to_string(&source)?;
    let text = set_desktop_value(&text, "Hidden", if enabled { "false" } else { "true" });
    let text = set_desktop_value(&text, "X-GNOME-Autostart-enabled", if enabled { "true" } else { "false" });
    fs::create_dir_all(user_dir())?;
    write_atomically(&user_file, &text)
}

fn write_atomically(path: &Path, text: &str) -> io::Result<()> {
    let tmp = path.with_extension("desktop.tmp");
    fs::write(&tmp, text)?;
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTRY: &str = "[Desktop Entry]\nType=Application\nName=Blip\nExec=blip --background\nX-GNOME-Autostart-enabled=true\n\n[Desktop Action x]\nName=Other\n";

    #[test]
    fn parses_and_filters_by_desktop() {
        let gnome = vec!["ubuntu".to_owned(), "GNOME".to_owned()];
        let e = parse("blip", ENTRY, &gnome, false).unwrap();
        assert_eq!((e.name.as_str(), e.command.as_str(), e.enabled), ("Blip", "blip --background", true));
        let kde_only = "[Desktop Entry]\nName=K\nOnlyShowIn=KDE;\n";
        assert!(parse("k", kde_only, &gnome, true).is_none());
        let not_gnome = "[Desktop Entry]\nName=K\nNotShowIn=GNOME;\n";
        assert!(parse("k", not_gnome, &gnome, true).is_none());
    }

    #[test]
    fn toggling_rewrites_only_the_entry_group() {
        let off = set_desktop_value(ENTRY, "Hidden", "true");
        let off = set_desktop_value(&off, "X-GNOME-Autostart-enabled", "false");
        assert!(!parse("blip", &off, &[], false).unwrap().enabled);
        assert!(off.contains("[Desktop Action x]\nName=Other"));
        assert_eq!(off.matches("X-GNOME-Autostart-enabled").count(), 1);
        let on = set_desktop_value(&off, "Hidden", "false");
        let on = set_desktop_value(&on, "X-GNOME-Autostart-enabled", "true");
        assert!(parse("blip", &on, &[], false).unwrap().enabled);
    }
}
