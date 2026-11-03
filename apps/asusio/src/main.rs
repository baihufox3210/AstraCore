use asusio_backends::aura_hid::{inspect_aura_interface, write_raw_output};
use asusio_core::detect_capabilities;
use asusio_discovery::{
    discover_board, hid::discover_hid_report, hid_descriptor::parse_hid_report_descriptor,
    hwmon::discover_hwmon_report, pci::discover_pci_report, usb::discover_usb_report,
};
use asusio_types::{DiscoveryWarning, SensorValue};
use clap::{Parser, Subcommand};
use std::{fs, io, path::Path};

#[derive(Parser)]
#[command(
    name = "asusio",
    version,
    about = "ASUS Hardware I/O Framework for Linux"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Display motherboard information
    Info,

    /// Display hardware sensor readings
    Sensors {
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },

    /// List detected PCI and USB devices
    Devices {
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,

        /// Include HID devices
        #[arg(long)]
        hid: bool,
    },

    /// Display capabilities inferred from readable sensors and device IDs
    Capabilities {
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },

    /// Inspect a HID report descriptor from sysfs without accessing hidraw
    HidDescriptor {
        /// HID sysfs device ID, as shown by `asusio devices --hid`
        device_id: String,
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },

    /// Inspect or send a native Aura HID Output Report
    Lighting {
        #[command(subcommand)]
        action: LightingAction,
    },
}

#[derive(Subcommand)]
enum LightingAction {
    /// Validate the Aura HID interface without writing to it
    Probe { device_id: String },
    /// Send a 63-byte vendor payload; requires --execute
    RawOutput {
        device_id: String,
        /// Exactly 126 hexadecimal digits (63 payload bytes, excluding Report ID)
        payload_hex: String,
        /// Actually write to the HID device
        #[arg(long)]
        execute: bool,
    },
}

