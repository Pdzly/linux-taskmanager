//! Prints GPUs and per-process GPU use: `cargo run -p taskmonitor-core --example gpus [detail]`.
fn main() {
    let detail = std::env::args().any(|a| a == "detail");
    let mut s = taskmonitor_core::GpuSampler::new();
    s.sample(detail);
    std::thread::sleep(std::time::Duration::from_secs(1));
    s.sample(detail);
    std::thread::sleep(std::time::Duration::from_secs(1));
    let t = std::time::Instant::now();
    let (gpus, per_pid) = s.sample(detail);
    let took = t.elapsed();
    for g in &gpus {
        println!("{g:?}");
    }
    let mut top: Vec<_> = per_pid.into_iter().filter(|(_, u)| *u > 0.0).collect();
    top.sort_by(|a, b| b.1.total_cmp(&a.1));
    println!("busiest pids: {:?}", &top[..top.len().min(5)]);
    println!("tick took {:.2} ms", took.as_secs_f64() * 1e3);
}
