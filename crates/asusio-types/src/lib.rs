use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardInfo {
    pub vendor: String,
    pub model: String,
    pub bios_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportStatus {
    NotDetected,
    Detected,
    Readable,
    Writable,
    Unverified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Temperature,
    FanRpm,
    FanPwm,
    CpuPower,
    GpuPower,
    Lighting,
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PciDevice {
    pub address: String,
    pub vendor_id: String,
    pub device_id: String,
    pub class_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsbDevice {
    pub sysfs_id: String,
    pub vendor_id: String,
    pub product_id: String,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HidDevice {
    pub device_id: String,
    pub vendor_id: String,
    pub product_id: String,
    pub name: String,
    pub driver: Option<String>,
    pub hidraw_path: Option<String>,
    pub report_descriptor_size: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
        self.warnings
            .sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.reason.cmp(&b.reason)));
    }
}
