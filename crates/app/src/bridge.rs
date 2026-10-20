//! Turns core `Snapshot`s into the structs the Slint UI binds to.
//!
//! `build` runs on the sampler thread (formatting, graph paths); `apply` runs on the
//! UI thread and only touches rows that changed, so Slint repaints as little as possible.

use std::collections::HashMap;
use std::fmt::Write;

use slint::{Model, ModelRc, VecModel};
use taskmonitor_core::{History, Snapshot};

use crate::format;
use crate::{CpuView, DeviceView, MainWindow, MemView, Metric, Summary, SysView};

/// Samples kept per graph.
const POINTS: usize = 60;

pub struct Histories {
    cpu: History,
    /// Per device name: (series A, series B), e.g. (read, write).
    devices: HashMap<String, (History, History)>,
}

impl Default for Histories {
    fn default() -> Self {
        Self { cpu: History::new(POINTS), devices: HashMap::new() }
    }
}

impl Histories {
    fn device(&mut self, key: String) -> &mut (History, History) {
        self.devices.entry(key).or_insert_with(|| (History::new(POINTS), History::new(POINTS)))
    }
}

/// A `DeviceView` minus its nested model, which can't cross threads.
pub struct Device {
    name: String,
    subtitle: String,
    metrics: Vec<Metric>,
    line_a: String,
    fill_a: String,
    line_b: String,
    scale: String,
}

impl Device {
    fn into_view(self) -> DeviceView {
        DeviceView {
            name: self.name.into(),
            subtitle: self.subtitle.into(),
            metrics: ModelRc::new(VecModel::from(self.metrics)),
            line_a: self.line_a.into(),
            fill_a: self.fill_a.into(),
            line_b: self.line_b.into(),
            scale: self.scale.into(),
        }
    }
}

pub struct Update {
    summary: Summary,
    cpu: CpuView,
    mem: MemView,
    sys: SysView,
    core_load: Vec<f32>,
    disks: Vec<Device>,
    nets: Vec<Device>,
}

pub fn build(s: &Snapshot, h: &mut Histories) -> Update {
    h.cpu.push(s.cpu_usage);
    let (cpu_line, cpu_fill) = graph(&h.cpu, 1.0);
    let m = &s.memory;
    let total = m.total.max(1) as f32;

    let disks = s
        .disks
        .iter()
        .map(|d| {
            let (read, write) = h.device(format!("disk:{}", d.name));
            read.push(d.rates.read_bps as f32);
            write.push(d.rates.write_bps as f32);
            // Never scale below 1 MB/s, so idle noise doesn't fill the graph.
            let max = read.max().max(write.max()).max(1024.0 * 1024.0);
            let (line_a, fill_a) = graph(read, max);
            let (line_b, _) = graph(write, max);
            let (rv, ru) = format::byte_rate(d.rates.read_bps);
            let (wv, wu) = format::byte_rate(d.rates.write_bps);
            let (mv, mu) = format::byte_rate(max as f64);
            Device {
                name: d.name.clone(),
                subtitle: format!(
                    "{} · {} · {}",
                    d.model,
                    if d.ssd { "SSD" } else { "HDD" },
                    format::capacity(d.capacity)
                ),
                metrics: vec![
                    metric("Read", rv, ru),
                    metric("Write", wv, wu),
                    metric("Active", format!("{:.0}", d.rates.active * 100.0), "%"),
                    metric("Response", format!("{:.1}", d.rates.response_ms), "ms"),
                ],
                line_a,
                fill_a,
                line_b,
                scale: format!("{mv} {mu}"),
            }
        })
        .collect();

    // Connected adapters first, so the default selection is the one in use.
    let mut nets: Vec<_> = s.nets.iter().collect();
    nets.sort_by_key(|n| !n.up);
    let nets = nets
        .into_iter()
        .map(|n| {
            let (rx, tx) = h.device(format!("net:{}", n.name));
            rx.push(n.rx_bps as f32);
            tx.push(n.tx_bps as f32);
            // Never scale below 1 Mb/s.
            let max = rx.max().max(tx.max()).max(125_000.0);
            let (line_a, fill_a) = graph(rx, max);
            let (line_b, _) = graph(tx, max);
            let (rv, ru) = format::bit_rate(n.rx_bps);
            let (tv, tu) = format::bit_rate(n.tx_bps);
            let (mv, mu) = format::bit_rate(max as f64);
            let kind = if n.wireless { "Wi-Fi" } else { "Ethernet" };
            let subtitle = match (&n.ipv4, n.up) {
                (_, false) => format!("{kind} · Disconnected"),
                (Some(ip), true) => format!("{kind} · {ip}"),
                (None, true) => kind.to_owned(),
            };
            Device {
                name: n.name.clone(),
                subtitle,
                metrics: vec![
                    metric("Receive", rv, ru),
                    metric("Send", tv, tu),
                    metric("Received", format::gib(n.rx_total), "GB"),
                ],
                line_a,
                fill_a,
                line_b,
                scale: format!("{mv} {mu}"),
            }
        })
        .collect();

    let uptime = format::uptime(s.uptime_secs);
    Update {
        summary: Summary {
            cpu: format!("{:.0}%", s.cpu_usage * 100.0).into(),
            ram: format!("{}/{} GB", format::gib(m.used()), format::gib(m.total)).into(),
            gpu: "—".into(),
            uptime: uptime.into(),
        },
        cpu: CpuView {
            model: s.cpu_model.as_str().into(),
            usage: format!("{:.0}", s.cpu_usage * 100.0).into(),
            speed: format::ghz(s.cpu_mhz).into(),
            processes: s.processes.to_string().into(),
            threads: s.threads.to_string().into(),
            graph_line: cpu_line.into(),
            graph_fill: cpu_fill.into(),
        },
        mem: MemView {
            total: format::gib_unit(m.total).into(),
            used: format::gib(m.used()).into(),
            available: format::gib_unit(m.available).into(),
            cached: format::gib_unit(m.cached).into(),
            swap: format::gib_unit(m.swap_used()).into(),
            used_frac: m.used() as f32 / total,
            cached_frac: m.cached as f32 / total,
        },
        sys: SysView {
            load1: format!("{:.2}", s.load[0]).into(),
            load_detail: format!("5 min {:.2} · 15 min {:.2}", s.load[1], s.load[2]).into(),
        },
        core_load: s.core_usage.clone(),
        disks,
        nets,
    }
}

