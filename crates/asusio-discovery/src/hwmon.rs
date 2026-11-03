use asusio_types::{HwmonDevice, SensorReading, SensorValue};
use std::{fs, io, path::Path};

fn read_trimmed(path: &Path) -> io::Result<String> {
    Ok(fs::read_to_string(path)?.trim().to_owned())
}

fn channel_number(filename: &str, prefix: &str) -> Option<u32> {
    filename
        .strip_prefix(prefix)?
        .strip_suffix("_input")?
        .parse::<u32>()
        .ok()
        .filter(|number| *number > 0)
}

fn identify_device(path: &Path, name: &str) -> (std::path::PathBuf, String, String) {
    let device_path = path.join("device");
    let sysfs_path = fs::canonicalize(&device_path)
        .or_else(|_| fs::canonicalize(path))
        .unwrap_or_else(|_| path.to_path_buf());
    let address = sysfs_path
        .file_name()
        .map(|part| part.to_string_lossy().into_owned())
        .unwrap_or_default();

    if name == "nvme" {
        let model = read_trimmed(&device_path.join("model")).ok();
        let pci_address = read_trimmed(&device_path.join("address")).ok();
        let device_id = pci_address
            .as_ref()
            .map(|value| format!("pci:{value}"))
            .unwrap_or_else(|| sysfs_path.display().to_string());
        let model = model
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("nvme ({address})"));
        let display_name = pci_address
            .map(|value| format!("{model} ({value})"))
            .unwrap_or(model);
        return (sysfs_path, device_id, display_name);
    }

    if name == "spd5118" {
        return (
            sysfs_path,
            format!("i2c:{address}"),
            format!("DDR5 SPD ({address})"),
        );
    }

    let device_id = sysfs_path.display().to_string();
    let display_name = format!("{name} ({address})");
    (sysfs_path, device_id, display_name)
}

pub fn discover_hwmon(root: &Path) -> io::Result<Vec<HwmonDevice>> {
    let mut devices = Vec::new();

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let id = entry.file_name().to_string_lossy().into_owned();

        if !id
            .strip_prefix("hwmon")
            .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }

        let Ok(name) = read_trimmed(&path.join("name")) else {
            continue;
        };

        let mut sensors = Vec::new();

        let Ok(attributes) = fs::read_dir(&path) else {
            continue;
        };

        for attr in attributes {
            let Ok(attr) = attr else {
                continue;
            };
            let filename = attr.file_name();
            let filename = filename.to_string_lossy();

            let sensor = if let Some(n) = channel_number(&filename, "temp") {
                let Ok(value) = read_trimmed(&attr.path()) else {
                    continue;
                };
                let Ok(value) = value.parse::<i64>() else {
                    continue;
                };

                let label = read_trimmed(&path.join(format!("temp{n}_label")))
                    .ok()
                    .filter(|label| !label.is_empty())
                    .unwrap_or_else(|| format!("temp{n}"));

                SensorReading {
                    label,
                    value: SensorValue::Temperature {
                        milli_celsius: value,
                    },
                }
            } else if let Some(n) = channel_number(&filename, "fan") {
                let Ok(value) = read_trimmed(&attr.path()) else {
                    continue;
                };
                let Ok(value) = value.parse::<u64>() else {
                    continue;
                };

                let label = read_trimmed(&path.join(format!("fan{n}_label")))
                    .ok()
                    .filter(|label| !label.is_empty())
                    .unwrap_or_else(|| format!("fan{n}"));

                SensorReading {
                    label,
                    value: SensorValue::FanSpeedRpm { rpm: value },
                }
            } else {
                continue;
            };

            sensors.push(sensor);
        }

        sensors.sort_by(|a, b| a.label.cmp(&b.label));

        if !sensors.is_empty() {
            let (sysfs_path, device_id, display_name) = identify_device(&path, &name);
            devices.push(HwmonDevice {
                sysfs_path,
                device_id,
                name,
                display_name,
                sensors,
            });
        }
    }

    devices.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.device_id.cmp(&b.device_id))
    });

    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::{channel_number, discover_hwmon, identify_device};
    use asusio_types::{HwmonDevice, SensorReading, SensorValue};
    use std::{fs, os::unix::fs::symlink, time::SystemTime};

    #[test]
    fn parses_sensor_channels() {
        assert_eq!(channel_number("temp1_input", "temp"), Some(1));
        assert_eq!(channel_number("fan2_input", "fan"), Some(2));
        assert_eq!(channel_number("temp1_label", "temp"), None);
        assert_eq!(channel_number("pwm1", "temp"), None);
    }

    #[test]
    fn discovers_readable_sensors_and_ignores_invalid_entries() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("asusio-hwmon-{}-{nonce}", std::process::id()));
        let device = root.join("hwmon0");
        let empty = root.join("hwmon1");
        fs::create_dir_all(&device).unwrap();
        fs::create_dir(&empty).unwrap();
        fs::write(device.join("name"), "testchip\n").unwrap();
        fs::write(device.join("temp1_input"), "42500\n").unwrap();
        fs::write(device.join("temp1_label"), "CPU\n").unwrap();
        fs::write(device.join("fan2_input"), "1200\n").unwrap();
        fs::write(device.join("temp2_input"), "invalid\n").unwrap();
        fs::write(empty.join("name"), "empty\n").unwrap();

        let sysfs_path = fs::canonicalize(&device).unwrap();
        let result = discover_hwmon(&root).unwrap();
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(
            result,
            vec![HwmonDevice {
                device_id: sysfs_path.display().to_string(),
                sysfs_path,
                name: "testchip".to_owned(),
                display_name: "testchip (hwmon0)".to_owned(),
                sensors: vec![
                    SensorReading {
                        label: "CPU".to_owned(),
                        value: SensorValue::Temperature {
                            milli_celsius: 42500,
                        },
                    },
                    SensorReading {
                        label: "fan2".to_owned(),
                        value: SensorValue::FanSpeedRpm { rpm: 1200 },
                    },
                ],
            }]
        );
    }

    #[test]
    fn identifies_nvme_and_spd_devices() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("asusio-id-{}-{nonce}", std::process::id()));
        let nvme = root.join("nvme0");
        let hwmon = root.join("hwmon0");
        let spd = root.join("7-0051");
        let spd_hwmon = root.join("hwmon1");
        fs::create_dir_all(&nvme).unwrap();
        fs::create_dir(&hwmon).unwrap();
        fs::create_dir(&spd).unwrap();
        fs::create_dir(&spd_hwmon).unwrap();
        symlink(&nvme, hwmon.join("device")).unwrap();
        symlink(&spd, spd_hwmon.join("device")).unwrap();
        fs::write(nvme.join("model"), "Example SSD\n").unwrap();
        fs::write(nvme.join("address"), "0000:08:00.0\n").unwrap();

        let nvme_info = identify_device(&hwmon, "nvme");
        let spd_info = identify_device(&spd_hwmon, "spd5118");
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(nvme_info.1, "pci:0000:08:00.0");
        assert_eq!(nvme_info.2, "Example SSD (0000:08:00.0)");
        assert_eq!(spd_info.1, "i2c:7-0051");
        assert_eq!(spd_info.2, "DDR5 SPD (7-0051)");
    }
}
