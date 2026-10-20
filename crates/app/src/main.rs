slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    let ui = MainWindow::new()?;

    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    let initials: String = user.chars().take(2).collect::<String>().to_uppercase();
    ui.set_user_name(user.into());
    ui.set_user_initials(initials.into());

    // Empty bars per logical CPU until the M1 sampler fills in real load.
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    ui.set_core_load(std::rc::Rc::new(slint::VecModel::from(vec![0.0f32; cores])).into());

    // Dev aid: TASKMONITOR_SNAPSHOT=out.ppm saves the rendered window and quits.
    // Wayland compositors block outside screenshots, so this is how we check the UI.
    if let Ok(path) = std::env::var("TASKMONITOR_SNAPSHOT") {
        if let Ok(theme) = std::env::var("TASKMONITOR_THEME") {
            ui.global::<Theme>().set_dark(theme == "dark");
        }
        let weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(800), move || {
            let ui = weak.unwrap();
            match ui.window().take_snapshot() {
                Ok(img) => {
                    let mut ppm = format!("P6 {} {} 255\n", img.width(), img.height()).into_bytes();
                    ppm.extend(img.as_slice().iter().flat_map(|p| [p.r, p.g, p.b]));
                    std::fs::write(&path, ppm).expect("write snapshot");
                }
                Err(e) => eprintln!("snapshot failed: {e}"),
            }
            let _ = slint::quit_event_loop();
        });
    }

    ui.run()
}