/// Applies an update in a single frame. (Spreading it over one frame per card was
/// measured slower: each frame has a fixed cost that outweighs the smaller repaint.)
pub fn apply(ui: &MainWindow, u: Update) {
    ui.set_summary(u.summary);
    ui.set_cpu(u.cpu);
    ui.set_mem(u.mem);
    ui.set_sys(u.sys);
    sync(ui.get_core_load(), u.core_load, |m| ui.set_core_load(m));
    sync(ui.get_disks(), u.disks.into_iter().map(Device::into_view).collect(), |m| ui.set_disks(m));
    sync(ui.get_nets(), u.nets.into_iter().map(Device::into_view).collect(), |m| ui.set_nets(m));
}

/// Updates rows in place when the row count is unchanged; replaces the model otherwise.
fn sync<T: Clone + PartialEq + 'static>(model: ModelRc<T>, rows: Vec<T>, set: impl FnOnce(ModelRc<T>)) {
    if model.row_count() != rows.len() {
        set(ModelRc::new(VecModel::from(rows)));
        return;
    }
    for (i, row) in rows.into_iter().enumerate() {
        if model.row_data(i).as_ref() != Some(&row) {
            model.set_row_data(i, row);
        }
    }
}

fn metric(label: &str, value: impl Into<slint::SharedString>, unit: &str) -> Metric {
    Metric { label: label.into(), value: value.into(), unit: unit.into() }
}

/// SVG path commands (line, closed fill) for a history in a 0..100 box,
/// newest sample at the right edge.
fn graph(h: &History, max: f32) -> (String, String) {
    let n = h.len();
    if n < 2 || max <= 0.0 {
        return (String::new(), String::new());
    }
    let step = 100.0 / (h.capacity() - 1) as f32;
    let x0 = (h.capacity() - n) as f32 * step;
    let mut line = String::with_capacity(n * 16);
    for (i, v) in h.iter().enumerate() {
        let x = x0 + i as f32 * step;
        // Keep a small margin so a 0% or 100% line isn't clipped by the edge.
        let y = 98.0 - (v / max).clamp(0.0, 1.0) * 94.0;
        let _ = write!(line, "{} {x:.2} {y:.2} ", if i == 0 { 'M' } else { 'L' });
    }
    let fill = format!("{line}L 100 100 L {x0:.2} 100 Z");
    (line, fill)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_right_aligns_partial_history() {
        let mut h = History::new(3);
        h.push(0.0);
        h.push(1.0);
        let (line, fill) = graph(&h, 1.0);
        assert_eq!(line, "M 50.00 98.00 L 100.00 4.00 ");
        assert!(fill.ends_with("L 100 100 L 50.00 100 Z"));
    }
}
