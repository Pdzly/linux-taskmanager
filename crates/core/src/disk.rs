//! Block device counters from `/proc/diskstats`.

/// diskstats always counts in 512-byte sectors, whatever the device's real sector size.
const SECTOR: u64 = 512;

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct DiskCounters {
    pub reads: u64,
    pub sectors_read: u64,
    pub read_ms: u64,
    pub writes: u64,
    pub sectors_written: u64,
    pub write_ms: u64,
    /// Milliseconds the device had at least one request in flight.
    pub io_ms: u64,
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct DiskRates {
    pub read_bps: f64,
    pub write_bps: f64,
    /// Share of wall time the disk was busy, `0.0..=1.0` (Windows' "Active time").
    pub active: f32,
    /// Average time per completed request, in ms (Windows' "Average response time").
    pub response_ms: f32,
}

impl DiskCounters {
    pub fn rates_since(&self, prev: &DiskCounters, secs: f64) -> DiskRates {
        if secs <= 0.0 {
            return DiskRates::default();
        }
        let d = |a: u64, b: u64| a.saturating_sub(b);
        let ops = d(self.reads, prev.reads) + d(self.writes, prev.writes);
        let wait_ms = d(self.read_ms, prev.read_ms) + d(self.write_ms, prev.write_ms);
        DiskRates {
            read_bps: (d(self.sectors_read, prev.sectors_read) * SECTOR) as f64 / secs,
            write_bps: (d(self.sectors_written, prev.sectors_written) * SECTOR) as f64 / secs,
            active: (d(self.io_ms, prev.io_ms) as f64 / (secs * 1000.0)).clamp(0.0, 1.0) as f32,
            response_ms: if ops == 0 { 0.0 } else { wait_ms as f32 / ops as f32 },
        }
    }
}

/// Calls `f(name, counters)` for every line of `/proc/diskstats`.
pub fn parse_diskstats<'a>(text: &'a str, mut f: impl FnMut(&'a str, DiskCounters)) {
    for line in text.lines() {
        let mut it = line.split_ascii_whitespace().skip(2);
        let Some(name) = it.next() else { continue };
        let mut v = [0u64; 10];
        for (slot, field) in v.iter_mut().zip(it) {
            *slot = field.parse().unwrap_or(0);
        }
        // v: reads, reads_merged, sectors_read, read_ms, writes, writes_merged,
        //    sectors_written, write_ms, in_flight, io_ms
        f(name, DiskCounters {
            reads: v[0],
            sectors_read: v[2],
            read_ms: v[3],
            writes: v[4],
            sectors_written: v[6],
            write_ms: v[7],
            io_ms: v[9],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_computes_rates() {
        let a_text = " 259       0 nvme0n1 1000 0 20000 500 2000 0 40000 1500 0 1000 0\n";
        let b_text = " 259       0 nvme0n1 1100 0 22048 560 2100 0 42048 1640 0 1500 0\n";
        let (mut a, mut b) = (None, None);
        parse_diskstats(a_text, |n, c| a = Some((n, c)));
        parse_diskstats(b_text, |n, c| b = Some((n, c)));
        let (name, a) = a.unwrap();
        let (_, b) = b.unwrap();
        assert_eq!(name, "nvme0n1");
        let r = b.rates_since(&a, 1.0);
        assert_eq!(r.read_bps, 2048.0 * 512.0);
        assert_eq!(r.write_bps, 2048.0 * 512.0);
        assert!((r.active - 0.5).abs() < 1e-6);
        assert!((r.response_ms - 1.0).abs() < 1e-6); // 200 ms over 200 ops
    }
}
