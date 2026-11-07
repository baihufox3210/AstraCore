//! Reusable orchestration over discovery and native backends.
pub mod board;
pub mod cpu_profile;
pub mod detect;
pub mod fan_curve;
pub mod fan_map;
pub mod fan_probe;
pub mod lighting;
mod persist;
pub mod registry;
pub mod service;
pub mod telemetry;

pub use detect::detect_capabilities;
pub use registry::CapabilityRegistry;
