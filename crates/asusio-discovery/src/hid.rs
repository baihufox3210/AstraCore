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

pub fn discover_hid(root: &Path) -> io::Result<Vec<HidDevice>> {
    Ok(discover_hid_report(root)?.devices)
}

pub fn discover_hid_report(root: &Path) -> io::Result<DiscoveryReport<HidDevice>> {
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
