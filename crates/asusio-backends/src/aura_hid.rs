//! Low-level transport for the observed ASUS Aura HID interface.
//! Vendor-specific color commands cannot be inferred from the descriptor.

use asusio_discovery::hid_descriptor::{
    HidReportDescriptorInfo, HidReportType, parse_hid_report_descriptor,
};
use asusio_types::HidDevice;
use std::{fs, io, io::Write, path::Path};

const VENDOR_ID: &str = "0b05";
const PRODUCT_ID: &str = "1cd2";
const REPORT_ID: u8 = 0x41;
const PAYLOAD_BYTES: usize = 63;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Validate the exact interface and output layout observed on the target device.
pub fn validate_aura_interface(
    device: &HidDevice,
    descriptor: &HidReportDescriptorInfo,
) -> io::Result<()> {
    if device.vendor_id != VENDOR_ID || device.product_id != PRODUCT_ID {
        return Err(invalid("not the supported Aura HID device ID"));
    }
    let outputs: Vec<_> = descriptor
        .reports
        .iter()
        .filter(|report| report.report_type == HidReportType::Output)
        .collect();
    if outputs.len() != 1
        || outputs[0].report_id != Some(REPORT_ID)
        || outputs[0].payload_bits != (PAYLOAD_BYTES * 8) as u64
        || outputs[0].transfer_bytes != (PAYLOAD_BYTES + 1) as u64
    {
        return Err(invalid("Aura HID output report layout does not match"));
    }
    if device.hidraw_path.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "hidraw node not found",
        ));
    }
    Ok(())
}

/// Read and validate the descriptor from sysfs. This does not access hidraw.
pub fn inspect_aura_interface(
    device: &HidDevice,
    sysfs_root: &Path,
) -> io::Result<HidReportDescriptorInfo> {
    let descriptor = fs::read(sysfs_root.join(&device.device_id).join("report_descriptor"))?;
    let info = parse_hid_report_descriptor(&descriptor)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    validate_aura_interface(device, &info)?;
    Ok(info)
}

/// Send an explicitly supplied vendor payload as one HID Output Report.
/// This is a protocol research primitive, not an RGB color API.
pub fn write_raw_output(device: &HidDevice, sysfs_root: &Path, payload: &[u8]) -> io::Result<()> {
    inspect_aura_interface(device, sysfs_root)?;
    if payload.len() != PAYLOAD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Aura output payload must be exactly 63 bytes",
        ));
    }
    let path = device
        .hidraw_path
        .as_deref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "hidraw node not found"))?;
    let mut node = fs::OpenOptions::new().write(true).open(path)?;
    write_report(&mut node, payload)
}

fn write_report(writer: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    if payload.len() != PAYLOAD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Aura output payload must be exactly 63 bytes",
        ));
    }
    let mut report = [0_u8; PAYLOAD_BYTES + 1];
    report[0] = REPORT_ID;
    report[1..].copy_from_slice(payload);
    let written = writer.write(&report)?;
    if written != report.len() {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "short HID Output Report write",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const AURA_DESCRIPTOR: [u8; 36] = [
        0x06, 0x0a, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x41, 0x09, 0x82, 0x75, 0x08, 0x95, 0x3f,
        0x15, 0x00, 0x26, 0xff, 0x00, 0x81, 0x02, 0x09, 0x02, 0x75, 0x08, 0x95, 0x3f, 0x15, 0x00,
        0x26, 0xff, 0x00, 0x91, 0x02, 0xc0,
    ];

    #[test]
    fn frames_aura_output_with_report_id() {
        let mut data = Vec::new();
        write_report(&mut data, &[0x7f; PAYLOAD_BYTES]).unwrap();
        assert_eq!(data.len(), 64);
        assert_eq!(data[0], 0x41);
        assert_eq!(&data[1..], &[0x7f; PAYLOAD_BYTES]);
        assert!(write_report(&mut data, &[0; 62]).is_err());
    }

    #[test]
    fn rejects_partial_write_without_sending_another_report() {
        struct PartialWriter(usize);
        impl Write for PartialWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0 += 1;
                Ok(bytes.len() - 1)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut writer = PartialWriter(0);
        let error = write_report(&mut writer, &[0; PAYLOAD_BYTES]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WriteZero);
        assert_eq!(writer.0, 1);
    }

    #[test]
    fn rejects_other_device_and_layout() {
        let mut device = HidDevice {
            device_id: "0003:0B05:1CD2.000D".into(),
            vendor_id: "0b05".into(),
            product_id: "1cd2".into(),
            name: "Aura".into(),
            driver: None,
            hidraw_path: Some("/dev/hidraw0".into()),
            report_descriptor_size: Some(36),
        };
        let info = parse_hid_report_descriptor(&AURA_DESCRIPTOR).unwrap();
        validate_aura_interface(&device, &info).unwrap();
        device.product_id = "1cd3".into();
        assert!(validate_aura_interface(&device, &info).is_err());
        device.product_id = "1cd2".into();
        let mut wrong_layout = AURA_DESCRIPTOR;
        wrong_layout[27] = 0x3e;
        let wrong_info = parse_hid_report_descriptor(&wrong_layout).unwrap();
        assert!(validate_aura_interface(&device, &wrong_info).is_err());
    }
}
