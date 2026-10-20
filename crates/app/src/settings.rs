//! User settings in ~/.config/taskmonitor/config as plain `key = value` lines.

use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// 0 = follow the desktop, 1 = light, 2 = dark.
    pub theme: i32,
    /// Index into the refresh-rate choices (see INTERVALS_MS).
    pub refresh: i32,
    pub start_page: String,
    pub always_on_top: bool,
    pub app_history: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { theme: 0, refresh: 1, start_page: "processes".into(), always_on_top: false, app_history: false }
    }
}

fn path() -> PathBuf {
    let config = std::env::var("XDG_CONFIG_HOME")
        .unwrap_or_else(|_| format!("{}/.config", std::env::var("HOME").unwrap_or_default()));
    PathBuf::from(config).join("taskmonitor/config")
}

impl Settings {
    pub fn load() -> Self {
        fs::read_to_string(path()).map(|t| Self::parse(&t)).unwrap_or_default()
    }

    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            match key.trim() {
                "theme" => s.theme = value.parse().unwrap_or(0).clamp(0, 2),
                "refresh" => s.refresh = value.parse().unwrap_or(1).clamp(0, 4),
                "start_page" => s.start_page = value.to_owned(),
                "always_on_top" => s.always_on_top = value == "true",
                "app_history" => s.app_history = value == "true",
                _ => {}
            }
        }
        s
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = format!(
            "# TaskMonitor settings\ntheme = {}\nrefresh = {}\nstart_page = {}\nalways_on_top = {}\napp_history = {}\n",
            self.theme, self.refresh, self.start_page, self.always_on_top, self.app_history
        );
        fs::write(path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_with_defaults_and_clamping() {
        let s = Settings::parse("theme = 2\nrefresh = 99\napp_history = true\nunknown = x\n");
        assert_eq!(s, Settings { theme: 2, refresh: 4, app_history: true, ..Default::default() });
        assert_eq!(Settings::parse(""), Settings::default());
    }
}
