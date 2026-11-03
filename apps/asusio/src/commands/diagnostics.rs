//! Raw native lighting diagnostics addressed by explicit device IDs.
//! Every command previews by default; hardware access requires `--execute`.
use crate::{
    cli::DiagnosticAction,
    output::{CliResult, hex_bytes, preview_only, print_json, print_warnings},
};
use asusio_backends::{aura_hid, ene_smbus, lianli_ipc, lianli_screen, scope_keyboard};
use asusio_core::lighting::{self, InterfaceKind, hex};
use asusio_discovery::{hid, hid_descriptor::HidReportDescriptorInfo, sysfs};
use asusio_types::HidDevice;
use serde_json::json;
use std::path::Path;

fn hid_root() -> &'static Path {
    Path::new(sysfs::HID)
}

/// Find a HID device by ID and validate it as the Aura controller (sysfs only).
fn aura_device(device_id: &str) -> std::io::Result<(HidDevice, HidReportDescriptorInfo)> {
    let device = hid::find(hid_root(), device_id)?;
    let info = aura_hid::inspect(&device, hid_root())?;
    Ok((device, info))
}

fn sorted_unique(mut values: Vec<u8>) -> Vec<u8> {
    values.sort_unstable();
    values.dedup();
    values
}

/// The diagnostic command that drives each interface kind.
fn command_for(kind: InterfaceKind) -> &'static str {
    match kind {
        InterfaceKind::AuraHid => "aura-static / aura-board-static",
        InterfaceKind::ScopeKeyboard => "keyboard-static",
        InterfaceKind::UniversalScreen => "screen-static",
    }
}

