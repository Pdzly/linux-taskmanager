//! Lists processes by CPU after a 1 s sample, and times a process sampling tick.
fn main() {
    let mut s = taskmonitor_core::ProcessSampler::new();
    s.sample();
    std::thread::sleep(std::time::Duration::from_secs(1));
    let t = std::time::Instant::now();
    let mut procs = s.sample();
    let took = t.elapsed();
    procs.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
    for p in procs.iter().take(12) {
        println!("{:>7} {:<22} {:>5.1}% {:>8.1} MB  {:<8} {:?}", p.pid, p.name, p.cpu * 100.0, p.memory as f64 / 1048576.0, p.user, p.group);
    }
    let apps = procs.iter().filter(|p| matches!(p.group, taskmonitor_core::Group::App(_))).count();
    println!("{} processes ({apps} in apps), tick took {:.2} ms", procs.len(), took.as_secs_f64() * 1e3);
}
