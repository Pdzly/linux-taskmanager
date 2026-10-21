//! App icons for the tables: finds an icon in the user's icon theme (following the
//! freedesktop icon theme spec) and renders it once into a small bitmap.
//!
//! Bitmaps rather than SVG images because Slint's software renderer re-rasterizes an
//! SVG on every frame it draws it; a decoded bitmap is just copied.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use slint::{Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use crate::MainWindow;

/// Rendered size. Icons are drawn at ~30 px; 64 keeps them sharp on 2x displays.
const SIZE: u32 = 64;

/// Icon slots handed to the UI: rows refer to icons by slot number, so each icon is
/// rendered and sent to the UI once.
pub struct IconStore {
    slots: HashMap<Arc<str>, i32>,
    next: i32,
    pending: Vec<(i32, SharedPixelBuffer<Rgba8Pixel>)>,
    themes: Vec<Theme>,
    pixmaps: Vec<PathBuf>,
}

impl Default for IconStore {
    fn default() -> Self {
        Self::new()
    }
}

impl IconStore {
    pub fn new() -> Self {
        let bases = base_dirs();
        let mut themes = Vec::new();
        let mut queue: Vec<String> = vec![current_theme().unwrap_or_default()];
        queue.extend(["hicolor", "Adwaita", "breeze", "Yaru"].map(String::from));
        while let Some(name) = queue.first().cloned() {
            queue.remove(0);
            if name.is_empty() || themes.iter().any(|t: &Theme| t.name == name) {
                continue;
            }
            if let Some(theme) = Theme::load(&name, &bases) {
                // Inherited themes go right after the theme that inherits them.
                for (i, parent) in theme.inherits.iter().enumerate() {
                    queue.insert(i, parent.clone());
                }
                themes.push(theme);
            }
        }
        let pixmaps = std::iter::once(PathBuf::from("/usr/share/pixmaps")).collect();
        Self { slots: HashMap::new(), next: 0, pending: Vec::new(), themes, pixmaps }
    }

    /// Slot of the icon for an `Icon=` value, or -1 if there is none. The first call for
    /// a name finds and renders it; later calls are a hash lookup.
    pub fn slot(&mut self, icon: Option<&Arc<str>>) -> i32 {
        let Some(icon) = icon else { return -1 };
        if let Some(&slot) = self.slots.get(icon) {
            return slot;
        }
        let slot = match self.find(icon).and_then(|p| render(&p)) {
            Some(buffer) => {
                let slot = self.next;
                self.next += 1;
                self.pending.push((slot, buffer));
                slot
            }
            None => -1,
        };
        self.slots.insert(icon.clone(), slot);
        slot
    }

    /// Icons rendered since the last call, to hand to the UI thread.
    pub fn take_pending(&mut self) -> Vec<(i32, SharedPixelBuffer<Rgba8Pixel>)> {
        std::mem::take(&mut self.pending)
    }

    /// A store that finds nothing, for tests and the benchmark.
    #[allow(dead_code)]
    pub fn empty() -> Self {
        Self { slots: HashMap::new(), next: 0, pending: Vec::new(), themes: Vec::new(), pixmaps: Vec::new() }
    }

    fn find(&self, icon: &str) -> Option<PathBuf> {
        let path = Path::new(icon);
        if path.is_absolute() {
            return path.exists().then(|| path.to_owned());
        }
        // Some entries spell out an extension ("foo.png"); the spec says to drop it.
        let name = icon.strip_suffix(".png").or_else(|| icon.strip_suffix(".svg")).unwrap_or(icon);
        for theme in &self.themes {
            for dir in &theme.dirs {
                for ext in ["svg", "png"] {
                    let file = dir.join(format!("{name}.{ext}"));
                    if file.exists() {
                        return Some(file);
                    }
                }
            }
        }
        self.pixmaps.iter().flat_map(|d| ["svg", "png"].map(|e| d.join(format!("{name}.{e}")))).find(|p| p.exists())
    }
}

/// Puts newly rendered icons into the window's `app-icons` list (UI thread only).
/// Must run before the rows that refer to them are applied.
pub fn apply(ui: &MainWindow, icons: Vec<(i32, SharedPixelBuffer<Rgba8Pixel>)>) {
    if icons.is_empty() {
        return;
    }
    let model = ui.get_app_icons();
    let model = match model.as_any().downcast_ref::<VecModel<Image>>() {
        Some(_) => model,
        None => {
            let fresh = ModelRc::new(VecModel::<Image>::default());
            ui.set_app_icons(fresh.clone());
            fresh
        }
    };
    let list = model.as_any().downcast_ref::<VecModel<Image>>().expect("set above");
    for (slot, buffer) in icons {
        while list.row_count() <= slot as usize {
            list.push(Image::default());
        }
        list.set_row_data(slot as usize, Image::from_rgba8_premultiplied(buffer));
    }
}

/// One icon theme: its app-icon directories, best size first, that exist on disk.
struct Theme {
    name: String,
    inherits: Vec<String>,
    dirs: Vec<PathBuf>,
}

