mod bench;
mod bridge;
mod format;

use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use taskmonitor_core::Sampler;

slint::include_modules!();

/// Refresh intervals in ms, indexed like `refresh-labels` in shell.slint. 0 = paused.
const INTERVALS_MS: [u64; 5] = [500, 1000, 2000, 5000, 0];

fn main() -> Result<(), slint::PlatformError> {
    if std::env::var_os("TASKMONITOR_BENCH").is_some() {
        bench::run();
        return Ok(());
    }
    let ui = MainWindow::new()?;

    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    let initials: String = user.chars().take(2).collect::<String>().to_uppercase();
    ui.set_user_name(user.into());
    ui.set_user_initials(initials.into());

    let sampler = spawn_sampler(ui.as_weak());
    ui.on_refresh_changed(move |i| {
        let _ = sampler.send(INTERVALS_MS.get(i as usize).copied().unwrap_or(1000));
    });

    // Dev aid: TASKMONITOR_SNAPSHOT=out.ppm saves the rendered window and quits.
    // Wayland compositors block outside screenshots, so this is how we check the UI.
    if let Ok(path) = std::env::var("TASKMONITOR_SNAPSHOT") {
        if let Ok(theme) = std::env::var("TASKMONITOR_THEME") {
            ui.global::<Theme>().set_dark(theme == "dark");
        }
        let delay = std::env::var("TASKMONITOR_SNAPSHOT_DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(800);
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_millis(delay), move || {
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

/// Samples on a background thread and pushes updates into the UI.
/// Send a new interval in ms (0 = pause) through the returned channel.
fn spawn_sampler(ui: slint::Weak<MainWindow>) -> mpsc::Sender<u64> {
    let (tx, rx) = mpsc::channel::<u64>();
    std::thread::Builder::new()
        .name("sampler".into())
        .spawn(move || {
            let mut sampler = Sampler::new();
            let mut histories = bridge::Histories::default();
            let mut interval = Some(Duration::from_millis(1000));
            // The first tick comes quickly so the window doesn't sit on placeholders.
            let mut wait = Some(Duration::from_millis(250));
            loop {
                let msg = match wait {
                    Some(d) => rx.recv_timeout(d),
                    None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                };
                match msg {
                    Ok(ms) => {
                        let resumed = interval.is_none() && ms > 0;
                        interval = (ms > 0).then(|| Duration::from_millis(ms));
                        wait = interval;
                        if resumed {
                            // Reset the baseline so the first rate after a pause isn't averaged over it.
                            sampler.sample();
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                wait = interval;
                let update = bridge::build(&sampler.sample(), &mut histories);
                if ui.upgrade_in_event_loop(move |ui| bridge::apply(&ui, update)).is_err() {
                    break; // window closed
                }
            }
        })
        .expect("spawn sampler thread");
    tx
}
