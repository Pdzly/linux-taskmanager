//! GPUs: Intel and AMD through sysfs and DRM `fdinfo` (per-client engine time, no root
//! needed); NVIDIA through NVML. NVML is only opened while detail is wanted and the card
//! is awake, and dropped otherwise, because an open NVML session can keep a laptop dGPU
//! from powering down.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendor {
    Intel,
    Amd,
    Nvidia,
    Other,
}

#[derive(Clone, Debug)]
pub struct Gpu {
    /// PCI address, e.g. "0000:01:00.0".
    pub slot: String,
    pub name: String,
    pub vendor: Vendor,
    pub driver: String,
    /// Runtime-suspended to save power; nothing is queried while asleep.
    pub asleep: bool,
    /// Busiest engine, `0.0..=1.0`.
    pub usage: Option<f32>,
    pub mem_used: Option<u64>,
    pub mem_total: Option<u64>,
    pub temp_c: Option<f32>,
    pub freq_mhz: Option<u32>,
    pub power_w: Option<f32>,
}

struct Card {
    slot: String,
    name: String,
    vendor: Vendor,
    driver: String,
    /// /sys/class/drm/cardN
    drm: PathBuf,
    /// /sys/class/drm/cardN/device
    dev: PathBuf,
}

pub struct GpuSampler {
    cards: Vec<Card>,
    nvml: Option<Nvml>,
    nvml_ts: u64,
    clients: Clients,
    last: Instant,
}

impl Default for GpuSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuSampler {
    pub fn new() -> Self {
        Self { cards: discover(), nvml: None, nvml_ts: 0, clients: Clients::default(), last: Instant::now() }
    }

    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    /// Returns every GPU plus per-process usage (`pid -> 0.0..=1.0`, busiest GPU engine).
    /// With `detail` off, NVIDIA cards are not queried at all.
    pub fn sample(&mut self, detail: bool) -> (Vec<Gpu>, HashMap<u32, f32>) {
        let now = Instant::now();
        let secs = now.duration_since(self.last).as_secs_f64();
        self.last = now;

        let (device_usage, mut per_pid) = self.clients.sample(secs);
        let any_nvidia_awake = self.cards.iter().any(|c| c.vendor == Vendor::Nvidia && !asleep(&c.dev));
        if !(detail && any_nvidia_awake) {
            self.nvml = None;
        }

        let mut gpus = Vec::with_capacity(self.cards.len());
        for card in &self.cards {
            let mut gpu = Gpu {
                slot: card.slot.clone(),
                name: card.name.clone(),
                vendor: card.vendor,
                driver: card.driver.clone(),
                asleep: asleep(&card.dev),
                usage: None,
                mem_used: None,
                mem_total: None,
                temp_c: None,
                freq_mhz: None,
                power_w: None,
            };
            if !gpu.asleep {
                match card.vendor {
                    Vendor::Nvidia if detail => {
                        if self.nvml.is_none() {
                            self.nvml = Nvml::init().ok();
                        }
                        if let Some(nvml) = &self.nvml {
                            self.nvml_ts = nvidia(nvml, card, &mut gpu, &mut per_pid, self.nvml_ts);
                        }
                    }
                    Vendor::Nvidia => {}
                    _ => sysfs(card, &mut gpu, device_usage.get(&card.slot).copied()),
                }
            }
            gpus.push(gpu);
        }
        (gpus, per_pid)
    }
}

fn asleep(dev: &Path) -> bool {
    fs::read_to_string(dev.join("power/runtime_status")).is_ok_and(|s| s.trim() == "suspended")
}

