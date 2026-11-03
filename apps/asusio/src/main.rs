use asusio_discovery::{
    discover_board, hwmon::discover_hwmon, pci::discover_pci, usb::discover_usb,
};
use asusio_types::SensorValue;
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
            let devices = discover_hwmon(Path::new("/sys/class/hwmon"))?;

            if json {
                println!("{}", serde_json::to_string_pretty(&devices)?);
                return Ok(());
            }

            for (index, device) in devices.into_iter().enumerate() {
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
        }

        Commands::Devices { json } => {
            let pci = discover_pci(Path::new("/sys/bus/pci/devices"))?;
            let usb = discover_usb(Path::new("/sys/bus/usb/devices"))?;

            if json {
                let output = serde_json::json!({
                    "pci": pci,
                    "usb": usb,
                });

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
        }
    }

    Ok(())
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
