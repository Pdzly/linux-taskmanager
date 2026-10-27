mod actions;
mod bench;
mod bridge;
mod format;
mod processes;
mod settings;
mod system_pages;
mod widgets;

use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use slint::{CloseRequestResponse, ComponentHandle, Model, ModelRc, SharedString, VecModel};
use taskmonitor_core::app_usage::{self, AppUsageLog};
use taskmonitor_core::services::{Bus, Service, Services};
use taskmonitor_core::users::{Logind, Session};
use taskmonitor_core::{GpuSampler, Process, ProcessSampler, Sampler, Snapshot};

slint::include_modules!();

/// Refresh intervals in ms, indexed like `refresh-labels` in shell.slint. 0 = paused.
const INTERVALS_MS: [u64; 5] = [500, 1000, 2000, 5000, 0];

fn main() -> Result<(), slint::PlatformError> {
    if std::env::var_os("TASKMONITOR_BENCH").is_some() {
        bench::run();
        return Ok(());
    }

    let widget_only = std::env::args().skip(1).any(|arg| arg == "--widgets");
    let ui = MainWindow::new()?;
    let desktop_widgets = widgets::create()?;
    // Lets the desktop match the window to io.github.taskmonitor.desktop (name and icon).
    // Needs the platform that MainWindow::new() set up, and must happen before showing.
    slint::set_xdg_app_id("io.github.taskmonitor")?;
    ui.set_version(env!("CARGO_PKG_VERSION").into());

    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    let initials: String = user.chars().take(2).collect::<String>().to_uppercase();
    ui.set_user_name(user.into());
    ui.set_user_initials(initials.into());

    // Settings: theme, refresh rate, start page, always on top, app history, desktop widgets.
    let prefs = settings::Settings::load();
    let widgets_enabled = prefs.desktop_widgets || widget_only;
    ui.set_theme_choice(prefs.theme);
    ui.set_refresh_index(prefs.refresh);
    ui.set_on_top(prefs.always_on_top);
    ui.set_app_history(prefs.app_history);
    ui.set_desktop_widgets(widgets_enabled);
    desktop_widgets.set_theme_choice(prefs.theme);
    desktop_widgets.set_paused(prefs.refresh == 4);

    let start = match prefs.start_page.as_str() {
        "performance" => (1, Page::Performance),
        "details" => (2, Page::Details),
        _ => (0, Page::Processes),
    };
    ui.set_start_page(start.0);
    ui.set_page(start.1);

    let main_visible = Rc::new(Cell::new(!widget_only));
    let widgets_visible = Rc::new(Cell::new(false));
    let (tx, sampler_thread) = spawn_sampler(
        ui.as_weak(),
        desktop_widgets.as_weak(),
        main_visible.get(),
        false,
    );
    let quit = tx.clone();
    let send = move |cmd| {
        let _ = tx.send(cmd);
    };

    send(Cmd::Page(start.1));
    if prefs.refresh != settings::Settings::default().refresh {
        send(Cmd::Interval(INTERVALS_MS[prefs.refresh as usize]));
    }
    send(Cmd::History(prefs.app_history));
    ui.on_settings_changed({
        let send = send.clone();
        let ui = ui.as_weak();
        let desktop_widgets = desktop_widgets.as_weak();
        let widgets_visible = widgets_visible.clone();
        move || {
            let ui = ui.unwrap();
            let wants_widgets = ui.get_desktop_widgets();
            if wants_widgets && !widgets_visible.get() {
                match desktop_widgets.upgrade() {
                    Some(widget) => match widget.show() {
                        Ok(()) => {
                            widgets_visible.set(true);
                            send(Cmd::WidgetsVisible(true));
                        }
                        Err(e) => {
                            ui.set_desktop_widgets(false);
                            ui.set_toast(format!("Couldn't show desktop widgets: {e}").into());
                        }
                    },
                    None => {
                        ui.set_desktop_widgets(false);
                        ui.set_toast("Couldn't show desktop widgets".into());
                    }
                }
            } else if !wants_widgets && widgets_visible.get() {
                match desktop_widgets.upgrade() {
                    Some(widget) => match widget.hide() {
                        Ok(()) => {
                            widgets_visible.set(false);
                            send(Cmd::WidgetsVisible(false));
                        }
                        Err(e) => {
                            ui.set_desktop_widgets(true);
                            ui.set_toast(format!("Couldn't hide desktop widgets: {e}").into());
                        }
                    },
                    None => {
                        widgets_visible.set(false);
                        send(Cmd::WidgetsVisible(false));
                    }
                }
            }
            if let Some(widget) = desktop_widgets.upgrade() {
                widget.set_theme_choice(ui.get_theme_choice());
                widget.set_paused(ui.get_refresh_index() == 4);
            }
            save_settings(&ui);
            send(Cmd::Interval(
                INTERVALS_MS
                    .get(ui.get_refresh_index() as usize)
                    .copied()
                    .unwrap_or(1000),
            ));
            send(Cmd::History(ui.get_app_history()));
        }
    });
    ui.on_clear_history({
        let send = send.clone();
        move || send(Cmd::ClearHistory)
    });
    ui.on_refresh_changed({
        let send = send.clone();
        let ui = ui.as_weak();
        let desktop_widgets = desktop_widgets.as_weak();
        move |i| {
            let ui = ui.unwrap();
            if let Some(widget) = desktop_widgets.upgrade() {
                widget.set_paused(i == 4);
            }
            save_settings(&ui);
            send(Cmd::Interval(
                INTERVALS_MS.get(i as usize).copied().unwrap_or(1000),
            ))
        }
    });
    ui.on_page_changed({
        let send = send.clone();
        move |p| send(Cmd::Page(p))
    });
    ui.on_perf_tab_changed({
        let send = send.clone();
        move |t| send(Cmd::PerfTab(t))
    });
    ui.on_sort({
        let send = send.clone();
        move |c| send(Cmd::Sort(c))
    });
    ui.on_toggle({
        let send = send.clone();
        move |k| send(Cmd::Toggle(k))
    });
    ui.on_filter_changed({
        let send = send.clone();
        move |f| send(Cmd::Filter(f))
    });
    ui.on_action({
        let send = send.clone();
        move |a, k| send(Cmd::Action(a, k))
    });
    ui.on_service_bus_changed({
        let send = send.clone();
        move |user| send(Cmd::ServiceBus(user))
    });
    ui.on_service_act({
        let send = send.clone();
        move |verb, name| send(Cmd::ServiceAct(verb, name))
    });
    ui.on_startup_toggled({
        let send = send.clone();
        move |id, on| send(Cmd::StartupToggle(id, on))
    });
    ui.on_session_act({
        let send = send.clone();
        move |verb, key| send(Cmd::SessionAct(verb, key))
    });
    ui.on_properties({
        let send = send.clone();
        move |k| send(Cmd::Properties(k))
    });
    ui.on_affinity_query({
        let send = send.clone();
        move |k| send(Cmd::AffinityQuery(k))
    });
    ui.on_affinity_all({
        let ui = ui.as_weak();
        move || {
            let ui = ui.unwrap();
            let all = vec![true; ui.get_affinity_cpus().row_count()];
            ui.set_affinity_cpus(ModelRc::new(VecModel::from(all)));
        }
    });
    ui.on_affinity_apply({
        let ui = ui.as_weak();
        let send = send.clone();
        move |k| {
            let mask: String = ui
                .unwrap()
                .get_affinity_cpus()
                .iter()
                .map(|on| if on { '1' } else { '0' })
                .collect();
            if mask.contains('1') {
                send(Cmd::Action(format!("affinity:{mask}").into(), k));
            } else {
                ui.unwrap().set_toast("Pick at least one processor".into());
            }
        }
    });
    ui.on_run_command({
        let ui = ui.as_weak();
        move |cmd| {
            let cmd = cmd.trim();
            if cmd.is_empty() {
                return;
            }
            let msg = match taskmonitor_core::actions::run(cmd) {
                Ok(pid) => format!("Started {cmd} (PID {pid})"),
                Err(e) => format!("Couldn't start {cmd}: {e}"),
            };
            ui.unwrap().set_toast(msg.into());
        }
    });
    ui.window().on_close_requested({
        let main_visible = main_visible.clone();
        let send = send.clone();
        move || {
            main_visible.set(false);
            send(Cmd::MainVisible(false));
            CloseRequestResponse::HideWindow
        }
    });
    ui.on_close_main({
        let ui = ui.as_weak();
        let main_visible = main_visible.clone();
        let send = send.clone();
        move || {
            let ui = ui.unwrap();
            main_visible.set(false);
            send(Cmd::MainVisible(false));
            if let Err(e) = ui.hide() {
                eprintln!("couldn't close TaskMonitor window: {e}");
            }
        }
    });
    desktop_widgets.on_open_main({
        let ui = ui.as_weak();
        let main_visible = main_visible.clone();
        let send = send.clone();
        move || {
            let ui = ui.unwrap();
            match ui.show() {
                Ok(()) => {
                    // Slint's cached minimized state can miss window-manager changes.
                    let _ = slint::winit_030::WinitWindowAccessor::with_winit_window(
                        ui.window(),
                        |window| {
                            window.set_minimized(false);
                            window.focus_window();
                        },
                    );
                    if !main_visible.replace(true) {
                        send(Cmd::MainVisible(true));
                        send(Cmd::Page(ui.get_page()));
                    }
                }
                Err(e) => eprintln!("couldn't show TaskMonitor window: {e}"),
            }
        }
    });
    desktop_widgets.on_dismiss({
        let ui = ui.as_weak();
        let desktop_widgets = desktop_widgets.as_weak();
        let widgets_visible = widgets_visible.clone();
        let send = send.clone();
        move || {
            let ui = ui.unwrap();
            let Some(widget) = desktop_widgets.upgrade() else {
                widgets_visible.set(false);
                ui.set_desktop_widgets(false);
                save_settings(&ui);
                send(Cmd::WidgetsVisible(false));
                return;
            };
            match widget.hide() {
                Ok(()) => {
                    widgets_visible.set(false);
                    ui.set_desktop_widgets(false);
                    save_settings(&ui);
                    send(Cmd::WidgetsVisible(false));
                }
                Err(e) => ui.set_toast(format!("Couldn't hide desktop widgets: {e}").into()),
            }
        }
    });
    desktop_widgets.on_quit(move || {
        if let Err(e) = slint::quit_event_loop() {
            eprintln!("couldn't quit TaskMonitor: {e}");
        }
    });
    desktop_widgets.window().on_close_requested({
        let ui = ui.as_weak();
        let widgets_visible = widgets_visible.clone();
        let send = send.clone();
        move || {
            widgets_visible.set(false);
            send(Cmd::WidgetsVisible(false));
            if let Some(ui) = ui.upgrade() {
                ui.set_desktop_widgets(false);
                save_settings(&ui);
            }
            CloseRequestResponse::HideWindow
        }
    });
    if let Ok(page) = std::env::var("TASKMONITOR_PAGE") {
        let page = match page.as_str() {
            "processes" => Page::Processes,
            "details" => Page::Details,
            "services" => Page::Services,
            "startup" => Page::Startup,
            "users" => Page::Users,
            "history" => Page::History,
            "settings" => Page::Settings,
            _ => Page::Performance,
        };
        ui.set_page(page);
        ui.invoke_page_changed(page);
    }
    if let Some(tab) = std::env::var("TASKMONITOR_TAB")
        .ok()
        .and_then(|t| t.parse().ok())
    {
        ui.set_perf_tab(tab);
        ui.invoke_perf_tab_changed(tab);
    }

    // Dev aid: TASKMONITOR_SNAPSHOT=out.ppm saves the rendered window and quits.
    // Wayland compositors block outside screenshots, so this is how we check the UI.
    if let Ok(path) = std::env::var("TASKMONITOR_SNAPSHOT") {
        if let Ok(theme) = std::env::var("TASKMONITOR_THEME") {
            if widget_only {
                desktop_widgets.global::<Theme>().set_dark(theme == "dark");
            } else {
                ui.global::<Theme>().set_dark(theme == "dark");
            }
        }
        // TASKMONITOR_OPEN=properties|affinity opens that dialog for TaskMonitor itself.
        let own = SharedString::from(format!("pid:{}", std::process::id()));
        match std::env::var("TASKMONITOR_OPEN").as_deref() {
            Ok("properties") => ui.invoke_properties(own),
            Ok("affinity") => ui.invoke_affinity_query(own),
            _ => {}
        }
        let delay = std::env::var("TASKMONITOR_SNAPSHOT_DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(800);
        if widget_only {
            let weak = desktop_widgets.as_weak();
            slint::Timer::single_shot(Duration::from_millis(delay), move || {
                let widgets = weak.unwrap();
                save_snapshot(widgets.window(), &path);
                if let Err(e) = slint::quit_event_loop() {
                    eprintln!("couldn't quit after snapshot: {e}");
                }
            });
        } else {
            let weak = ui.as_weak();
            slint::Timer::single_shot(Duration::from_millis(delay), move || {
                let ui = weak.unwrap();
                save_snapshot(ui.window(), &path);
                if let Err(e) = slint::quit_event_loop() {
                    eprintln!("couldn't quit after snapshot: {e}");
                }
            });
        }
    }

    if widgets_enabled {
        desktop_widgets.show()?;
        widgets_visible.set(true);
        send(Cmd::WidgetsVisible(true));
    }
    if !widget_only {
        ui.show()?;
    }
    slint::run_event_loop()?;
    // Let the sampler save app history before the process exits.
    let _ = quit.send(Cmd::Quit);
    let _ = sampler_thread.join();
    Ok(())
}

fn save_settings(ui: &MainWindow) {
    let prefs = settings::Settings {
        theme: ui.get_theme_choice(),
        refresh: ui.get_refresh_index(),
        start_page: ["processes", "performance", "details"]
            [ui.get_start_page().clamp(0, 2) as usize]
            .into(),
        always_on_top: ui.get_on_top(),
        app_history: ui.get_app_history(),
        desktop_widgets: ui.get_desktop_widgets(),
    };
    if let Err(e) = prefs.save() {
        ui.set_toast(format!("Couldn't save settings: {e}").into());
    }
}
fn save_snapshot(window: &slint::Window, path: &str) {
    match window.take_snapshot() {
        Ok(img) => {
            let mut ppm = format!("P6 {} {} 255\n", img.width(), img.height()).into_bytes();
            ppm.extend(img.as_slice().iter().flat_map(|p| [p.r, p.g, p.b]));
            std::fs::write(path, ppm).expect("write snapshot");
        }
        Err(e) => eprintln!("snapshot failed: {e}"),
    }
}

/// Messages from the UI to the sampler thread.
enum Cmd {
    /// Refresh interval in ms; 0 pauses.
    Interval(u64),
    Page(Page),
    /// Performance page tab: 0 disk, 1 network, 2 GPU.
    PerfTab(i32),
    Sort(SharedString),
    Filter(SharedString),
    Toggle(SharedString),
    /// (action name, row key), see actions::Action::parse.
    Action(SharedString, SharedString),
    Properties(SharedString),
    AffinityQuery(SharedString),
    /// Services page: false = system services, true = the user's.
    ServiceBus(bool),
    /// (start|stop|restart|enable|disable, unit name without .service)
    ServiceAct(SharedString, SharedString),
    /// A service action finished; reload unit-file states.
    ServicesChanged,
    StartupToggle(SharedString, bool),
    /// (lock|terminate, session id)
    SessionAct(SharedString, SharedString),
    History(bool),
    MainVisible(bool),
    WidgetsVisible(bool),
    ClearHistory,
    Quit,
}

/// Samples on a background thread and pushes updates into visible UI surfaces.
/// Per-process data is only collected while the main window needs it or history is enabled.
fn spawn_sampler(
    ui: slint::Weak<MainWindow>,
    widget_ui: slint::Weak<DesktopWidgets>,
    main_visible: bool,
    widgets_visible: bool,
) -> (mpsc::Sender<Cmd>, std::thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel::<Cmd>();
    let tx_back = tx.clone();
    let handle = std::thread::Builder::new()
        .name("sampler".into())
        .spawn(move || {
            let mut sampler = Sampler::new();
            let mut procs_sampler: Option<ProcessSampler> = None;
            let mut histories = bridge::Histories::default();
            let mut view = processes::View::default();
            let mut gpu_sampler = GpuSampler::new();
            let mut page = Page::Performance;
            let mut perf_tab = 0;
            let mut last: (Snapshot, Vec<Process>) = Default::default();
            let mut gpu_total = 0.0f32;
            let mut services: Option<Services> = None;
            let mut service_bus = Bus::System;
            let mut service_list: Vec<Service> = Vec::new();
            let mut logind: Option<Logind> = None;
            let history_path = app_usage::default_path();
            let mut usage_log: Option<AppUsageLog> = None;
            let mut last_save = std::time::Instant::now();
            let mut tick: u64 = 0;
            let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
            let mut interval = Some(Duration::from_millis(1000));
            // The first tick comes quickly so the window doesn't sit on placeholders.
            let mut wait = Some(Duration::from_millis(250));
            let mut main_visible = main_visible;
            let mut widgets_visible = widgets_visible;

            let push_rows = |view: &processes::View,
                             page: Page,
                             last: &(Snapshot, Vec<Process>),
                             gpu_total: f32| {
                let rows = view.build(page, &last.1, &last.0, gpu_total);
                ui.upgrade_in_event_loop(move |ui| processes::apply(&ui, rows))
                    .is_ok()
            };

            loop {
                let msg = match wait {
                    Some(d) => rx.recv_timeout(d),
                    None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                };
                let shows_procs =
                    |p: Page| matches!(p, Page::Processes | Page::Details | Page::Users);
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
                                if main_visible && p == Page::Startup {
                                    push_startup(&ui);
                                }
                                if main_visible && p == Page::Services {
                                    let svc = services.get_or_insert_with(Services::new);
                                    service_list =
                                        push_services(&ui, svc, service_bus, &view.filter);
                                }
                                if main_visible && p == Page::History {
                                    push_history(&ui, usage_log.as_ref());
                                }
                                // Sample right away so the table isn't empty for a whole interval.
                                if main_visible && shows_procs(p) {
                                    let ps = procs_sampler.get_or_insert_with(ProcessSampler::new);
                                    last.1 = ps.sample();
                                }
                            }
                            Cmd::PerfTab(t) => {
                                perf_tab = t;
                                continue;
                            }
                            Cmd::Sort(c) => view.set_sort(c),
                            Cmd::Filter(f) => {
                                view.filter = f.to_string();
                                if main_visible && page == Page::Services {
                                    let (rows, summary) =
                                        system_pages::service_rows(&service_list, &view.filter);
                                    let _ = ui.upgrade_in_event_loop(move |ui| {
                                        bridge::sync(ui.get_service_rows(), rows, |m| {
                                            ui.set_service_rows(m)
                                        });
                                        ui.set_service_summary(summary.into());
                                    });
                                    continue;
                                }
                            }
                            Cmd::ServiceBus(user) => {
                                service_bus = if user { Bus::User } else { Bus::System };
                                if main_visible {
                                    let svc = services.get_or_insert_with(Services::new);
                                    service_list =
                                        push_services(&ui, svc, service_bus, &view.filter);
                                }
                                continue;
                            }
                            Cmd::ServiceAct(verb, name) => {
                                service_action(&ui, &tx_back, service_bus, &verb, &name);
                                continue;
                            }
                            Cmd::ServicesChanged => {
                                if let Some(svc) = services.as_mut() {
                                    // Our own connection's cache is stale after another connection changed units.
                                    *svc = Services::new();
                                    if main_visible {
                                        service_list =
                                            push_services(&ui, svc, service_bus, &view.filter);
                                    }
                                }
                                continue;
                            }
                            Cmd::StartupToggle(id, on) => {
                                let msg = match taskmonitor_core::startup::set_enabled(&id, on) {
                                    Ok(()) => None,
                                    Err(e) => Some(format!("Couldn't change startup setting: {e}")),
                                };
                                if main_visible {
                                    push_startup(&ui);
                                    if let Some(msg) = msg {
                                        let _ = ui.upgrade_in_event_loop(move |ui| {
                                            ui.set_toast(msg.into())
                                        });
                                    }
                                }
                                continue;
                            }
                            Cmd::SessionAct(verb, key) => {
                                session_action(&ui, &verb, &key);
                                continue;
                            }
                            Cmd::History(on) => {
                                if on && usage_log.is_none() {
                                    usage_log = Some(AppUsageLog::load(&history_path));
                                } else if !on && let Some(log) = usage_log.take() {
                                    let _ = log.save(&history_path);
                                }
                                if main_visible {
                                    push_history(&ui, usage_log.as_ref());
                                }
                                continue;
                            }
                            Cmd::ClearHistory => {
                                if usage_log.is_some() {
                                    let log = AppUsageLog::default();
                                    let _ = log.save(&history_path);
                                    usage_log = Some(log);
                                }
                                if main_visible {
                                    push_history(&ui, usage_log.as_ref());
                                }
                                continue;
                            }
                            Cmd::MainVisible(visible) => {
                                if !visible {
                                    // `sample(false)` releases NVML immediately without waking NVIDIA.
                                    let _ = gpu_sampler.sample(false);
                                }
                                main_visible = visible;
                                if main_visible && page == Page::History {
                                    push_history(&ui, usage_log.as_ref());
                                }
                                continue;
                            }
                            Cmd::WidgetsVisible(visible) => {
                                widgets_visible = visible;
                                continue;
                            }
                            Cmd::Quit => {
                                if let Some(log) = &usage_log {
                                    let _ = log.save(&history_path);
                                }
                                break;
                            }
                            Cmd::Toggle(k) => view.toggle(k),
                            Cmd::Action(a, k) => {
                                run_action(&ui, &a, &k, last.1.clone());
                                continue;
                            }
                            Cmd::Properties(k) => {
                                show_properties(&ui, &k, &last.1);
                                continue;
                            }
                            Cmd::AffinityQuery(k) => {
                                show_affinity(&ui, &k, &last.1);
                                continue;
                            }
                            Cmd::Interval(_) => unreachable!(),
                        }
                        if main_visible
                            && shows_procs(page)
                            && !push_rows(&view, page, &last, gpu_total)
                        {
                            main_visible = false;
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                wait = interval;
                let snap = sampler.sample();
                if widgets_visible {
                    let update = widgets::Update::from_snapshot(&snap);
                    if widget_ui
                        .upgrade_in_event_loop(move |widget| update.apply(&widget))
                        .is_err()
                    {
                        widgets_visible = false;
                    }
                }
                // Keep DRM per-process rates aligned with history records while the main
                // window is hidden, but only request NVIDIA detail for the visible main view.
                let history_record_due = usage_log.is_some() && (tick + 1).is_multiple_of(5);
                let (gpus, gpu_per_pid) = if main_visible || history_record_due {
                    let gpu_detail = main_visible
                        && (page == Page::Processes
                            || (page == Page::Performance && perf_tab == 2));
                    gpu_sampler.sample(gpu_detail)
                } else {
                    Default::default()
                };
                gpu_total = gpus.iter().filter_map(|g| g.usage).fold(0.0, f32::max);
                if main_visible {
                    let update = bridge::build(&snap, &gpus, &mut histories);
                    if ui
                        .upgrade_in_event_loop(move |ui| bridge::apply(&ui, update))
                        .is_err()
                    {
                        main_visible = false;
                    }
                }
                last.0 = snap;
                tick += 1;
                // App history needs process samples even when no process page is open,
                // but every 5th tick is enough: it accumulates rates times elapsed time.
                let sample_procs = (main_visible && shows_procs(page)) || history_record_due;
                if sample_procs {
                    let ps = procs_sampler.get_or_insert_with(ProcessSampler::new);
                    last.1 = ps.sample();
                    for p in &mut last.1 {
                        p.gpu = gpu_per_pid.get(&p.pid).copied().unwrap_or(0.0);
                    }
                    if let Some(log) = usage_log.as_mut() {
                        log.record(&last.1, ps.last_interval, cpus);
                    }
                }
                if let Some(log) = &usage_log
                    && last_save.elapsed() > Duration::from_secs(60)
                {
                    let _ = log.save(&history_path);
                    last_save = std::time::Instant::now();
                }
                if main_visible && page == Page::History {
                    push_history(&ui, usage_log.as_ref());
                }
                if main_visible && shows_procs(page) && !push_rows(&view, page, &last, gpu_total) {
                    main_visible = false;
                }
                if main_visible && page == Page::Services {
                    let svc = services.get_or_insert_with(Services::new);
                    service_list = push_services(&ui, svc, service_bus, &view.filter);
                }
                if main_visible && page == Page::Users {
                    let sessions: Vec<Session> = logind
                        .get_or_insert_with(Logind::new)
                        .sessions()
                        .unwrap_or_default();
                    let rows = system_pages::user_rows(&sessions, &last.1, &last.0);
                    let _ = ui.upgrade_in_event_loop(move |ui| {
                        bridge::sync(ui.get_user_rows(), rows, |m| ui.set_user_rows(m))
                    });
                }
            }
        })
        .expect("spawn sampler thread");
    (tx, handle)
}

