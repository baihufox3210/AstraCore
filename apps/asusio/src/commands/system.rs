//! Read-only discovery commands.
use crate::output::{CliResult, print_json, print_warnings};
use asusio_core::detect_capabilities;
use asusio_discovery::{
    discover_board, hid, hid_descriptor::parse_hid_report_descriptor, hwmon, pci, sysfs, usb,
};
use asusio_types::{SensorValue, merge_warnings};
use serde_json::json;

pub fn info() -> CliResult {
    let board = discover_board()?;
    println!("ASUSIO Hardware Information");
    println!("Vendor : {}", board.vendor);
    println!("Board  : {}", board.model);
    println!(
        "BIOS   : {}",
        board.bios_version.as_deref().unwrap_or("Unknown")
    );
    Ok(())
}

pub fn sensors(json: bool) -> CliResult {
    let report = hwmon::discover(sysfs::HWMON)?;
    if json {
        return print_json(json!({"devices": report.devices, "warnings": report.warnings}));
    }
    for (index, device) in report.devices.iter().enumerate() {
        if index > 0 {
            println!();
        }
        println!("[{}]", device.display_name);
        for sensor in &device.sensors {
            match sensor.value {
                SensorValue::Temperature { milli_celsius } => println!(
                    "  {:<20} {:.1} °C",
                    sensor.label,
                    milli_celsius as f64 / 1000.0
                ),
                SensorValue::FanSpeedRpm { rpm } => {
                    println!("  {:<20} {} RPM", sensor.label, rpm)
                }
            }
        }
    }
    print_warnings(&report.warnings);
    Ok(())
}

pub fn devices(json: bool, include_hid: bool) -> CliResult {
    let pci = pci::discover(sysfs::PCI)?;
    let usb = usb::discover(sysfs::USB)?;
    // Only scan HID when explicitly requested.
    let hid = include_hid.then(|| hid::discover(sysfs::HID)).transpose()?;
    let hid_warnings = hid.as_ref().map(|report| report.warnings.clone());
    let warnings = merge_warnings([pci.warnings, usb.warnings, hid_warnings.unwrap_or_default()]);

    if json {
        let mut document = json!({"pci": pci.devices, "usb": usb.devices, "warnings": warnings});
        if let Some(report) = &hid {
            document["hid"] = json!(report.devices);
        }
        return print_json(document);
    }

    println!("PCI Devices ({})", pci.devices.len());
    for device in &pci.devices {
        println!(
            "  {}  {}:{}  class={}",
            device.address, device.vendor_id, device.device_id, device.class_id,
        );
    }
    println!("\nUSB Devices ({})", usb.devices.len());
    for device in &usb.devices {
        println!(
            "  {}  {}:{}  {}",
            device.sysfs_id,
            device.vendor_id,
            device.product_id,
            device.product.as_deref().unwrap_or("Unknown"),
        );
    }
    if let Some(report) = hid {
        println!("\nHID Devices ({})", report.devices.len());
        for device in report.devices {
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
                    .map_or_else(|| "Unavailable".to_owned(), |size| format!("{size} bytes"))
            );
        }
    }
    print_warnings(&warnings);
    Ok(())
}

pub fn capabilities(json: bool) -> CliResult {
    let hwmon = hwmon::discover(sysfs::HWMON)?;
    let usb = usb::discover(sysfs::USB)?;
    let hid = hid::discover(sysfs::HID)?;
    let registry = detect_capabilities(&hwmon.devices, &usb.devices, &hid.devices);
    let warnings = merge_warnings([hwmon.warnings, usb.warnings, hid.warnings]);

    if json {
        return print_json(json!({
            "capabilities": registry.capabilities(),
            "warnings": warnings,
        }));
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
    Ok(())
}

pub fn hid_descriptor(device_id: &str, json: bool) -> CliResult {
    hid::find(sysfs::HID, device_id)?;
    let bytes = hid::read_report_descriptor(sysfs::HID, device_id)?;
    let info = match parse_hid_report_descriptor(&bytes) {
        Ok(info) => info,
        Err(error) => {
            if json {
                print_json(json!({"device_id": device_id, "error": error}))?;
            }
            return Err(Box::new(error));
        }
    };
    if json {
        return print_json(json!({"device_id": device_id, "descriptor": info}));
    }
    println!("HID Report Descriptor: {device_id}");
    println!("Size: {} bytes", info.descriptor_size_bytes);
    println!("Uses Report IDs: {}", info.uses_report_ids);
    println!("Long items skipped: {}", info.long_items_skipped);
    for report in info.reports {
        let id = report
            .report_id
            .map_or_else(|| "none".to_owned(), |id| format!("0x{id:02x}"));
        println!(
            "  {:?} ID={} payload={} bits ({} bytes), transfer={} bytes",
            report.report_type,
            id,
            report.payload_bits,
            report.payload_bytes,
            report.transfer_bytes,
        );
    }
    Ok(())
}
