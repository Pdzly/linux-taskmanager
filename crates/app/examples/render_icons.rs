//! Renders logo/taskmonitorlogo.svg to the PNGs the app and the package use:
//! `cargo run -p taskmonitor --example render_icons`
//!
//! - crates/app/ui/icons/logo-<n>.png  (drawn in the top-left corner of the app)
//! - packaging/icons/hicolor/<n>x<n>/apps/io.github.taskmonitor.png  (menu/taskbar icon)

use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let svg = std::fs::read(root.join("logo/taskmonitorlogo.svg")).expect("read logo SVG");
    let tree = resvg::usvg::Tree::from_data(&svg, &resvg::usvg::Options::default()).expect("parse logo SVG");

    let render = |size: u32, out: &Path| {
        let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).expect("pixmap");
        let scale = size as f32 / tree.size().width();
        resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        pixmap.save_png(out).expect("write PNG");
        println!("{}", out.display());
    };

    // 52 px in the UI; 104 covers 2x displays.
    render(104, &root.join("crates/app/ui/icons/logo-104.png"));
    for size in [16, 22, 24, 32, 48, 64, 128, 256, 512] {
        render(size, &root.join(format!("packaging/icons/hicolor/{size}x{size}/apps/io.github.taskmonitor.png")));
    }
    let scalable = root.join("packaging/icons/hicolor/scalable/apps/io.github.taskmonitor.svg");
    std::fs::create_dir_all(scalable.parent().unwrap()).unwrap();
    std::fs::copy(root.join("logo/taskmonitorlogo.svg"), &scalable).unwrap();
    println!("{}", scalable.display());
}
