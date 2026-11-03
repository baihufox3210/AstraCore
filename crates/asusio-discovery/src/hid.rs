use crate::hid_descriptor::{HidReportDescriptorInfo, parse_hid_report_descriptor};
use asusio_types::{DiscoveryReport, DiscoveryWarning, HidDevice};
use std::{fs, io, path::Path};

fn read_optional(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn uevent_field(contents: &str, key: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        line.strip_prefix(key)
            .and_then(|value| value.strip_prefix('='))
            .map(str::to_owned)
    })
}

fn parse_hid_id(value: &str) -> Option<(String, String)> {
    let mut parts = value.split(':');
    let bus = parts.next()?;
    let vendor = parts.next()?;
    let product = parts.next()?;
    if parts.next().is_some() || u32::from_str_radix(bus, 16).is_err() {
        return None;
    }
    let vendor = u32::from_str_radix(vendor, 16).ok()?;
    let product = u32::from_str_radix(product, 16).ok()?;
    Some((format!("{vendor:04x}"), format!("{product:04x}")))
}

/// Scan `root` (normally [`crate::sysfs::HID`]); unreadable entries become warnings.
pub fn discover(root: impl AsRef<Path>) -> io::Result<DiscoveryReport<HidDevice>> {
    let root = root.as_ref();
    let mut report = DiscoveryReport::default();

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();

        let Some(uevent) = read_optional(&path.join("uevent")) else {
            report.warnings.push(DiscoveryWarning {
                path,
                reason: "missing, unreadable, or empty HID uevent".to_owned(),
            });
            continue;
        };

        let Some(hid_id) = uevent_field(&uevent, "HID_ID") else {
            report.warnings.push(DiscoveryWarning {
                path,
                reason: "missing HID_ID in uevent".to_owned(),
            });
            continue;
        };

        let Some((vendor_id, product_id)) = parse_hid_id(&hid_id) else {
            report.warnings.push(DiscoveryWarning {
                path,
                reason: "invalid HID_ID in uevent".to_owned(),
            });
            continue;
        };

        let name = uevent_field(&uevent, "HID_NAME")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Unknown HID Device".to_owned());

        let driver = fs::read_link(path.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()));

        let hidraw_path = fs::read_dir(path.join("hidraw")).ok().and_then(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    name.starts_with("hidraw").then(|| format!("/dev/{name}"))
                })
                .min()
        });

        let report_descriptor_size = fs::read(path.join("report_descriptor"))
            .ok()
            .map(|descriptor| descriptor.len());

        report.devices.push(HidDevice {
            device_id: entry.file_name().to_string_lossy().into_owned(),
            vendor_id,
            product_id,
            name,
            driver,
            hidraw_path,
            report_descriptor_size,
        });
    }

    report.devices.sort_by(|a, b| a.device_id.cmp(&b.device_id));
    report.sort_warnings();

    Ok(report)
}

/// Look up one HID device by its sysfs ID, e.g. `0003:0B05:1CD2.0001`.
pub fn find(root: impl AsRef<Path>, device_id: &str) -> io::Result<HidDevice> {
    discover(root)?
        .devices
        .into_iter()
        .find(|device| device.device_id == device_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HID device ID not found"))
}

/// Raw report descriptor bytes from sysfs. This does not access hidraw.
pub fn read_report_descriptor(root: impl AsRef<Path>, device_id: &str) -> io::Result<Vec<u8>> {
    fs::read(root.as_ref().join(device_id).join("report_descriptor"))
}

/// Read and parse the report descriptor from sysfs. This does not access hidraw.
pub fn report_descriptor(
    root: impl AsRef<Path>,
    device_id: &str,
) -> io::Result<HidReportDescriptorInfo> {
    parse_hid_report_descriptor(&read_report_descriptor(root, device_id)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// `HID_PHYS` from the device's uevent: a stable physical USB interface path.
pub fn physical_path(root: impl AsRef<Path>, device_id: &str) -> io::Result<String> {
    let text = fs::read_to_string(root.as_ref().join(device_id).join("uevent"))?;
    uevent_field(&text, "HID_PHYS")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing HID physical identity"))
}

#[cfg(test)]
mod tests {
    use super::{parse_hid_id, uevent_field};

    #[test]
    fn normalizes_hid_ids_and_rejects_invalid_values() {
        assert_eq!(
            parse_hid_id("0003:00000B05:00001AB5"),
            Some(("0b05".to_owned(), "1ab5".to_owned()))
        );
        assert_eq!(parse_hid_id("0003:bogus:00001AB5"), None);
        assert_eq!(parse_hid_id("0003:00000B05"), None);
        assert_eq!(
            uevent_field("HID_ID=0003:00000B05:00001AB5", "HID_ID"),
            Some("0003:00000B05:00001AB5".to_owned())
        );
    }
}
