use crate::CapabilityRegistry;
use asusio_types::{
    AccessStatus, Capability, DetectionStatus, HidDevice, HwmonDevice, SensorValue, UsbDevice,
};

fn parse_hex(value: &str) -> Option<u16> {
    u16::from_str_radix(value.trim_start_matches("0x"), 16).ok()
}

// Candidate match only: vendor/product IDs do not prove protocol support.
fn is_aura_candidate(vendor: &str, product: &str) -> bool {
    parse_hex(vendor) == Some(0x0b05) && parse_hex(product) == Some(0x1cd2)
}

fn verified_read_capability(id: &str, device_id: &str) -> Capability {
    let mut capability = Capability::new(id, Some(device_id.to_owned()));
    capability.detection = DetectionStatus::Verified;
    capability.read_access = AccessStatus::Accessible;
    capability.write_access = AccessStatus::NotApplicable;
    capability
}

/// Derive only claims supported by the supplied discovery snapshots.
pub fn detect_capabilities(
    hwmon: &[HwmonDevice],
    usb: &[UsbDevice],
    hid: &[HidDevice],
) -> CapabilityRegistry {
    let mut registry = CapabilityRegistry::new();

    for device in hwmon {
        let has_temperature = device
            .sensors
            .iter()
            .any(|sensor| matches!(sensor.value, SensorValue::Temperature { .. }));
        let has_fan = device
            .sensors
            .iter()
            .any(|sensor| matches!(sensor.value, SensorValue::FanSpeedRpm { .. }));

        if has_temperature {
            registry.register(verified_read_capability(
                "sensor.temperature.read",
                &device.device_id,
            ));
        }
        if has_fan {
            registry.register(verified_read_capability(
                "sensor.fan_rpm.read",
                &device.device_id,
            ));
        }
    }

    for device in usb {
        if is_aura_candidate(&device.vendor_id, &device.product_id) {
            let mut capability = Capability::new("asus.aura.usb", Some(device.sysfs_id.clone()));
            capability.detection = DetectionStatus::Detected;
            capability.detail = Some("vendor/product ID match; protocol unverified".to_owned());
            registry.register(capability);
        }
    }

    for device in hid {
        if !is_aura_candidate(&device.vendor_id, &device.product_id) {
            continue;
        }

        let mut hid_capability = Capability::new("asus.aura.hid", Some(device.device_id.clone()));
        hid_capability.detection = DetectionStatus::Detected;
        hid_capability.detail = Some("vendor/product ID match; protocol unverified".to_owned());
        registry.register(hid_capability);

        let mut rgb_capability =
            Capability::new("asus.aura.rgb.control", Some(device.device_id.clone()));
        rgb_capability.detail = Some("candidate HID device; RGB protocol not verified".to_owned());
        registry.register(rgb_capability);
    }

    registry
}

#[cfg(test)]
mod tests {
    use super::detect_capabilities;
    use asusio_types::{
        AccessStatus, DetectionStatus, HidDevice, HwmonDevice, SensorReading, SensorValue,
        SupportStatus, UsbDevice,
    };
    use std::path::PathBuf;

    #[test]
    fn reports_only_observed_sensor_reads() {
        let hwmon = HwmonDevice {
            sysfs_path: PathBuf::from("/sys/devices/example"),
            device_id: "pci:0000:01:00.0".to_owned(),
            name: "example".to_owned(),
            display_name: "example".to_owned(),
            sensors: vec![SensorReading {
                label: "CPU".to_owned(),
                value: SensorValue::Temperature {
                    milli_celsius: 42000,
                },
            }],
        };

        let hwmon = vec![hwmon];
        let usb: Vec<UsbDevice> = Vec::new();
        let hid: Vec<HidDevice> = Vec::new();
        let registry = detect_capabilities(hwmon.as_slice(), usb.as_slice(), hid.as_slice());
        let temperature = registry.find("sensor.temperature.read");
        assert_eq!(temperature.len(), 1);
        assert_eq!(temperature[0].detection, DetectionStatus::Verified);
        assert_eq!(temperature[0].read_access, AccessStatus::Accessible);
        assert_eq!(temperature[0].support, SupportStatus::Unknown);
        assert!(registry.find("sensor.fan_rpm.read").is_empty());
        assert!(registry.find("asus.aura.hid").is_empty());
    }

    #[test]
    fn candidate_id_does_not_claim_rgb_support() {
        let usb = UsbDevice {
            sysfs_id: "1-2".to_owned(),
            vendor_id: "0b05".to_owned(),
            product_id: "1cd2".to_owned(),
            manufacturer: None,
            product: None,
        };
        let hid = HidDevice {
            device_id: "0003:0B05:1CD2.0001".to_owned(),
            vendor_id: "0b05".to_owned(),
            product_id: "1cd2".to_owned(),
            name: "candidate".to_owned(),
            driver: None,
            hidraw_path: Some("/dev/hidraw0".to_owned()),
            report_descriptor_size: None,
        };

        let hwmon: Vec<HwmonDevice> = Vec::new();
        let usb = vec![usb];
        let hid = vec![hid];
        let registry = detect_capabilities(hwmon.as_slice(), usb.as_slice(), hid.as_slice());
        assert_eq!(
            registry.find("asus.aura.usb")[0].detection,
            DetectionStatus::Detected
        );
        let rgb = registry.find("asus.aura.rgb.control");
        assert_eq!(rgb[0].support, SupportStatus::Unknown);
        assert_eq!(rgb[0].detection, DetectionStatus::NotChecked);
        assert_eq!(rgb[0].write_access, AccessStatus::NotChecked);
    }

    #[test]
    fn reports_verified_fan_read_without_claiming_write_access() {
        let hwmon = HwmonDevice {
            sysfs_path: PathBuf::from("/sys/devices/fan-controller"),
            device_id: "pci:0000:02:00.0".to_owned(),
            name: "fan-controller".to_owned(),
            display_name: "fan-controller".to_owned(),
            sensors: vec![SensorReading {
                label: "fan1".to_owned(),
                value: SensorValue::FanSpeedRpm { rpm: 1200 },
            }],
        };

        let hwmon = vec![hwmon];
        let usb: Vec<UsbDevice> = Vec::new();
        let hid: Vec<HidDevice> = Vec::new();
        let registry = detect_capabilities(hwmon.as_slice(), usb.as_slice(), hid.as_slice());
        let fan = registry.find("sensor.fan_rpm.read");
        assert_eq!(fan.len(), 1);
        assert_eq!(fan[0].detection, DetectionStatus::Verified);
        assert_eq!(fan[0].read_access, AccessStatus::Accessible);
        assert_eq!(fan[0].write_access, AccessStatus::NotApplicable);
        assert_eq!(fan[0].support, SupportStatus::Unknown);
    }
}
