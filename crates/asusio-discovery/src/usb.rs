use asusio_types::UsbDevice;
use std::{fs, io, path::Path};

fn read(path: &Path) -> io::Result<String> {
    Ok(fs::read_to_string(path)?.trim().to_owned())
}

fn read_optional(path: &Path) -> Option<String> {
    read(path).ok().filter(|value| !value.is_empty())
}

pub fn discover_usb(root: &Path) -> io::Result<Vec<UsbDevice>> {
    let mut devices = Vec::new();

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();

        let (Some(vendor_id), Some(product_id)) = (
            read_optional(&path.join("idVendor")),
            read_optional(&path.join("idProduct")),
        ) else {
            continue;
        };

        devices.push(UsbDevice {
            sysfs_id: entry.file_name().to_string_lossy().into_owned(),
            vendor_id,
            product_id,
            manufacturer: read_optional(&path.join("manufacturer")),
            product: read_optional(&path.join("product")),
        });
    }

    devices.sort_by(|a, b| a.sysfs_id.cmp(&b.sysfs_id));
    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::discover_usb;
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
        fs::create_dir_all(&valid).unwrap();
        fs::create_dir(&incomplete).unwrap();
        fs::write(valid.join("idVendor"), "0b05\n").unwrap();
        fs::write(valid.join("idProduct"), "1234\n").unwrap();
        fs::write(valid.join("product"), "  \n").unwrap();
        fs::write(incomplete.join("idVendor"), "\n").unwrap();
        fs::write(incomplete.join("idProduct"), "5678\n").unwrap();

        let devices = discover_usb(&root).unwrap();
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].sysfs_id, "2-1");
        assert_eq!(devices[0].vendor_id, "0b05");
        assert_eq!(devices[0].product, None);
    }
}
