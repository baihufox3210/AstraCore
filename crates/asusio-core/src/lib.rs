//! Reusable orchestration over discovery and native backends.
pub mod cpu_profile;
pub mod detect;
pub mod fan_curve;
pub mod lighting;
mod persist;
pub mod registry;
pub mod service;
pub mod telemetry;

pub use detect::detect_capabilities;
pub use registry::CapabilityRegistry;
