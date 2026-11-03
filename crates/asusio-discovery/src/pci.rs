use asusio_types::{DiscoveryReport, DiscoveryWarning, PciDevice};
use std::{fs, io, path::Path};

fn read(path: &Path) -> io::Result<String> {
    Ok(fs::read_to_string(path)?.trim().to_owned())
}

fn read_nonempty(path: &Path, report: &mut DiscoveryReport<PciDevice>) -> Option<String> {
    let value = match read(path) {
        Ok(value) => value,
        Err(error) => {
            report.warnings.push(DiscoveryWarning {
                path: path.to_path_buf(),
                reason: error.to_string(),
            });
            return None;
        }
    };
    if value.is_empty() {
        report.warnings.push(DiscoveryWarning {
            path: path.to_path_buf(),
            reason: "empty PCI field".to_owned(),
        });
        return None;
    }
    Some(value)
}

pub fn discover_pci(root: &Path) -> io::Result<Vec<PciDevice>> {
    Ok(discover_pci_report(root)?.devices)
}

pub fn discover_pci_report(root: &Path) -> io::Result<DiscoveryReport<PciDevice>> {
    let mut report = DiscoveryReport::default();

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();

        let (Some(vendor_id), Some(device_id), Some(class_id)) = (
            read_nonempty(&path.join("vendor"), &mut report),
            read_nonempty(&path.join("device"), &mut report),
            read_nonempty(&path.join("class"), &mut report),
        ) else {
            continue;
        };

        report.devices.push(PciDevice {
            address: entry.file_name().to_string_lossy().into_owned(),
            vendor_id,
            device_id,
            class_id,
        });
    }

    report.devices.sort_by(|a, b| a.address.cmp(&b.address));
    report.sort_warnings();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::{discover_pci, discover_pci_report};
    use std::{fs, time::SystemTime};

    #[test]
    fn skips_incomplete_devices_and_sorts_addresses() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("asusio-pci-{}-{nonce}", std::process::id()));
        for address in ["0000:02:00.0", "0000:01:00.0", "0000:03:00.0"] {
            fs::create_dir_all(root.join(address)).unwrap();
        }
        for address in ["0000:02:00.0", "0000:01:00.0"] {
            let path = root.join(address);
            fs::write(path.join("vendor"), "0x1022\n").unwrap();
            fs::write(path.join("device"), "0x1234\n").unwrap();
            fs::write(path.join("class"), "0x010800\n").unwrap();
        }
        fs::write(root.join("0000:03:00.0/vendor"), "\n").unwrap();
        fs::write(root.join("0000:03:00.0/device"), "0x1234\n").unwrap();
        fs::write(root.join("0000:03:00.0/class"), "0x010800\n").unwrap();

        let report = discover_pci_report(&root).unwrap();
        let devices = discover_pci(&root).unwrap();
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].address, "0000:01:00.0");
        assert_eq!(devices[1].address, "0000:02:00.0");
        assert_eq!(devices[0].class_id, "0x010800");
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].path.ends_with("0000:03:00.0/vendor"));
    }
}
