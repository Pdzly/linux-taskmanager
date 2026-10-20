//! Memory figures from `/proc/meminfo`, in bytes.

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Memory {
    pub total: u64,
    pub available: u64,
    pub free: u64,
    /// Buffers + page cache + reclaimable slab: what `free` calls buff/cache.
    pub cached: u64,
    pub swap_total: u64,
    pub swap_free: u64,
    pub committed: u64,
    pub commit_limit: u64,
}

impl Memory {
    /// In use, the way `free` (procps 4+) and Windows count it.
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub fn swap_used(&self) -> u64 {
        self.swap_total.saturating_sub(self.swap_free)
    }
}

pub fn parse_meminfo(text: &str) -> Memory {
    let mut m = Memory::default();
    let (mut buffers, mut page_cache, mut sreclaimable) = (0, 0, 0);
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else { continue };
        let kib: u64 = rest.split_ascii_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let bytes = kib * 1024;
        match key {
            "MemTotal" => m.total = bytes,
            "MemAvailable" => m.available = bytes,
            "MemFree" => m.free = bytes,
            "Buffers" => buffers = bytes,
            "Cached" => page_cache = bytes,
            "SReclaimable" => sreclaimable = bytes,
            "SwapTotal" => m.swap_total = bytes,
            "SwapFree" => m.swap_free = bytes,
            "Committed_AS" => m.committed = bytes,
            "CommitLimit" => m.commit_limit = bytes,
            _ => {}
        }
    }
    m.cached = buffers + page_cache + sreclaimable;
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_meminfo() {
        let text = "\
MemTotal:       16000000 kB
MemFree:         2000000 kB
MemAvailable:    9000000 kB
Buffers:          100000 kB
Cached:          5000000 kB
SwapTotal:       4000000 kB
SwapFree:        3000000 kB
SReclaimable:     400000 kB
Committed_AS:   12000000 kB
CommitLimit:    12000000 kB
";
        let m = parse_meminfo(text);
        assert_eq!(m.total, 16_000_000 * 1024);
        assert_eq!(m.used(), 7_000_000 * 1024);
        assert_eq!(m.cached, 5_500_000 * 1024);
        assert_eq!(m.swap_used(), 1_000_000 * 1024);
    }
}
