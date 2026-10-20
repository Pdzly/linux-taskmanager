use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::cpu::{self, CpuTimes};
use crate::disk::{self, DiskCounters, DiskRates};
use crate::mem::{self, Memory};
use crate::{net, system};

/// Everything the Performance page shows, at one instant.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub cpu_model: String,
    /// Aggregate CPU busy fraction, `0.0..=1.0`.
    pub cpu_usage: f32,
    /// Per logical CPU busy fraction.
    pub core_usage: Vec<f32>,
    pub cpu_mhz: Option<f32>,
    pub processes: u32,
    pub threads: u32,
    pub memory: Memory,
    pub disks: Vec<Disk>,
    pub nets: Vec<Net>,
    pub uptime_secs: u64,
    pub load: [f32; 3],
}

#[derive(Clone, Debug, Default)]
pub struct Disk {
    pub name: String,
    pub model: String,
    pub ssd: bool,
    pub capacity: u64,
    pub rates: DiskRates,
}

#[derive(Clone, Debug, Default)]
pub struct Net {
    pub name: String,
    pub wireless: bool,
    pub up: bool,
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub rx_total: u64,
    pub tx_total: u64,
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
}

/// Static facts about a block device, looked up once. `None` means "not a physical disk".
type DiskInfo = Option<(String, bool, u64)>;

/// Reads system counters and turns consecutive readings into rates.
/// Keeps its buffers between calls so steady-state sampling barely allocates.
pub struct Sampler {
    buf: String,
    last: Instant,
    cpu_model: String,
    cpu_prev: Vec<CpuTimes>,
    cpu_now: Vec<CpuTimes>,
    freq_paths: Vec<PathBuf>,
    disk_info: HashMap<String, DiskInfo>,
    disk_prev: HashMap<String, DiskCounters>,
    net_info: HashMap<String, Option<bool>>,
    net_prev: HashMap<String, (u64, u64)>,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        let mut s = Self {
            buf: String::with_capacity(16 * 1024),
            last: Instant::now(),
            cpu_model: fs::read_to_string("/proc/cpuinfo")
                .ok()
                .and_then(|t| cpu::parse_model(&t))
                .unwrap_or_else(|| "Processor".into()),
            cpu_prev: Vec::new(),
            cpu_now: Vec::new(),
            freq_paths: Vec::new(),
            disk_info: HashMap::new(),
            disk_prev: HashMap::new(),
            net_info: HashMap::new(),
            net_prev: HashMap::new(),
        };
        // Prime the counters so the first real sample has something to diff against.
        s.sample();
        s
    }

    pub fn sample(&mut self) -> Snapshot {
        let now = Instant::now();
        let secs = now.duration_since(self.last).as_secs_f64();
        self.last = now;

        let mut snap = Snapshot { cpu_model: self.cpu_model.clone(), ..Default::default() };

        if read("/proc/stat", &mut self.buf) {
            cpu::parse_stat(&self.buf, &mut self.cpu_now);
            if self.cpu_prev.len() == self.cpu_now.len() {
                let usage = self.cpu_now.iter().zip(&self.cpu_prev).map(|(n, p)| n.usage_since(p));
                snap.core_usage = usage.collect();
                snap.cpu_usage = snap.core_usage.remove(0);
            } else {
                snap.core_usage = vec![0.0; self.cpu_now.len().saturating_sub(1)];
            }
            std::mem::swap(&mut self.cpu_prev, &mut self.cpu_now);
        }
        snap.cpu_mhz = self.average_mhz(snap.core_usage.len());

        if read("/proc/meminfo", &mut self.buf) {
            snap.memory = mem::parse_meminfo(&self.buf);
        }
        if read("/proc/uptime", &mut self.buf) {
            snap.uptime_secs = system::parse_uptime(&self.buf);
        }
        if read("/proc/loadavg", &mut self.buf) {
            (snap.load, snap.threads) = system::parse_loadavg(&self.buf);
        }
        snap.processes = count_processes();
        snap.disks = self.sample_disks(secs);
        snap.nets = self.sample_nets(secs);
        snap
    }

    /// Mean current frequency across logical CPUs, in MHz.
    fn average_mhz(&mut self, cores: usize) -> Option<f32> {
        if self.freq_paths.len() != cores {
            self.freq_paths = (0..cores)
                .map(|i| PathBuf::from(format!("/sys/devices/system/cpu/cpu{i}/cpufreq/scaling_cur_freq")))
                .collect();
        }
        let (mut sum, mut n) = (0u64, 0u32);
        for path in &self.freq_paths {
            if read(path, &mut self.buf)
                && let Ok(khz) = self.buf.trim().parse::<u64>()
            {
                sum += khz;
                n += 1;
            }
        }
        (n > 0).then(|| sum as f32 / n as f32 / 1000.0)
    }

    fn sample_disks(&mut self, secs: f64) -> Vec<Disk> {
        if !read("/proc/diskstats", &mut self.buf) {
            return Vec::new();
        }
        let mut disks = Vec::new();
        let (info, prev) = (&mut self.disk_info, &mut self.disk_prev);
        disk::parse_diskstats(&self.buf, |name, counters| {
            let Some((model, ssd, capacity)) = info.entry(name.to_owned()).or_insert_with(|| disk_info(name)) else {
                return;
            };
            let rates = prev.get(name).map(|p| counters.rates_since(p, secs)).unwrap_or_default();
            prev.insert(name.to_owned(), counters);
            disks.push(Disk {
                name: name.to_owned(),
                model: model.clone(),
                ssd: *ssd,
                capacity: *capacity,
                rates,
            });
        });
        // Forget unplugged devices.
        prev.retain(|name, _| disks.iter().any(|d| &d.name == name));
        disks
    }

    fn sample_nets(&mut self, secs: f64) -> Vec<Net> {
        if !read("/proc/net/dev", &mut self.buf) {
            return Vec::new();
        }
        let mut nets = Vec::new();
        let (info, prev) = (&mut self.net_info, &mut self.net_prev);
        net::parse_net_dev(&self.buf, |name, rx, tx| {
            let Some(wireless) = *info.entry(name.to_owned()).or_insert_with(|| net_info(name)) else {
                return;
            };
            let rate = |now: u64, before: u64| {
                if secs > 0.0 { now.saturating_sub(before) as f64 / secs } else { 0.0 }
            };
            let (rx_bps, tx_bps) = prev.get(name).map_or((0.0, 0.0), |&(prx, ptx)| (rate(rx, prx), rate(tx, ptx)));
            prev.insert(name.to_owned(), (rx, tx));
            let up = fs::read_to_string(format!("/sys/class/net/{name}/operstate")).is_ok_and(|s| s.trim() == "up");
            nets.push(Net { name: name.to_owned(), wireless, up, rx_bps, tx_bps, rx_total: rx, tx_total: tx, ..Default::default() });
        });
        prev.retain(|name, _| nets.iter().any(|n| &n.name == name));
        fill_addresses(&mut nets);
        nets
    }
}