/// Runs an action on its own thread: a polkit password prompt may block for a while.
fn run_action(ui: &slint::Weak<MainWindow>, action: &str, key: &str, procs: Vec<Process>) {
    let Some(action) = actions::Action::parse(action) else {
        return;
    };
    let (ui, key) = (ui.clone(), key.to_owned());
    std::thread::spawn(move || {
        let msg = actions::perform(&action, &key, &procs);
        let _ = ui.upgrade_in_event_loop(move |ui| ui.set_toast(msg.into()));
    });
}

fn show_properties(ui: &slint::Weak<MainWindow>, key: &str, procs: &[Process]) {
    let Some(&pid) = actions::pids_for_key(key, procs).first() else {
        return;
    };
    let title = format!(
        "{} (PID {pid})",
        actions::name_for_key(&format!("pid:{pid}"), procs)
    );
    let facts: Vec<Metric> = taskmonitor_core::actions::info(pid)
        .into_iter()
        .map(|(label, value)| Metric {
            label: label.into(),
            value: value.into(),
            unit: "".into(),
        })
        .collect();
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_props_title(title.into());
        ui.set_props(ModelRc::new(VecModel::from(facts)));
        ui.set_props_open(true);
    });
}

fn show_affinity(ui: &slint::Weak<MainWindow>, key: &str, procs: &[Process]) {
    let Some(&pid) = actions::pids_for_key(key, procs).first() else {
        return;
    };
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let title = actions::name_for_key(key, procs);
    let result = taskmonitor_core::actions::affinity(pid, cpus);
    let _ = ui.upgrade_in_event_loop(move |ui| match result {
        Ok(mask) => {
            ui.set_affinity_title(title.into());
            ui.set_affinity_cpus(ModelRc::new(VecModel::from(mask)));
            ui.set_affinity_open(true);
        }
        Err(e) => ui.set_toast(format!("Can't read affinity of {title}: {}", e.desc()).into()),
    });
}

