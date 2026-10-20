mod bench;
mod bridge;
mod format;
mod processes;

use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use slint::SharedString;
use taskmonitor_core::{Process, ProcessSampler, Sampler, Snapshot};

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

    let tx = spawn_sampler(ui.as_weak());
    let send = move |cmd| {
        let _ = tx.send(cmd);
    };
    ui.on_refresh_changed({
        let send = send.clone();
        move |i| send(Cmd::Interval(INTERVALS_MS.get(i as usize).copied().unwrap_or(1000)))
    });
    ui.on_page_changed({
        let send = send.clone();
        move |p| send(Cmd::Page(p))
    });
    ui.on_sort({
        let send = send.clone();
        move |c| send(Cmd::Sort(c))
    });
    ui.on_toggle({
        let send = send.clone();
        move |k| send(Cmd::Toggle(k))
    });
    ui.on_filter_changed(move |f| send(Cmd::Filter(f)));
    if let Ok(page) = std::env::var("TASKMONITOR_PAGE") {
        let page = match page.as_str() {
            "processes" => Page::Processes,
            "details" => Page::Details,
            _ => Page::Performance,
        };
        ui.set_page(page);
        ui.invoke_page_changed(page);
    }

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

/// Messages from the UI to the sampler thread.
enum Cmd {
    /// Refresh interval in ms; 0 pauses.
    Interval(u64),
    Page(Page),
    Sort(SharedString),
    Filter(SharedString),
    Toggle(SharedString),
}

/// Samples on a background thread and pushes updates into the UI.
/// Per-process data is only collected while a page that shows it is open.
fn spawn_sampler(ui: slint::Weak<MainWindow>) -> mpsc::Sender<Cmd> {
    let (tx, rx) = mpsc::channel::<Cmd>();
    std::thread::Builder::new()
        .name("sampler".into())
        .spawn(move || {
            let mut sampler = Sampler::new();
            let mut procs_sampler: Option<ProcessSampler> = None;
            let mut histories = bridge::Histories::default();
            let mut view = processes::View::default();
            let mut page = Page::Performance;
            let mut last: (Snapshot, Vec<Process>) = Default::default();
            let mut interval = Some(Duration::from_millis(1000));
            // The first tick comes quickly so the window doesn't sit on placeholders.
            let mut wait = Some(Duration::from_millis(250));

            let push_rows = |view: &processes::View, page: Page, last: &(Snapshot, Vec<Process>)| {
                let rows = view.build(page, &last.1, &last.0);
                ui.upgrade_in_event_loop(move |ui| processes::apply(&ui, rows)).is_ok()
            };

            loop {
                let msg = match wait {
                    Some(d) => rx.recv_timeout(d),
                    None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                };
                let shows_procs = |p: Page| matches!(p, Page::Processes | Page::Details);
                match msg {
                    Ok(Cmd::Interval(ms)) => {
                        let resumed = interval.is_none() && ms > 0;
                        interval = (ms > 0).then(|| Duration::from_millis(ms));
                        wait = interval;
                        if resumed {
                            // Reset the baseline so the first rate after a pause isn't averaged over it.
                            sampler.sample();
                            if let Some(p) = procs_sampler.as_mut() {
                                p.sample();
                            }
                        }
                        continue;
                    }
                    Ok(cmd) => {
                        match cmd {
                            Cmd::Page(p) => {
                                page = p;
                                // Sample right away so the table isn't empty for a whole interval.
                                if shows_procs(p) {
                                    let ps = procs_sampler.get_or_insert_with(ProcessSampler::new);
                                    last.1 = ps.sample();
                                }
                            }
                            Cmd::Sort(c) => view.set_sort(c),
                            Cmd::Filter(f) => view.filter = f.to_string(),
                            Cmd::Toggle(k) => view.toggle(k),
                            Cmd::Interval(_) => unreachable!(),
                        }
                        if shows_procs(page) && !push_rows(&view, page, &last) {
                            break;
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                wait = interval;
                let snap = sampler.sample();
                let update = bridge::build(&snap, &mut histories);
                if ui.upgrade_in_event_loop(move |ui| bridge::apply(&ui, update)).is_err() {
                    break; // window closed
                }
                last.0 = snap;
                if shows_procs(page) {
                    last.1 = procs_sampler.get_or_insert_with(ProcessSampler::new).sample();
                    if !push_rows(&view, page, &last) {
                        break;
                    }
                }
            }
        })
        .expect("spawn sampler thread");
    tx
}
