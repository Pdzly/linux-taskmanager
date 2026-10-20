//! Human-readable numbers. Memory uses binary units labelled "GB", as Windows does;
//! disk capacity uses decimal units, as drive makers do.

const KIB: f64 = 1024.0;
const MIB: f64 = KIB * 1024.0;
const GIB: f64 = MIB * 1024.0;

/// One decimal below 10, none above: "3.4", "27".
fn num(v: f64) -> String {
    if v < 10.0 { format!("{v:.1}") } else { format!("{v:.0}") }
}

/// Bytes as a bare GiB figure: "9.1".
pub fn gib(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / GIB)
}

pub fn gib_unit(bytes: u64) -> String {
    format!("{} GB", gib(bytes))
}

/// Bytes per second as (value, unit), e.g. ("3.4", "MB/s").
pub fn byte_rate(bps: f64) -> (String, &'static str) {
    if bps < MIB {
        (num(bps / KIB), "KB/s")
    } else if bps < GIB {
        (num(bps / MIB), "MB/s")
    } else {
        (num(bps / GIB), "GB/s")
    }
}

/// Bytes per second shown as bits, the way network speeds are quoted: ("12", "Mb/s").
pub fn bit_rate(bytes_per_sec: f64) -> (String, &'static str) {
    let bits = bytes_per_sec * 8.0;
    if bits < 1e6 {
        (num(bits / 1e3), "Kb/s")
    } else if bits < 1e9 {
        (num(bits / 1e6), "Mb/s")
    } else {
        (num(bits / 1e9), "Gb/s")
    }
}

pub fn capacity(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e12 { format!("{:.1} TB", b / 1e12) } else { format!("{:.0} GB", b / 1e9) }
}

pub fn uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60);
    match (d, h) {
        (0, 0) => format!("{m}m"),
        (0, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}

pub fn ghz(mhz: Option<f32>) -> String {
    mhz.map_or_else(|| "—".into(), |m| format!("{:.2} GHz", m / 1000.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(byte_rate(3.5 * MIB), ("3.5".into(), "MB/s"));
        assert_eq!(byte_rate(512.0), ("0.5".into(), "KB/s"));
        assert_eq!(bit_rate(1_500_000.0), ("12".into(), "Mb/s"));
        assert_eq!(uptime(3 * 3600 + 5 * 60), "3h 5m");
        assert_eq!(uptime(2 * 86_400 + 7200), "2d 2h");
        assert_eq!(capacity(1_024_209_543_168), "1.0 TB");
        assert_eq!(gib(16 * 1024 * 1024 * 1024), "16.0");
    }
}