pub fn run(action: DiagnosticAction) -> CliResult {
    match action {
        DiagnosticAction::List { json } => {
            let report = lighting::discover_interfaces()?;
            if json {
                let devices: Vec<_> = report
                    .devices
                    .iter()
                    .map(|device| {
                        json!({
                            "device_id": device.device_id,
                            "name": device.name,
                            "backend": command_for(device.kind),
                            "access": "not_checked",
                        })
                    })
                    .collect();
                return print_json(json!({"devices": devices, "warnings": report.warnings}));
            }
            for device in &report.devices {
                println!(
                    "{}: {} [{}]",
                    device.device_id,
                    device.name.as_deref().unwrap_or_default(),
                    command_for(device.kind)
                );
            }
            print_warnings(&report.warnings);
            println!(
                "I2C GPU/DRAM: use lighting i2c-list, then ene-probe on an explicitly selected bus/address"
            );
        }
        DiagnosticAction::I2cList => {
            for adapter in ene_smbus::adapters(Path::new(sysfs::I2C))? {
                println!(
                    "i2c-{}: {} ({})",
                    adapter.bus,
                    adapter.name,
                    adapter.sysfs_path.display()
                );
            }
        }
        DiagnosticAction::EneProbe {
            bus,
            address,
            execute,
        } => {
            if execute {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&ene_smbus::probe(bus, address)?)?
                );
            } else {
                println!(
                    "Preview: identify ENE at i2c-{bus} address 0x{address:02x}; add --execute (register reads select the ENE pointer)"
                );
            }
        }
        DiagnosticAction::EneState {
            bus,
            address,
            execute,
        } => {
            if execute {
                print_json(json!({"state": ene_smbus::read_state(bus, address)?}))?;
            } else {
                println!(
                    "Preview: read ENE mode and colors at i2c-{bus} 0x{address:02x}; add --execute (register pointer selection only)"
                );
            }
        }
        DiagnosticAction::EneStatic {
            bus,
            address,
            color,
            execute,
        } => {
            if execute {
                let info = ene_smbus::write_static(bus, address, color, None)?;
                println!(
                    "{} ({} LEDs): static reports written; physical color requires verification",
                    info.version, info.led_count
                );
            } else {
                println!(
                    "Preview: set i2c-{bus} address 0x{address:02x} to {}; add --execute to identify firmware and apply",
                    hex(color)
                );
            }
        }
        DiagnosticAction::AuraStatic {
            device_id,
            color,
            headers,
            execute,
        } => {
            let headers = sorted_unique(headers);
            let (device, _) = aura_device(&device_id)?;
            println!("Native Aura ARGB static color reports");
            for &header in &headers {
                let report = aura_hid::frame(&aura_hid::argb_static_payload(header, color)?);
                println!("ARGB header {header}: {}", hex_bytes(&report));
            }
            if execute {
                aura_hid::write_argb_static(&device, hid_root(), &headers, color)?;
                println!("Static reports written");
            } else {
                preview_only("query controller state and send these reports");
            }
        }
        DiagnosticAction::AuraBoardStatic {
            device_id,
            color,
            locations,
            execute,
        } => {
            let locations = sorted_unique(locations);
            let (device, _) = aura_device(&device_id)?;
            for &location in &locations {
                let report = aura_hid::frame(&aura_hid::onboard_static_payload(location, color));
                println!("Onboard location {location}: {}", hex_bytes(&report));
            }
            if execute {
                aura_hid::write_onboard_static(&device, hid_root(), &locations, color)?;
                println!("Onboard static reports written; physical lighting requires verification");
            } else {
                preview_only("validate locations and send");
            }
        }
        DiagnosticAction::AuraConfig {
            device_id,
            details,
            execute,
        } => {
            let (device, _) = aura_device(&device_id)?;
            let request = aura_hid::frame(&aura_hid::configuration_query_payload());
            println!("Configuration request: {}", hex_bytes(&request));
            if !execute {
                preview_only("send this vendor configuration query");
                if details {
                    println!(
                        "Details: query mainboard config (08), ARGB config (03 01 index), and WDL state (24)"
                    );
                }
                return Ok(());
            }
            let reports = if details {
                aura_hid::query_configuration_details(&device, hid_root())?
            } else {
                vec![(
                    "base".to_owned(),
                    aura_hid::query_configuration(&device, hid_root())?,
                )]
            };
            for (label, response) in reports {
                match label.as_str() {
                    "base" => println!(
                        "Advertised ARGB headers: {}",
                        aura_hid::argb_header_count(&response)?
                    ),
                    "mainboard" => println!(
                        "Advertised onboard locations: {:?}",
                        aura_hid::onboard_locations(&response)?
                    ),
                    _ => {}
                }
                println!("Configuration response ({label}): {}", hex_bytes(&response));
            }
        }
        DiagnosticAction::Probe { device_id } => {
            let (device, info) = aura_device(&device_id)?;
            print_aura_summary(&device);
            println!("Descriptor: {} bytes", info.descriptor_size_bytes);
            let hidraw = device.hidraw_path.as_deref();
            println!("hidraw: {}", hidraw.unwrap_or("unavailable"));
            println!(
                "hidraw node exists: {}",
                hidraw.is_some_and(|path| Path::new(path).exists())
            );
        }
        DiagnosticAction::RawOutput {
            device_id,
            payload_hex,
            execute,
        } => {
            let (device, _) = aura_device(&device_id)?;
            print_aura_summary(&device);
            if execute {
                aura_hid::write_raw_output(&device, hid_root(), &payload_hex)?;
                println!("Output Report written");
            } else {
                println!("Validated payload; use --execute to write it");
            }
        }
        DiagnosticAction::KeyboardStatic {
            device_id,
            color,
            execute,
        } => {
            let device = hid::find(hid_root(), &device_id)?;
            scope_keyboard::inspect(&device, hid_root())?;
            println!(
                "Scope II RX static: {}",
                hex_bytes(&scope_keyboard::static_report(color))
            );
            if execute {
                scope_keyboard::write_static(&device, hid_root(), color)?;
                println!("Report written; physical keyboard color requires verification");
            } else {
                preview_only("send (no persistent-save command)");
            }
        }
        DiagnosticAction::ScreenStatic {
            device_id,
            color,
            daemon_socket,
            execute,
        } => {
            let usb_root = Path::new(sysfs::USB);
            let node = lianli_screen::inspect(&device_id, usb_root)?;
            println!("Universal Screen LEDs: {device_id} ({})", node.display());
            match &daemon_socket {
                Some(socket) => println!(
                    "Native lianli-daemon IPC: {} (Static {}, full brightness)",
                    socket.display(),
                    hex(color)
                ),
                None => {
                    for packet in lianli_screen::static_packets(color) {
                        println!("{}", hex_bytes(&packet));
                    }
                }
            }
            if !execute {
                preview_only("send");
            } else if let Some(socket) = daemon_socket {
                lianli_ipc::set_static(&socket, &device_id, color)?;
                println!(
                    "Static effect accepted by lianli-daemon; physical rendering requires verification"
                );
            } else {
                lianli_screen::write_static(&device_id, usb_root, color)?;
                println!("Three LED packets written; physical color requires verification");
            }
        }
    }
    Ok(())
}

fn print_aura_summary(device: &HidDevice) {
    println!(
        "Aura HID: {} (Output Report ID 0x41, payload {} bytes, transfer {} bytes)",
        device.device_id,
        aura_hid::PAYLOAD_BYTES,
        aura_hid::PAYLOAD_BYTES + 1
    );
}
