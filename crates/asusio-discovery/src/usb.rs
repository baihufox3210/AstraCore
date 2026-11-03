use asusio_types::{DiscoveryReport, DiscoveryWarning, UsbDevice};
use std::{fs, io, path::Path};

fn read(path: &Path) -> io::Result<String> {
    Ok(fs::read_to_string(path)?.trim().to_owned())
}

fn read_optional(path: &Path) -> Option<String> {
    read(path).ok().filter(|value| !value.is_empty())
}

fn read_required(path: &Path, report: &mut DiscoveryReport<UsbDevice>) -> Option<String> {
    match read(path) {
        Ok(value) if !value.is_empty() => Some(value),
        Ok(_) => {
            report.warnings.push(DiscoveryWarning {
                path: path.to_path_buf(),
                reason: "empty USB ID".to_owned(),
            });
            None
        }
        Err(error) => {
            report.warnings.push(DiscoveryWarning {
                path: path.to_path_buf(),
                reason: error.to_string(),
            });
            None
        }
    }
}

/// Scan `root` (normally [`crate::sysfs::USB`]); unreadable entries become warnings.
pub fn discover(root: impl AsRef<Path>) -> io::Result<DiscoveryReport<UsbDevice>> {
    let root = root.as_ref();
    let mut report = DiscoveryReport::default();

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let sysfs_id = entry.file_name().to_string_lossy().into_owned();

        // USB interface entries (for example 1-2:1.0) do not carry device IDs.
        if sysfs_id.contains(':') {
            continue;
        }

        let (Some(vendor_id), Some(product_id)) = (
            read_required(&path.join("idVendor"), &mut report),
            read_required(&path.join("idProduct"), &mut report),
        ) else {
            continue;
        };

        report.devices.push(UsbDevice {
            sysfs_id,
            vendor_id,
            product_id,
            manufacturer: read_optional(&path.join("manufacturer")),
            product: read_optional(&path.join("product")),
        });
    }

    report.devices.sort_by(|a, b| a.sysfs_id.cmp(&b.sysfs_id));
    report.sort_warnings();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::discover;
    use std::{fs, time::SystemTime};

    #[test]
    fn skips_missing_ids_and_normalizes_empty_names() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("asusio-usb-{}-{nonce}", std::process::id()));
        let valid = root.join("2-1");
        let incomplete = root.join("1-1");
        let interface = root.join("2-1:1.0");
        fs::create_dir_all(&valid).unwrap();
        fs::create_dir(&incomplete).unwrap();
        fs::create_dir(&interface).unwrap();
        fs::write(valid.join("idVendor"), "0b05\n").unwrap();
        fs::write(valid.join("idProduct"), "1234\n").unwrap();
        fs::write(valid.join("product"), "  \n").unwrap();
        fs::write(incomplete.join("idVendor"), "\n").unwrap();
        fs::write(incomplete.join("idProduct"), "5678\n").unwrap();

        let report = discover(&root).unwrap();
        let devices = report.devices.clone();
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].sysfs_id, "2-1");
        assert_eq!(devices[0].vendor_id, "0b05");
        assert_eq!(devices[0].product, None);
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].path.ends_with("1-1/idVendor"));
    }
}