fn parse_hex_payload(value: &str) -> Result<Vec<u8>, &'static str> {
    if value.len() != 126 || !value.is_ascii() {
        return Err("payload must contain exactly 126 hexadecimal digits");
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let hex = std::str::from_utf8(pair).map_err(|_| "invalid UTF-8 payload")?;
            u8::from_str_radix(hex, 16).map_err(|_| "payload contains non-hexadecimal digits")
        })
        .collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Info => {
            let board = discover_board()?;

            println!("ASUSIO Hardware Information");
            println!("Vendor : {}", board.vendor);
            println!("Board  : {}", board.model);
            println!(
                "BIOS   : {}",
                board.bios_version.as_deref().unwrap_or("Unknown")
            );
        }

        Commands::Sensors { json } => {
            let report = discover_hwmon_report(Path::new("/sys/class/hwmon"))?;

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": 1,
                        "devices": report.devices,
                        "warnings": report.warnings,
                    }))?
                );
                return Ok(());
            }

            for (index, device) in report.devices.into_iter().enumerate() {
                if index > 0 {
                    println!();
                }

                println!("[{}]", device.display_name);

                for sensor in device.sensors {
                    match sensor.value {
                        SensorValue::Temperature { milli_celsius } => {
                            println!(
                                "  {:<20} {:.1} °C",
                                sensor.label,
                                milli_celsius as f64 / 1000.0
                            );
                        }

                        SensorValue::FanSpeedRpm { rpm } => {
                            println!("  {:<20} {} RPM", sensor.label, rpm);
                        }
                    }
                }
            }
            print_warnings(&report.warnings);
        }

        Commands::Devices { json, hid } => {
            let pci_report = discover_pci_report(Path::new("/sys/bus/pci/devices"))?;
            let usb_report = discover_usb_report(Path::new("/sys/bus/usb/devices"))?;

            // Only scan HID when explicitly requested.
            let hid_devices = if hid {
                Some(discover_hid_report(Path::new("/sys/bus/hid/devices"))?)
            } else {
                None
            };

            let mut warnings = pci_report.warnings;
            warnings.extend(usb_report.warnings);
            if let Some(ref report) = hid_devices {
                warnings.extend(report.warnings.iter().cloned());
            }
            warnings.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.reason.cmp(&b.reason)));
            let pci = pci_report.devices;
            let usb = usb_report.devices;

            if json {
                let output = if let Some(report) = hid_devices.as_ref() {
                    serde_json::json!({
                        "schema_version": 1,
                        "pci": &pci,
                        "usb": &usb,
                        "hid": &report.devices,
                        "warnings": &warnings,
                    })
                } else {
                    serde_json::json!({
                        "schema_version": 1,
                        "pci": &pci,
                        "usb": &usb,
                        "warnings": &warnings,
                    })
                };

                println!("{}", serde_json::to_string_pretty(&output)?);
                return Ok(());
            }

            println!("PCI Devices ({})", pci.len());

            for device in &pci {
                println!(
                    "  {}  {}:{}  class={}",
                    device.address, device.vendor_id, device.device_id, device.class_id,
                );
            }

            println!("\nUSB Devices ({})", usb.len());

            for device in &usb {
                println!(
                    "  {}  {}:{}  {}",
                    device.sysfs_id,
                    device.vendor_id,
                    device.product_id,
                    device.product.as_deref().unwrap_or("Unknown"),
                );
            }

            if let Some(report) = hid_devices {
                let devices = report.devices;
                println!("\nHID Devices ({})", devices.len());

                for device in devices {
                    println!(
                        "  {}  {}:{}  {}",
                        device.device_id, device.vendor_id, device.product_id, device.name,
                    );

                    println!(
                        "    Driver : {}",
                        device.driver.as_deref().unwrap_or("Unknown")
                    );

                    println!(
                        "    HIDRAW : {}",
                        device.hidraw_path.as_deref().unwrap_or("Not found")
                    );

                    println!(
                        "    Report Descriptor : {}",
                        device
                            .report_descriptor_size
                            .map(|size| format!("{size} bytes"))
                            .unwrap_or_else(|| "Unavailable".to_owned())
                    );
                }
            }
            print_warnings(&warnings);
        }

        Commands::Capabilities { json } => {
            let hwmon = discover_hwmon_report(Path::new("/sys/class/hwmon"))?;
            let usb = discover_usb_report(Path::new("/sys/bus/usb/devices"))?;
            let hid = discover_hid_report(Path::new("/sys/bus/hid/devices"))?;
            let registry = detect_capabilities(&hwmon.devices, &usb.devices, &hid.devices);

            let mut warnings = hwmon.warnings;
            warnings.extend(usb.warnings);
            warnings.extend(hid.warnings);
            warnings.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.reason.cmp(&b.reason)));

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": 1,
                        "capabilities": registry.capabilities(),
                        "warnings": warnings,
                    }))?
                );
                return Ok(());
            }

            println!("Capabilities ({})", registry.capabilities().len());
            for capability in registry.capabilities() {
                println!(
                    "  {} [{}]  support={:?} detection={:?} read={:?} write={:?}",
                    capability.id,
                    capability.device_id.as_deref().unwrap_or("system"),
                    capability.support,
                    capability.detection,
                    capability.read_access,
                    capability.write_access,
                );
                if let Some(detail) = &capability.detail {
                    println!("    {detail}");
                }
            }
            print_warnings(&warnings);
        }
        Commands::HidDescriptor { device_id, json } => {
            let root = Path::new("/sys/bus/hid/devices");
            let devices = discover_hid_report(root)?;
            if !devices
                .devices
                .iter()
                .any(|device| device.device_id == device_id)
            {
                return Err(
                    io::Error::new(io::ErrorKind::NotFound, "HID device ID not found").into(),
                );
            }

            let descriptor = fs::read(root.join(&device_id).join("report_descriptor"))?;
            let info = match parse_hid_report_descriptor(&descriptor) {
                Ok(info) => info,
                Err(error) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "schema_version": 1,
                                "device_id": device_id,
                                "error": error,
                            }))?
                        );
                    }
                    return Err(Box::new(error));
                }
            };

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": 1,
                        "device_id": device_id,
                        "descriptor": info,
                    }))?
                );
            } else {
                println!("HID Report Descriptor: {device_id}");
                println!("Size: {} bytes", info.descriptor_size_bytes);
                println!("Uses Report IDs: {}", info.uses_report_ids);
                println!("Long items skipped: {}", info.long_items_skipped);
                for report in info.reports {
                    let id = report
                        .report_id
                        .map(|id| format!("0x{id:02x}"))
                        .unwrap_or_else(|| "none".to_owned());
                    println!(
                        "  {:?} ID={} payload={} bits ({} bytes), transfer={} bytes",
                        report.report_type,
                        id,
                        report.payload_bits,
                        report.payload_bytes,
                        report.transfer_bytes,
                    );
                }
            }
        }
        Commands::Lighting { action } => {
            let (device_id, payload) = match &action {
                LightingAction::Probe { device_id } => (device_id, None),
                LightingAction::RawOutput {
                    device_id,
                    payload_hex,
                    ..
                } => (device_id, Some(parse_hex_payload(payload_hex)?)),
            };
            let root = Path::new("/sys/bus/hid/devices");
            let devices = discover_hid_report(root)?;
            let device = devices
                .devices
                .iter()
                .find(|device| &device.device_id == device_id)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "HID device ID not found")
                })?;
            let info = inspect_aura_interface(device, root)?;
            println!(
                "Aura HID: {} (Output Report ID 0x41, payload 63 bytes, transfer 64 bytes)",
                device.device_id
            );
            if let Some(payload) = payload {
                if let LightingAction::RawOutput { execute, .. } = action {
                    if execute {
                        write_raw_output(device, root, &payload)?;
                        println!("Output Report written");
                    } else {
                        println!("Validated payload; use --execute to write it");
                    }
                }
            } else {
                println!("Descriptor: {} bytes", info.descriptor_size_bytes);
                println!(
                    "hidraw: {}",
                    device.hidraw_path.as_deref().unwrap_or("unavailable")
                );
                println!(
                    "hidraw node exists: {}",
                    device
                        .hidraw_path
                        .as_deref()
                        .is_some_and(|path| Path::new(path).exists())
                );
            }
        }
    }

    Ok(())
}

fn print_warnings(warnings: &[DiscoveryWarning]) {
    for warning in warnings {
        eprintln!("warning: {}: {}", warning.path.display(), warning.reason);
    }
}

#[cfg(test)]
mod tests {
    use super::parse_hex_payload;
    use asusio_types::SensorValue;

    #[test]
    fn sensor_json_carries_type_and_units() {
        let temperature = serde_json::to_value(SensorValue::Temperature {
            milli_celsius: 42500,
        })
        .unwrap();

        let fan = serde_json::to_value(SensorValue::FanSpeedRpm { rpm: 1200 }).unwrap();

        assert_eq!(
            temperature,
            serde_json::json!({
                "kind": "temperature",
                "milli_celsius": 42500
            })
        );

        assert_eq!(
            fan,
            serde_json::json!({
                "kind": "fan_speed_rpm",
                "rpm": 1200
            })
        );
    }

    #[test]
    fn raw_payload_requires_exact_hex_length() {
        assert_eq!(parse_hex_payload(&"ab".repeat(63)).unwrap(), vec![0xab; 63]);
        assert!(parse_hex_payload(&"ab".repeat(62)).is_err());
        assert!(parse_hex_payload(&"gg".repeat(63)).is_err());
    }
}
