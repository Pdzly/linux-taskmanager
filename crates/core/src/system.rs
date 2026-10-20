//! Whole-system figures: uptime and load.

/// Seconds since boot, from `/proc/uptime`.
pub fn parse_uptime(text: &str) -> u64 {
    text.split_ascii_whitespace()
        .next()
        .and_then(|v| v.parse::<f64>().ok())
        .map_or(0, |s| s as u64)
}

/// `/proc/loadavg`: 1/5/15-minute load and the total number of threads.
pub fn parse_loadavg(text: &str) -> ([f32; 3], u32) {
    let mut f = text.split_ascii_whitespace();
    let mut load = [0.0; 3];
    for slot in &mut load {
        *slot = f.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    }
    // "running/total" scheduling entities; total counts every thread.
    let threads = f
        .next()
        .and_then(|v| v.split_once('/'))
        .and_then(|(_, total)| total.parse().ok())
        .unwrap_or(0);
    (load, threads)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_uptime_and_loadavg() {
        assert_eq!(parse_uptime("12345.67 99999.00\n"), 12345);
        let (load, threads) = parse_loadavg("0.52 0.58 0.61 2/1234 56789\n");
        assert_eq!(load, [0.52, 0.58, 0.61]);
        assert_eq!(threads, 1234);
    }
}
