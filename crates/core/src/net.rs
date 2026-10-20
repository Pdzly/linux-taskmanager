//! Network interface byte counters from `/proc/net/dev`.

/// Calls `f(interface, rx_bytes, tx_bytes)` for every interface.
pub fn parse_net_dev<'a>(text: &'a str, mut f: impl FnMut(&'a str, u64, u64)) {
    for line in text.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else { continue };
        let mut fields = rest.split_ascii_whitespace();
        let rx = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        // rx: bytes packets errs drop fifo frame compressed multicast, then tx bytes
        let tx = fields.nth(7).and_then(|v| v.parse().ok()).unwrap_or(0);
        f(name.trim(), rx, tx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_net_dev() {
        let text = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo:  5000      50    0    0    0     0          0         0     5000      50    0    0    0     0       0          0
  wlo1: 123456   100    0    0    0     0          0         0    654321     90    0    0    0     0       0          0
";
        let mut seen = Vec::new();
        parse_net_dev(text, |n, rx, tx| seen.push((n, rx, tx)));
        assert_eq!(seen, vec![("lo", 5000, 5000), ("wlo1", 123456, 654321)]);
    }
}