/// Intel and AMD: frequency, memory and temperature from sysfs; usage from fdinfo
/// (AMD's own busy counter wins when present).
fn sysfs(card: &Card, gpu: &mut Gpu, fdinfo_usage: Option<f32>) {
    let num = |p: &Path| fs::read_to_string(p).ok().and_then(|s| s.trim().parse::<u64>().ok());
    gpu.usage = num(&card.dev.join("gpu_busy_percent")).map(|p| p as f32 / 100.0).or(fdinfo_usage).or(Some(0.0));
    gpu.freq_mhz = num(&card.drm.join("gt_act_freq_mhz"))
        .or_else(|| num(&card.dev.join("tile0/gt0/freq0/act_freq")))
        // 0 while the GPU sits in its deepest idle state (RC6); show that as unknown.
        .filter(|&f| f > 0)
        .map(|f| f as u32);
    gpu.mem_used = num(&card.dev.join("mem_info_vram_used"));
    gpu.mem_total = num(&card.dev.join("mem_info_vram_total"));
    if let Ok(dir) = fs::read_dir(card.dev.join("hwmon")) {
        for hw in dir.flatten() {
            if let Some(t) = num(&hw.path().join("temp1_input")) {
                gpu.temp_c = Some(t as f32 / 1000.0);
            }
            if let Some(p) = num(&hw.path().join("power1_average")).or_else(|| num(&hw.path().join("power1_input"))) {
                gpu.power_w = Some(p as f32 / 1e6);
            }
        }
    }
}

/// Fills `gpu` from NVML; returns the newest per-process sample timestamp.
fn nvidia(nvml: &Nvml, card: &Card, gpu: &mut Gpu, per_pid: &mut HashMap<u32, f32>, since: u64) -> u64 {
    // NVML spells the PCI domain with 8 digits ("00000000:01:00.0").
    let bus = format!("0000{}", card.slot);
    let Ok(dev) = nvml.device_by_pci_bus_id(bus.as_str()).or_else(|_| nvml.device_by_pci_bus_id(card.slot.as_str()))
    else {
        return since;
    };
    if let Ok(name) = dev.name() {
        gpu.name = name;
    }
    gpu.usage = dev.utilization_rates().ok().map(|u| u.gpu as f32 / 100.0);
    if let Ok(m) = dev.memory_info() {
        gpu.mem_used = Some(m.used);
        gpu.mem_total = Some(m.total);
    }
    gpu.temp_c = dev.temperature(TemperatureSensor::Gpu).ok().map(|t| t as f32);
    gpu.freq_mhz = dev.clock_info(Clock::Graphics).ok();
    gpu.power_w = dev.power_usage().ok().map(|mw| mw as f32 / 1000.0);

    let mut newest = since;
    if let Ok(samples) = dev.process_utilization_stats(since) {
        for s in samples {
            newest = newest.max(s.timestamp);
            let u = (s.sm_util.max(s.enc_util).max(s.dec_util) as f32 / 100.0).clamp(0.0, 1.0);
            let e = per_pid.entry(s.pid).or_default();
            *e = e.max(u);
        }
    }
    newest
}

/// Finds DRM devices once at startup.
fn discover() -> Vec<Card> {
    let Ok(dir) = fs::read_dir("/sys/class/drm") else { return Vec::new() };
    let mut cards: Vec<Card> = Vec::new();
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // cardN only (not connectors like card1-eDP-1, not renderD*).
        if !name.strip_prefix("card").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
            continue;
        }
        let drm = entry.path();
        let dev = drm.join("device");
        let Ok(slot) = fs::canonicalize(&dev).map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
        else {
            continue;
        };
        if cards.iter().any(|c| c.slot == slot) {
            continue;
        }
        let hex = |f: &str| {
            fs::read_to_string(dev.join(f)).ok().and_then(|s| u16::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        };
        let (Some(vendor_id), Some(device_id)) = (hex("vendor"), hex("device")) else { continue };
        let vendor = match vendor_id {
            0x8086 => Vendor::Intel,
            0x1002 => Vendor::Amd,
            0x10de => Vendor::Nvidia,
            _ => Vendor::Other,
        };
        let driver = fs::read_link(dev.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let name = pci_name(vendor_id, device_id).unwrap_or_else(|| format!("GPU {vendor_id:04x}:{device_id:04x}"));
        cards.push(Card { slot, name, vendor, driver, drm, dev });
    }
    cards.sort_by(|a, b| a.slot.cmp(&b.slot));
    cards
}

/// Marketing name from the PCI ID database: the bracketed part if present
/// ("GA106M [GeForce RTX 3060 Mobile / Max-Q]" -> "NVIDIA GeForce RTX 3060 Mobile / Max-Q").
fn pci_name(vendor: u16, device: u16) -> Option<String> {
    let db = ["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids"].iter().find_map(|p| fs::read_to_string(p).ok())?;
    parse_pci_name(&db, vendor, device)
}

