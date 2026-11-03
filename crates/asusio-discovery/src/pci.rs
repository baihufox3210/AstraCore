use asusio_types::PciDevice;
use std::{fs, io, path::Path};

fn read(path: &Path) -> io::Result<String> {
    Ok(fs::read_to_string(path)?.trim().to_owned())
}

fn read_nonempty(path: &Path) -> io::Result<String> {
    let value = read(path)?;
    if value.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty PCI field",
        ));
    }
    Ok(value)
}

pub fn discover_pci(root: &Path) -> io::Result<Vec<PciDevice>> {
    let mut devices = Vec::new();

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();

        let (Ok(vendor_id), Ok(device_id), Ok(class_id)) = (
            read_nonempty(&path.join("vendor")),
            read_nonempty(&path.join("device")),
            read_nonempty(&path.join("class")),
        ) else {
            continue;
        };

        devices.push(PciDevice {
            address: entry.file_name().to_string_lossy().into_owned(),
            vendor_id,
            device_id,
            class_id,
        });
    }

    devices.sort_by(|a, b| a.address.cmp(&b.address));
    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::discover_pci;
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

        let devices = discover_pci(&root).unwrap();
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].address, "0000:01:00.0");
        assert_eq!(devices[1].address, "0000:02:00.0");
        assert_eq!(devices[0].class_id, "0x010800");
    }
}
