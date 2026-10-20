//! Lists services, startup entries and sessions from the live system.
use taskmonitor_core::services::{Bus, Services};

fn main() {
    let t = std::time::Instant::now();
    let svc = Services::new();
    for bus in [Bus::System, Bus::User] {
        match svc.list(bus) {
            Ok(list) => {
                let running = list.iter().filter(|s| s.sub == "running").count();
                println!("{bus:?}: {} services, {running} running, e.g. {:?}", list.len(), list.iter().find(|s| s.pid > 0));
            }
            Err(e) => println!("{bus:?}: {e}"),
        }
    }
    println!("services took {:.1} ms (first, uncached)", t.elapsed().as_secs_f64() * 1e3);
    let t = std::time::Instant::now();
    let n = svc.list(Bus::System).map_or(0, |l| l.len()) + svc.list(Bus::User).map_or(0, |l| l.len());
    println!("refresh of {n} services took {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
    for e in taskmonitor_core::startup::list() {
        println!("startup: {:<40} enabled={} system={}", e.name, e.enabled, e.system);
    }
    println!("{:?}", taskmonitor_core::users::Logind::new().sessions());
}
