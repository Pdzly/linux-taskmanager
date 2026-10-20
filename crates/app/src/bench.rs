//! Headless render benchmark: `TASKMONITOR_BENCH=1 cargo run --release -p taskmonitor`.
//! Renders the real UI into memory (no window system) and times each update frame,
//! so rendering cost can be measured without `perf`. Build with `--features profiling`
//! to also print where update frames spend their time.

use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, PlatformError};
use taskmonitor_core::Sampler;

use crate::{MainWindow, bridge};

const W: u32 = 1320;
const H: u32 = 860;

struct Headless {
    window: Rc<MinimalSoftwareWindow>,
    start: Instant,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }
}

struct Bench {
    ui: MainWindow,
    window: Rc<MinimalSoftwareWindow>,
    buf: Vec<PremultipliedRgbaColor>,
    sampler: Sampler,
    histories: bridge::Histories,
}

impl Bench {
    /// Applies one real sample and renders; returns (time, repainted pixels).
    fn tick(&mut self) -> (Duration, u64) {
        bridge::apply(&self.ui, bridge::build(&self.sampler.sample(), &[], &mut self.histories));
        self.render()
    }

    fn render(&mut self) -> (Duration, u64) {
        slint::platform::update_timers_and_animations();
        let t = Instant::now();
        let mut area = 0;
        let buf = &mut self.buf;
        self.window.draw_if_needed(|r| {
            let region = r.render(buf, W as usize);
            area = region.iter().map(|(_, s)| s.width as u64 * s.height as u64).sum();
        });
        (t.elapsed(), area)
    }
}

pub fn run() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(Headless { window: window.clone(), start: Instant::now() }))
        .expect("set headless platform");
    window.set_size(PhysicalSize::new(W, H));

    let ui = MainWindow::new().expect("create window");
    ui.set_user_name("bench".into());
    ui.show().expect("show");

    let mut b = Bench {
        ui,
        window,
        buf: vec![PremultipliedRgbaColor::default(); (W * H) as usize],
        sampler: Sampler::new(),
        histories: bridge::Histories::default(),
    };
    let pct = |area: u64| area as f64 * 100.0 / (W * H) as f64;

    let (t, area) = b.render();
    println!("first frame: {:.2} ms, {:.0}% of window", t.as_secs_f64() * 1e3, pct(area));

    let ticks = 40;
    let (mut total, mut total_area) = (Duration::ZERO, 0);
    for _ in 0..ticks {
        std::thread::sleep(Duration::from_millis(50));
        let (t, area) = b.tick();
        total += t;
        total_area += area;
    }
    println!(
        "update frame: {:.2} ms avg, {:.0}% of window repainted avg",
        total.as_secs_f64() * 1e3 / ticks as f64,
        pct(total_area / ticks)
    );

    #[cfg(feature = "profiling")]
    profile(&mut b);
}

/// Samples where update frames spend their time and prints the hottest functions.
#[cfg(feature = "profiling")]
fn profile(b: &mut Bench) {
    use std::collections::{HashMap, HashSet};

    let guard = pprof::ProfilerGuardBuilder::default().frequency(4000).build().expect("profiler");
    for _ in 0..300 {
        b.tick();
    }
    let report = guard.report().build().expect("report");
    let (mut inclusive, mut total) = (HashMap::<String, isize>::new(), 0);
    for (frames, &count) in &report.data {
        total += count;
        let mut seen = HashSet::new();
        for f in frames.frames.iter().flatten() {
            let name = f.name();
            let short: String = name.split("::{{closure}}").next().unwrap_or(&name).chars().take(110).collect();
            if seen.insert(short.clone()) {
                *inclusive.entry(short).or_default() += count;
            }
        }
    }
    let mut rows: Vec<_> = inclusive.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    println!("profile: {total} samples, inclusive % per function");
    for (name, n) in rows.iter().take(45) {
        println!("{:5.1}%  {name}", *n as f64 * 100.0 / total as f64);
    }
}
