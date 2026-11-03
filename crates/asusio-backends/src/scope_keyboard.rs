//! Native static lighting for ASUS Scope II RX (0b05:1ab5).
//! Wire format reference: AsusAuraTUFKeyboardController::UpdateDevice, Static=0.

use crate::util::{invalid_data, invalid_input, open_hidraw, write_once};
use asusio_discovery::{
    hid,
    hid_descriptor::{HidReportDescriptorInfo, HidReportType},
};
use asusio_types::{HidDevice, Rgb, UsbId};
use std::{io, io::Write, path::Path};

pub const DEVICE: UsbId = UsbId::new(0x0b05, 0x1ab5);

pub fn static_report(rgb: Rgb) -> [u8; 65] {
    // hidraw requires a zero report-number prefix for unnumbered reports.
    let mut report = [0; 65];
    report[1] = 0x51;
    report[2] = 0x2c;
    report[6] = 100; // brightness (0..100)
    report[9] = 2; // per-key keyboard effect color format
    report[10..13].copy_from_slice(&rgb);
    report
}

/// Validate the device ID and vendor interface from sysfs. This does not access hidraw.
pub fn inspect(device: &HidDevice, sysfs_root: &Path) -> io::Result<()> {
    if !device.is(DEVICE) {
        return Err(invalid_input("expected Scope II RX 0b05:1ab5"));
    }
    validate_descriptor(&hid::report_descriptor(sysfs_root, &device.device_id)?)
}

fn validate_descriptor(info: &HidReportDescriptorInfo) -> io::Result<()> {
    let valid = !info.uses_report_ids
        && info.reports.len() == 2
        && [HidReportType::Input, HidReportType::Output]
            .iter()
            .all(|kind| {
                info.reports
                    .iter()
                    .any(|report| report.report_type == *kind && report.payload_bits == 512)
            })
        && info
            .fields
            .iter()
            .all(|field| field.usage_page == Some(0xff00));
    if !valid {
        return Err(invalid_data(
            "select the vendor ff00 interface with 64-byte Input/Output reports",
        ));
    }
    Ok(())
}

/// Static color at full brightness; no persistent-save command.
pub fn write_static(device: &HidDevice, sysfs_root: &Path, rgb: Rgb) -> io::Result<()> {
    inspect(device, sysfs_root)?;
    send(&mut open_hidraw(device)?, &static_report(rgb))
}

fn send(writer: &mut impl Write, report: &[u8; 65]) -> io::Result<()> {
    write_once(writer, report, "keyboard report")
}

#[cfg(test)]
mod tests {
    use super::*;
    use asusio_discovery::hid_descriptor::parse_hid_report_descriptor;
    fn validate(bytes: &[u8]) -> io::Result<()> {
        validate_descriptor(&parse_hid_report_descriptor(bytes).unwrap())
    }
    const DESCRIPTOR: &[u8] = &[
        0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x09, 0x02, 0x75, 0x08, 0x95, 0x40, 0x15, 0x00,
        0x26, 0xff, 0x00, 0x81, 0x00, 0x09, 0x03, 0x75, 0x08, 0x95, 0x40, 0x15, 0x00, 0x26, 0xff,
        0x00, 0x91, 0x00, 0xc0,
    ];
    #[test]
    fn accepts_vendor_interface_and_rejects_keyboard_led_interface() {
        validate(DESCRIPTOR).unwrap();
        assert!(validate(&[0x05, 0x08, 0x75, 0x01, 0x95, 0x05, 0x91, 0x02]).is_err());
    }
    #[test]
    fn static_color_uses_unnumbered_report_and_rgb_order() {
        let report = static_report([0x12, 0x34, 0x56]);
        assert_eq!(
            &report[..13],
            &[0, 0x51, 0x2c, 0, 0, 0, 100, 0, 0, 2, 0x12, 0x34, 0x56]
        );
        assert!(report[13..].iter().all(|byte| *byte == 0));
    }
    #[test]
    fn short_write_is_not_retried() {
        struct Short(usize);
        impl Write for Short {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                self.0 += 1;
                Ok(64)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut writer = Short(0);
        assert!(send(&mut writer, &static_report([0; 3])).is_err());
        assert_eq!(writer.0, 1);
    }
}
