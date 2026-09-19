//! JevNQL optimizer: bridges logical JevIR to physical JevIR.

pub mod physical;

pub use physical::{PhysicalConfig, physical_plan};