fn push_services(
    ui: &slint::Weak<MainWindow>,
    svc: &Services,
    bus: Bus,
    filter: &str,
) -> Vec<Service> {
    let list = match svc.list(bus) {
        Ok(list) => list,
        Err(e) => {
            let msg = format!("Can't read services: {e}");
            let _ = ui.upgrade_in_event_loop(move |ui| ui.set_service_summary(msg.into()));
            return Vec::new();
        }
    };
    let (rows, summary) = system_pages::service_rows(&list, filter);
    let _ = ui.upgrade_in_event_loop(move |ui| {
        bridge::sync(ui.get_service_rows(), rows, |m| ui.set_service_rows(m));
        ui.set_service_summary(summary.into());
    });
    list
}

fn push_startup(ui: &slint::Weak<MainWindow>) {
    let rows = system_pages::startup_rows(&taskmonitor_core::startup::list());
    let _ = ui.upgrade_in_event_loop(move |ui| {
        bridge::sync(ui.get_startup_rows(), rows, |m| ui.set_startup_rows(m))
    });
}

/// systemd may show a password prompt, so this runs on its own thread with its own connection.
fn service_action(
    ui: &slint::Weak<MainWindow>,
    done: &mpsc::Sender<Cmd>,
    bus: Bus,
    verb: &str,
    name: &str,
) {
    let (ui, done, verb, unit) = (
        ui.clone(),
        done.clone(),
        verb.to_owned(),
        format!("{name}.service"),
    );
    std::thread::spawn(move || {
        let msg = match Services::new().control(bus, &verb, &unit) {
            Ok(()) => {
                let past = match verb.as_str() {
                    "start" => "Started",
                    "stop" => "Stopped",
                    "restart" => "Restarted",
                    "enable" => "Enabled",
                    _ => "Disabled",
                };
                format!("{past} {unit}")
            }
            Err(e) => format!("Couldn't {verb} {unit}: {}", dbus_error(&e)),
        };
        let _ = done.send(Cmd::ServicesChanged);
        let _ = ui.upgrade_in_event_loop(move |ui| ui.set_toast(msg.into()));
    });
}

fn session_action(ui: &slint::Weak<MainWindow>, verb: &str, session: &str) {
    let (ui, verb, session) = (ui.clone(), verb.to_owned(), session.to_owned());
    std::thread::spawn(move || {
        let msg = match Logind::new().control(&verb, &session) {
            Ok(()) if verb == "lock" => format!("Locked session {session}"),
            Ok(()) => format!("Signing out session {session}"),
            Err(e) => format!("Couldn't change session {session}: {}", dbus_error(&e)),
        };
        let _ = ui.upgrade_in_event_loop(move |ui| ui.set_toast(msg.into()));
    });
}

/// The human part of a D-Bus error ("Access denied" rather than the error name).
fn dbus_error(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(_, Some(detail), _) => detail.clone(),
        other => other.to_string(),
    }
}

fn push_history(ui: &slint::Weak<MainWindow>, log: Option<&AppUsageLog>) {
    let (rows, since) = log.map_or_else(Default::default, |l| {
        (
            system_pages::history_rows(l),
            system_pages::since_text(l.since),
        )
    });
    let _ = ui.upgrade_in_event_loop(move |ui| {
        bridge::sync(ui.get_history_rows(), rows, |m| ui.set_history_rows(m));
        ui.set_history_since(since.into());
    });
}