pub fn parse_pci_name(db: &str, vendor: u16, device: u16) -> Option<String> {
    let vendor_tag = format!("{vendor:04x}  ");
    let device_tag = format!("\t{device:04x}  ");
    let mut lines = db.lines().skip_while(|l| !l.starts_with(&vendor_tag));
    let vendor_name = lines.next()?[6..].trim();
    let short_vendor = match vendor {
        0x8086 => "Intel",
        0x1002 => "AMD",
        0x10de => "NVIDIA",
        _ => vendor_name,
    };
    let line = lines.take_while(|l| l.starts_with('\t') || l.starts_with('#')).find(|l| l.starts_with(&device_tag))?;
    let model = line[device_tag.len()..].trim();
    let model = match (model.find('['), model.rfind(']')) {
        (Some(a), Some(b)) if b > a => &model[a + 1..b],
        _ => model,
    };
    Some(format!("{short_vendor} {model}"))
}

/// Engine counters of one DRM client, from one `fdinfo` file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClientInfo {
    pub pdev: String,
    pub client_id: u64,
    /// (engine, busy, total) — busy in ns (total 0) or GPU cycles with a cycle total (xe).
    pub engines: Vec<(String, u64, u64)>,
    pub capacity: HashMap<String, u32>,
}

/// Parses DRM `fdinfo` (Documentation/gpu/drm-usage-stats.rst). None if it isn't a DRM fd.
pub fn parse_fdinfo(text: &str) -> Option<ClientInfo> {
    let mut c = ClientInfo::default();
    let mut cycles: HashMap<String, u64> = HashMap::new();
    let mut totals: HashMap<String, u64> = HashMap::new();
    let mut is_drm = false;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        let first = value.split_whitespace().next().unwrap_or("");
        if key == "drm-driver" {
            is_drm = true;
        } else if key == "drm-pdev" {
            c.pdev = value.to_owned();
        } else if key == "drm-client-id" {
            c.client_id = first.parse().unwrap_or(0);
        } else if let Some(engine) = key.strip_prefix("drm-engine-capacity-") {
            c.capacity.insert(engine.to_owned(), first.parse().unwrap_or(1));
        } else if let Some(engine) = key.strip_prefix("drm-engine-") {
            c.engines.push((engine.to_owned(), first.parse().unwrap_or(0), 0));
        } else if let Some(engine) = key.strip_prefix("drm-total-cycles-") {
            totals.insert(engine.to_owned(), first.parse().unwrap_or(0));
        } else if let Some(engine) = key.strip_prefix("drm-cycles-") {
            cycles.insert(engine.to_owned(), first.parse().unwrap_or(0));
        }
    }
    for (engine, busy) in cycles {
        c.engines.push((engine.clone(), busy, totals.get(&engine).copied().unwrap_or(0)));
    }
    is_drm.then_some(c)
}

/// Tracks DRM clients of the current user's processes through `/proc/[pid]/fdinfo`.
/// Which fds are DRM devices is rescanned every few seconds; in between, only the
/// known fdinfo files are read.
#[derive(Default)]
struct Clients {
    fds: HashMap<u32, Vec<String>>,
    scanned: Option<Instant>,
    prev: HashMap<(String, u64), ClientInfo>,
}

const RESCAN: Duration = Duration::from_secs(5);