impl Theme {
    fn load(name: &str, bases: &[PathBuf]) -> Option<Theme> {
        let roots: Vec<PathBuf> = bases.iter().map(|b| b.join(name)).filter(|p| p.is_dir()).collect();
        let index = roots.iter().find_map(|r| fs::read_to_string(r.join("index.theme")).ok())?;
        let groups = parse_ini(&index);
        let head = groups.get("Icon Theme")?;
        let inherits = head.get("Inherits").map(|v| list(v)).unwrap_or_default();
        let mut subdirs: Vec<(u32, String)> = head
            .get("Directories")
            .map(|v| list(v))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|dir| {
                let g = groups.get(dir.as_str())?;
                let context = g.get("Context").map(String::as_str).unwrap_or("");
                if !matches!(context, "Applications" | "Apps" | "") || g.get("Scale").is_some_and(|s| s != "1") {
                    return None;
                }
                let size: u32 = g.get("Size").and_then(|s| s.parse().ok()).unwrap_or(0);
                let scalable = g.get("Type").is_some_and(|t| t == "Scalable");
                Some((preference(size, scalable), dir))
            })
            .collect();
        subdirs.sort();
        let dirs = subdirs
            .into_iter()
            .flat_map(|(_, d)| roots.iter().map(move |r| r.join(&d)))
            .filter(|p| p.is_dir())
            .collect();
        Some(Theme { name: name.to_owned(), inherits, dirs })
    }
}

/// Lower is better: exactly our size, then scalable, then bigger, then smaller.
fn preference(size: u32, scalable: bool) -> u32 {
    if scalable {
        1
    } else if size == SIZE {
        0
    } else if size > SIZE {
        size
    } else {
        10_000 - size
    }
}

fn list(v: &str) -> Vec<String> {
    v.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect()
}

fn parse_ini(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut groups: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current = String::new();
    for line in text.lines().map(str::trim) {
        if let Some(group) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = group.to_owned();
        } else if let Some((k, v)) = line.split_once('=') {
            groups.entry(current.clone()).or_default().insert(k.trim().to_owned(), v.trim().to_owned());
        }
    }
    groups
}

/// Icon theme base directories in spec order.
fn base_dirs() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let data_home = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{home}/.local/share"));
    let data_dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    std::iter::once(format!("{home}/.icons"))
        .chain(std::iter::once(data_home))
        .chain(data_dirs.split(':').map(String::from))
        .filter(|d| !d.is_empty())
        .map(|d| if d.ends_with(".icons") { PathBuf::from(d) } else { PathBuf::from(d).join("icons") })
        .collect()
}

/// The desktop's icon theme: KDE and COSMIC keep it in config files, GNOME-based
/// desktops in GSettings.
fn current_theme() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    if desktop.contains("KDE")
        && let Ok(text) = fs::read_to_string(format!("{home}/.config/kdeglobals"))
        && let Some(theme) = parse_ini(&text).get("Icons").and_then(|g| g.get("Theme"))
    {
        return Some(theme.clone());
    }
    if desktop.contains("COSMIC")
        && let Ok(text) = fs::read_to_string(format!("{home}/.config/cosmic/com.system76.CosmicTk/v1/icon_theme"))
    {
        return Some(text.trim().trim_matches('"').to_owned());
    }
    let out = std::process::Command::new("gsettings").args(["get", "org.gnome.desktop.interface", "icon-theme"]).output().ok()?;
    let theme = String::from_utf8_lossy(&out.stdout).trim().trim_matches('\'').to_owned();
    (!theme.is_empty()).then_some(theme)
}

/// Decodes or rasterizes an icon file into a SIZE×SIZE premultiplied RGBA bitmap,
/// keeping its aspect ratio and centring it.
fn render(path: &Path) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    use resvg::tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};
    let data = fs::read(path).ok()?;
    let mut out = Pixmap::new(SIZE, SIZE)?;
    if path.extension().is_some_and(|e| e == "svg") {
        let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
        let (w, h) = (tree.size().width(), tree.size().height());
        let scale = SIZE as f32 / w.max(h);
        let (dx, dy) = ((SIZE as f32 - w * scale) / 2.0, (SIZE as f32 - h * scale) / 2.0);
        resvg::render(&tree, Transform::from_scale(scale, scale).post_translate(dx, dy), &mut out.as_mut());
    } else {
        let src = Pixmap::decode_png(&data).ok()?;
        let scale = SIZE as f32 / src.width().max(src.height()) as f32;
        let (dx, dy) =
            ((SIZE as f32 - src.width() as f32 * scale) / 2.0, (SIZE as f32 - src.height() as f32 * scale) / 2.0);
        let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..Default::default() };
        out.draw_pixmap(0, 0, src.as_ref(), &paint, Transform::from_scale(scale, scale).post_translate(dx, dy), None);
    }
    Some(SharedPixelBuffer::clone_from_slice(out.data(), SIZE, SIZE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_exact_then_scalable_then_bigger() {
        let mut v = [
            (preference(16, false), "16"),
            (preference(128, false), "128"),
            (preference(0, true), "svg"),
            (preference(64, false), "64"),
            (preference(48, false), "48"),
        ];
        v.sort();
        assert_eq!(v.iter().map(|x| x.1).collect::<Vec<_>>(), ["64", "svg", "128", "48", "16"]);
    }

    #[test]
    fn finds_and_renders_real_icons() {
        let mut store = IconStore::new();
        // Every desktop has the hicolor theme; most have a terminal or settings icon.
        let found = ["utilities-terminal", "org.gnome.Settings", "preferences-system", "system-file-manager"]
            .iter()
            .map(|n| store.slot(Some(&Arc::from(*n))))
            .filter(|s| *s >= 0)
            .count();
        assert!(found > 0, "no common icon found in any theme: {:?}", store.themes.iter().map(|t| &t.name).collect::<Vec<_>>());
        let pending = store.take_pending();
        assert_eq!(pending.len(), found);
        assert!(pending.iter().all(|(_, b)| b.width() == SIZE && b.as_bytes().iter().any(|&p| p != 0)));
        assert_eq!(store.slot(Some(&Arc::from("no-such-icon-anywhere"))), -1);
    }
}
