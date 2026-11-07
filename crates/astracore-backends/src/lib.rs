//! Native device protocols and OS transports.
//!
//! Each backend validates the exact device identity and layout before any write.
//! HID backends take a discovered [`astracore_types::HidDevice`] plus its sysfs root
//! (normally [`astracore_discovery::sysfs::HID`]).
pub mod aura_hid;
pub mod cpu_pstate;
pub mod ene_smbus;
pub mod fan_hwmon;
pub mod lianli_ipc;
pub mod lianli_screen;
pub mod nvidia_nvml;
pub mod scope_keyboard;
mod util;

pub use astracore_types::Rgb;