impl Clients {
    /// Returns (usage per PCI slot, usage per pid).
    fn sample(&mut self, secs: f64) -> (HashMap<String, f32>, HashMap<u32, f32>) {
        if self.scanned.is_none_or(|t| t.elapsed() > RESCAN) {
            self.rescan();
        }
        let mut now: HashMap<(String, u64), (u32, ClientInfo)> = HashMap::new();
        for (pid, fds) in &mut self.fds {
            // Drop fds that have no usage stats (e.g. NVIDIA's) or duplicate a client
            // already read through another fd; reading fdinfo costs the driver real work.
            fds.retain(|fd| {
                let Ok(text) = fs::read_to_string(format!("/proc/{pid}/fdinfo/{fd}")) else { return false };
                let Some(c) = parse_fdinfo(&text) else { return false };
                let key = (c.pdev.clone(), c.client_id);
                if now.contains_key(&key) {
                    return false;
                }
                now.insert(key, (*pid, c));
                true
            });
        }

        let mut device_engines: HashMap<(String, String), f32> = HashMap::new();
        let mut per_pid: HashMap<u32, f32> = HashMap::new();
        for (key, (pid, c)) in &now {
            let Some(prev) = self.prev.get(key) else { continue };
            let mut busiest = 0.0f32;
            for (engine, busy, total) in &c.engines {
                let Some((_, pbusy, ptotal)) = prev.engines.iter().find(|(e, ..)| e == engine) else { continue };
                let cap = c.capacity.get(engine).copied().unwrap_or(1).max(1) as f64;
                let d = busy.saturating_sub(*pbusy) as f64;
                let util = if *total > 0 {
                    let dt = total.saturating_sub(*ptotal) as f64;
                    if dt > 0.0 { d / dt / cap } else { 0.0 }
                } else if secs > 0.0 {
                    d / (secs * 1e9 * cap)
                } else {
                    0.0
                };
                let util = util.clamp(0.0, 1.0) as f32;
                busiest = busiest.max(util);
                *device_engines.entry((c.pdev.clone(), engine.clone())).or_default() += util;
            }
            let e = per_pid.entry(*pid).or_default();
            *e = (*e + busiest).min(1.0);
        }
        let mut per_device: HashMap<String, f32> = HashMap::new();
        for ((pdev, _), util) in device_engines {
            let e = per_device.entry(pdev).or_default();
            *e = e.max(util.min(1.0));
        }
        self.prev = now.into_iter().map(|(k, (_, c))| (k, c)).collect();
        (per_device, per_pid)
    }

    /// Finds which fds of our own processes point at /dev/dri.
    fn rescan(&mut self) {
        self.scanned = Some(Instant::now());
        self.fds.clear();
        let uid = nix::unistd::getuid().as_raw();
        let Ok(dir) = fs::read_dir("/proc") else { return };
        for entry in dir.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            if entry.metadata().map_or(true, |m| m.uid() != uid) {
                continue;
            }
            let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
            let drm: Vec<String> = fds
                .flatten()
                .filter(|fd| fs::read_link(fd.path()).is_ok_and(|t| t.starts_with("/dev/dri/")))
                .map(|fd| fd.file_name().to_string_lossy().into_owned())
                .collect();
            if !drm.is_empty() {
                self.fds.insert(pid, drm);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_i915_and_xe_fdinfo() {
        let i915 = "pos:\t0\nflags:\t02100002\ndrm-driver:\ti915\ndrm-client-id:\t28\ndrm-pdev:\t0000:00:02.0\n\
                    drm-engine-render:\t1500 ns\ndrm-engine-video:\t0 ns\ndrm-engine-capacity-video:\t2\n";
        let c = parse_fdinfo(i915).unwrap();
        assert_eq!((c.pdev.as_str(), c.client_id), ("0000:00:02.0", 28));
        assert!(c.engines.contains(&("render".into(), 1500, 0)));
        assert_eq!(c.capacity["video"], 2);

        let xe = "drm-driver:\txe\ndrm-client-id:\t5\ndrm-pdev:\t0000:00:02.0\ndrm-cycles-rcs:\t100\ndrm-total-cycles-rcs:\t1000\n";
        assert_eq!(parse_fdinfo(xe).unwrap().engines, [("rcs".into(), 100, 1000)]);

        assert!(parse_fdinfo("pos:\t0\nflags:\t0\n").is_none());
    }

    #[test]
    fn names_from_pci_ids() {
        let db = "# comment\n10de  NVIDIA Corporation\n\t2560  GA106M [GeForce RTX 3060 Mobile / Max-Q]\n\t\t1043 1e11  Sub\n\
                  8086  Intel Corporation\n\t46a6  Alder Lake-P GT2 [Iris Xe Graphics]\n";
        assert_eq!(parse_pci_name(db, 0x10de, 0x2560).unwrap(), "NVIDIA GeForce RTX 3060 Mobile / Max-Q");
        assert_eq!(parse_pci_name(db, 0x8086, 0x46a6).unwrap(), "Intel Iris Xe Graphics");
        assert!(parse_pci_name(db, 0x8086, 0xffff).is_none());
    }
}
