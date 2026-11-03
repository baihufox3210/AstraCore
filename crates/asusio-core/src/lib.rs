//! Reusable orchestration over discovery and native backends.
pub mod detect;
pub mod lighting;
pub mod registry;

pub use detect::detect_capabilities;
pub use registry::CapabilityRegistry;
