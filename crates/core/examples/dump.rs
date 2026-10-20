//! Prints two samples a second apart, for cross-checking against top/free/iostat.
fn main() {
    let mut sampler = taskmonitor_core::Sampler::new();
    std::thread::sleep(std::time::Duration::from_secs(1));
    println!("{:#?}", sampler.sample());
}
