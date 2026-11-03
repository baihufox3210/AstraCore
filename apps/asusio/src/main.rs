use asusio_discovery::{
    discover_board, hid::discover_hid_report, hwmon::discover_hwmon_report,
    pci::discover_pci_report, usb::discover_usb_report,
};
use asusio_types::{DiscoveryWarning, SensorValue};
use clap::{Parser, Subcommand};
use std::path::Path;

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
}