fn read(path: impl AsRef<Path>, buf: &mut String) -> bool {
    buf.clear();
    fs::File::open(path).and_then(|mut f| f.read_to_string(buf)).is_ok()
}

fn count_processes() -> u32 {
    let Ok(dir) = fs::read_dir("/proc") else { return 0 };
    dir.filter_map(Result::ok)
        .filter(|e| e.file_name().as_encoded_bytes().iter().all(u8::is_ascii_digit))
        .count() as u32
}

/// Physical disks have a `device` link; loop, ram, zram and device-mapper nodes don't.
/// Partitions never appear directly under /sys/block.
fn disk_info(name: &str) -> DiskInfo {
    let base = Path::new("/sys/block").join(name);
    if !base.join("device").exists() {
        return None;
    }
    let text = |p: &str| fs::read_to_string(base.join(p)).map(|s| s.trim().to_owned()).unwrap_or_default();
    let model = text("device/model");
    let ssd = text("queue/rotational") == "0";
    let capacity = text("size").parse::<u64>().unwrap_or(0) * 512;
    Some((if model.is_empty() { name.to_owned() } else { model }, ssd, capacity))
}

/// Physical NICs have a `device` link; lo, bridges, veth and VPN tunnels don't.
/// Returns whether the NIC is wireless.
fn net_info(name: &str) -> Option<bool> {
    let base = Path::new("/sys/class/net").join(name);
    base.join("device").exists().then(|| base.join("wireless").exists() || base.join("phy80211").exists())
}

fn fill_addresses(nets: &mut [Net]) {
    let Ok(addrs) = nix::ifaddrs::getifaddrs() else { return };
    for a in addrs {
        let Some(net) = nets.iter_mut().find(|n| n.name == a.interface_name) else { continue };
        let Some(addr) = a.address else { continue };
        let ip = if let Some(v4) = addr.as_sockaddr_in() {
            IpAddr::V4(v4.ip())
        } else if let Some(v6) = addr.as_sockaddr_in6() {
            IpAddr::V6(v6.ip())
        } else {
            continue;
        };
        match ip {
            IpAddr::V4(v4) if net.ipv4.is_none() => net.ipv4 = Some(v4.to_string()),
            // Prefer a global address over link-local (fe80::/10).
            IpAddr::V6(v6) if net.ipv6.is_none() || net.ipv6.as_deref().is_some_and(|s| s.starts_with("fe80")) => {
                net.ipv6 = Some(v6.to_string());
            }
            _ => {}
        }
    }
}
