//! Desktop widget window setup and projection from the existing system snapshot.

use crate::format;
use taskmonitor_core::Snapshot;

/// Creates the independently movable widget panel.
///
/// Slint declares the preferred window level, while this hook realizes it
/// after the first frame for each actual winit window. The native window can
/// be recreated after hiding.
pub fn create() -> Result<crate::DesktopWidgets, slint::PlatformError> {
    use std::{cell::Cell, rc::Rc, time::Duration};

    use slint::{
        ComponentHandle, Timer,
        winit_030::{
            EventResult, WinitWindowAccessor,
            winit::{
                event::WindowEvent,
                window::{WindowId, WindowLevel},
            },
        },
    };

    let widget = crate::DesktopWidgets::new()?;
    let widget_weak = widget.as_weak();
    let level_applied_to = Rc::new(Cell::<Option<WindowId>>::new(None));
    let level_pending_for = Rc::new(Cell::<Option<WindowId>>::new(None));
    widget.window().on_winit_window_event({
        let level_applied_to = level_applied_to.clone();
        let level_pending_for = level_pending_for.clone();
        move |window, event| {
            if !matches!(event, WindowEvent::RedrawRequested) {
                return EventResult::Propagate;
            }
            let Some(id) = window.with_winit_window(|native| native.id()) else {
                return EventResult::Propagate;
            };
            if level_applied_to.get() == Some(id) || level_pending_for.get() == Some(id) {
                return EventResult::Propagate;
            }
            level_pending_for.set(Some(id));

            let widget_weak = widget_weak.clone();
            let level_applied_to = level_applied_to.clone();
            let level_pending_for = level_pending_for.clone();
            Timer::single_shot(Duration::ZERO, move || {
                if let Some(widget) = widget_weak.upgrade() {
                    widget.window().with_winit_window(|native| {
                        if native.id() == id {
                            native.set_window_level(WindowLevel::AlwaysOnTop);
                            level_applied_to.set(Some(id));
                        }
                    });
                }
                if level_pending_for.get() == Some(id) {
                    level_pending_for.set(None);
                }
            });
            EventResult::Propagate
        }
    });
    Ok(widget)
}

/// A one-tick projection of system-wide metrics for the desktop panel.
pub struct Update {
    cpu_value: String,
    cpu_detail: String,
    cpu_progress: f32,
    memory_value: String,
    memory_detail: String,
    memory_progress: f32,
    disk_read: String,
    disk_write: String,
    network_receive: String,
    network_send: String,
}

impl Update {
    /// Projects one sampler snapshot without retaining or cloning it.
    pub fn from_snapshot(snapshot: &Snapshot) -> Self {
        let cpu_progress = snapshot.cpu_usage.clamp(0.0, 1.0);
        let memory_used = snapshot.memory.used();
        let memory_total = snapshot.memory.total;
        let memory_progress = if memory_total == 0 {
            0.0
        } else {
            (memory_used as f32 / memory_total as f32).clamp(0.0, 1.0)
        };
        let (disk_read, disk_write) = snapshot.disks.iter().fold((0.0, 0.0), |(read, write), disk| {
            (read + disk.rates.read_bps, write + disk.rates.write_bps)
        });
        let (network_receive, network_send) = snapshot.nets.iter().fold((0.0, 0.0), |(receive, send), net| {
            (receive + net.rx_bps, send + net.tx_bps)
        });

        Self {
            cpu_value: format!("{:.0}%", cpu_progress * 100.0),
            cpu_detail: snapshot.cpu_model.clone(),
            cpu_progress,
            memory_value: if memory_total == 0 { "Unavailable".into() } else { format::gib_unit(memory_used) },
            memory_detail: if memory_total == 0 {
                "Memory total unavailable".into()
            } else {
                format!("of {}", format::gib_unit(memory_total))
            },
            memory_progress,
            disk_read: rate(disk_read, snapshot.disks.is_empty()),
            disk_write: rate(disk_write, snapshot.disks.is_empty()),
            network_receive: network_rate(network_receive, snapshot.nets.is_empty()),
            network_send: network_rate(network_send, snapshot.nets.is_empty()),
        }
    }

    /// Applies this owned projection to the generated Slint component.
    pub fn apply(self, widget: &crate::DesktopWidgets) {
        widget.set_cpu_value(self.cpu_value.into());
        widget.set_cpu_detail(self.cpu_detail.into());
        widget.set_cpu_progress(self.cpu_progress);
        widget.set_memory_value(self.memory_value.into());
        widget.set_memory_detail(self.memory_detail.into());
        widget.set_memory_progress(self.memory_progress);
        widget.set_disk_read(self.disk_read.into());
        widget.set_disk_write(self.disk_write.into());
        widget.set_network_receive(self.network_receive.into());
        widget.set_network_send(self.network_send.into());
    }
}

fn rate(bytes_per_second: f64, unavailable: bool) -> String {
    if unavailable {
        return "No disks".into();
    }
    let (value, unit) = format::byte_rate(bytes_per_second.max(0.0));
    format!("{value} {unit}")
}

fn network_rate(bytes_per_second: f64, unavailable: bool) -> String {
    if unavailable {
        return "No network".into();
    }
    let (value, unit) = format::bit_rate(bytes_per_second.max(0.0));
    format!("{value} {unit}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use taskmonitor_core::{Disk, Net};

    #[test]
    fn aggregates_all_device_rates_and_handles_unknown_memory_total() {
        let snapshot = Snapshot {
            disks: vec![
                Disk { rates: taskmonitor_core::disk::DiskRates { read_bps: 1024.0, write_bps: 2048.0, ..Default::default() }, ..Default::default() },
                Disk { rates: taskmonitor_core::disk::DiskRates { read_bps: 2048.0, write_bps: 1024.0, ..Default::default() }, ..Default::default() },
            ],
            nets: vec![
                Net { rx_bps: 125_000.0, tx_bps: 250_000.0, ..Default::default() },
                Net { rx_bps: 125_000.0, tx_bps: 125_000.0, ..Default::default() },
            ],
            ..Default::default()
        };

        let update = Update::from_snapshot(&snapshot);
        assert_eq!(update.disk_read, "3.0 KB/s");
        assert_eq!(update.disk_write, "3.0 KB/s");
        assert_eq!(update.network_receive, "2.0 Mb/s");
        assert_eq!(update.network_send, "3.0 Mb/s");
        assert_eq!(update.memory_value, "Unavailable");
        assert_eq!(update.memory_progress, 0.0);
    }
}
