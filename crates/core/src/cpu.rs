//! CPU counters from `/proc/stat` and identity from `/proc/cpuinfo`.

/// Cumulative jiffies for one CPU (or the aggregate) at one instant.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct CpuTimes {
    pub busy: u64,
    pub total: u64,
}

impl CpuTimes {
    /// Fraction of time spent busy since `prev`, in `0.0..=1.0`.
    pub fn usage_since(&self, prev: &CpuTimes) -> f32 {
        let total = self.total.saturating_sub(prev.total);
        if total == 0 {
            return 0.0;
        }
        let busy = self.busy.saturating_sub(prev.busy);
        (busy as f64 / total as f64).clamp(0.0, 1.0) as f32
    }
}

/// Parses the `cpu`/`cpuN` lines of `/proc/stat` into `out`.
/// `out[0]` is the aggregate; `out[1..]` are logical CPUs in file order.
pub fn parse_stat(text: &str, out: &mut Vec<CpuTimes>) {
    out.clear();
    for line in text.lines() {
        let mut fields = line.split_ascii_whitespace();
        match fields.next() {
            Some(label) if label.starts_with("cpu") => {}
            _ => break, // cpu lines always come first
        }
        // user nice system idle iowait irq softirq steal. Guest time is already
        // included in user/nice, so the later columns are deliberately ignored.
        let mut v = [0u64; 8];
        for (slot, field) in v.iter_mut().zip(fields) {
            *slot = field.parse().unwrap_or(0);
        }
        let total: u64 = v.iter().sum();
        let idle = v[3] + v[4];
        out.push(CpuTimes { busy: total - idle, total });
    }
}

/// First `model name` in `/proc/cpuinfo`, without trademark clutter.
pub fn parse_model(cpuinfo: &str) -> Option<String> {
    let line = cpuinfo.lines().find(|l| l.starts_with("model name"))?;
    let name = line.split_once(':')?.1;
    let cleaned = name.replace("(R)", "").replace("(TM)", "").replace(" CPU", "");
    Some(cleaned.split_whitespace().collect::<Vec<_>>().join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "\
cpu  100 0 50 800 50 0 0 0 0 0
cpu0 60 0 20 400 20 0 0 0 0 0
cpu1 40 0 30 400 30 0 0 0 0 0
intr 12345
";

    #[test]
    fn parses_aggregate_and_cores() {
        let mut out = Vec::new();
        parse_stat(STAT, &mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], CpuTimes { busy: 150, total: 1000 });
        assert_eq!(out[1], CpuTimes { busy: 80, total: 500 });
    }

    #[test]
    fn usage_is_delta_based() {
        let a = CpuTimes { busy: 100, total: 1000 };
        let b = CpuTimes { busy: 175, total: 1100 };
        assert!((b.usage_since(&a) - 0.75).abs() < 1e-6);
        assert_eq!(a.usage_since(&a), 0.0);
    }

    #[test]
    fn cleans_model_name() {
        let info = "processor\t: 0\nmodel name\t: 12th Gen Intel(R) Core(TM) i7-12700H\n";
        assert_eq!(parse_model(info).unwrap(), "12th Gen Intel Core i7-12700H");
    }
}
