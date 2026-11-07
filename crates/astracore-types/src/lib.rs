use serde::Serialize;
use std::path::PathBuf;

pub mod capability;
pub use capability::{AccessStatus, Capability, DetectionStatus, SupportStatus};

/// An RGB triple in red, green, blue order. Backends convert to their own wire order.
pub type Rgb = [u8; 3];

/// A USB vendor/product pair. Sysfs exposes these as hexadecimal strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbId {
    pub vendor: u16,
    pub product: u16,
}

impl UsbId {
    pub const fn new(vendor: u16, product: u16) -> Self {
        Self { vendor, product }
    }

    /// Compare against sysfs hexadecimal IDs, ignoring case and an optional `0x` prefix.
    pub fn matches(&self, vendor: &str, product: &str) -> bool {
        let parse =
            |value: &str| u16::from_str_radix(value.trim().trim_start_matches("0x"), 16).ok();
        parse(vendor) == Some(self.vendor) && parse(product) == Some(self.product)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardInfo {
    pub vendor: String,
    pub model: String,
    pub bios_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SensorValue {
    Temperature { milli_celsius: i64 },
    FanSpeedRpm { rpm: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SensorReading {
    pub label: String,
    pub value: SensorValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HwmonDevice {
    pub sysfs_path: PathBuf,
    pub device_id: String,
    pub name: String,
    pub display_name: String,
    pub sensors: Vec<SensorReading>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PciDevice {
    pub address: String,
    pub vendor_id: String,
    pub device_id: String,
    pub class_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsbDevice {
    pub sysfs_id: String,
    pub vendor_id: String,
    pub product_id: String,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
}

impl UsbDevice {
    pub fn is(&self, id: UsbId) -> bool {
        id.matches(&self.vendor_id, &self.product_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HidDevice {
    pub device_id: String,
    pub vendor_id: String,
    pub product_id: String,
    pub name: String,
    pub driver: Option<String>,
    pub hidraw_path: Option<String>,
    pub report_descriptor_size: Option<usize>,
}

impl HidDevice {
    pub fn is(&self, id: UsbId) -> bool {
        id.matches(&self.vendor_id, &self.product_id)
    }
}

/// Ordered by path, then reason, so merged warning lists sort deterministically.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DiscoveryWarning {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveryReport<T> {
    pub devices: Vec<T>,
    pub warnings: Vec<DiscoveryWarning>,
}

impl<T> Default for DiscoveryReport<T> {
    fn default() -> Self {
        Self {
            devices: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

impl<T> DiscoveryReport<T> {
    pub fn sort_warnings(&mut self) {
        self.warnings.sort();
    }
}

/// Combine warnings from several discovery passes into one sorted list.
pub fn merge_warnings(
    lists: impl IntoIterator<Item = Vec<DiscoveryWarning>>,
) -> Vec<DiscoveryWarning> {
    let mut warnings: Vec<_> = lists.into_iter().flatten().collect();
    warnings.sort();
    warnings
}

#[cfg(test)]
mod tests {
    use super::UsbId;

    #[test]
    fn usb_id_matches_sysfs_spellings() {
        let id = UsbId::new(0x0b05, 0x1cd2);
        assert!(id.matches("0b05", "1cd2"));
        assert!(id.matches("0x0B05", "1CD2"));
        assert!(!id.matches("0b05", "1cd3"));
        assert!(!id.matches("bogus", "1cd2"));
    }
}
