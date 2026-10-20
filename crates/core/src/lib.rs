//! System data collection for TaskMonitor. No UI dependencies.
//!
//! Parsers take `&str` so they can be tested against captured `/proc` text;
//! [`Sampler`] does the file reading and turns counters into rates.

pub mod cpu;
pub mod disk;
pub mod history;
pub mod mem;
pub mod net;
mod sampler;
pub mod system;

pub use history::History;
pub use sampler::{Disk, Net, Sampler, Snapshot};
