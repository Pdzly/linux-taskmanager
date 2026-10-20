//! Times one sampler tick: `cargo run --release -p taskmonitor-core --example bench`.
fn main() {
    let mut s = taskmonitor_core::Sampler::new();
    let n = 500;
    let t = std::time::Instant::now();
    for _ in 0..n {
        std::hint::black_box(s.sample());
    }
    println!("{:.3} ms per sample", t.elapsed().as_secs_f64() * 1000.0 / n as f64);
}
