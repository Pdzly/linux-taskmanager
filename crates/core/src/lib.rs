//! System data collection for TaskMonitor. No UI dependencies.
//!
//! Parsers take `&str` so they can be tested against captured `/proc` text;
//! [`Sampler`] does the file reading and turns counters into rates.

pub mod actions;
pub mod apps;
pub mod cpu;
pub mod disk;
pub mod gpu;
pub mod history;
pub mod mem;
pub mod net;
pub mod process;
mod sampler;
pub mod system;

pub use gpu::{Gpu, GpuSampler, Vendor};
pub use history::History;
pub use process::{Group, Process, ProcessSampler};
pub use sampler::{Disk, Net, Sampler, Snapshot};
