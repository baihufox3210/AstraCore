//! Transport for the ASUS Aura mainboard HID controller (0b05:1cd2).
//! Vendor commands are traced in ASUS MB Manager 1.00.11, AacHal_x64_MbMgr.dll;
//! they cannot be inferred from the descriptor.

use crate::util::{hidraw_path, invalid_data, invalid_input, open_hidraw};
use asusio_discovery::{
    hid,
    hid_descriptor::{HidReportDescriptorInfo, HidReportType},
};
use asusio_types::{HidDevice, Rgb, UsbId};
use std::{
    fs, io,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};

pub const DEVICE: UsbId = UsbId::new(0x0b05, 0x1cd2);
/// Highest ARGB header capacity traced in the HAL; controllers advertise their own count.
pub const MAX_ARGB_HEADERS: u8 = 5;
pub const PAYLOAD_BYTES: usize = 63;
const REPORT_BYTES: usize = PAYLOAD_BYTES + 1;
const REPORT_ID: u8 = 0x41;

/// One Output Report payload, excluding the Report ID.
pub type Payload = [u8; PAYLOAD_BYTES];
/// One Input Report including the Report ID.
pub type Response = [u8; REPORT_BYTES];

const CMD_CONFIG: u8 = 0x03;
const CMD_EFFECT: u8 = 0x05;
const CMD_MAINBOARD: u8 = 0x08;
const CMD_WDL: u8 = 0x24;
const EFFECT_STATIC: u8 = 1;

fn command(code: u8) -> Payload {
    let mut payload = [0; PAYLOAD_BYTES];
    payload[0] = code;
    payload
}

/// The complete Output Report (Report ID followed by the payload), as written to hidraw.
pub fn frame(payload: &Payload) -> Response {
    let mut report = [0; REPORT_BYTES];
    report[0] = REPORT_ID;
    report[1..].copy_from_slice(payload);
    report
}

/// Base configuration query. Full report starts 41 03 00; no color, mode, or save command.
pub fn configuration_query_payload() -> Payload {
    command(CMD_CONFIG)
}

fn configuration_response(bytes: &[u8], command: u8) -> Option<Response> {
    let report: Response = bytes.try_into().ok()?;
    (report[0] == REPORT_ID && report[1] == command.wrapping_add(0x80)).then_some(report)
}

/// The HAL copies base response byte 3 into its ARGB configuration loop bound.
pub fn argb_header_count(report: &[u8]) -> io::Result<u8> {
    let report = configuration_response(report, CMD_CONFIG)
        .filter(|report| report[2] == 0)
        .ok_or_else(|| invalid_data("not an Aura base configuration response"))?;
    if report[3] > MAX_ARGB_HEADERS {
        return Err(invalid_data(
            "ARGB header count exceeds the traced HAL capacity",
        ));
    }
    Ok(report[3])
}

/// Onboard PWM LED locations from the HAL's mainboard configuration table.
pub fn onboard_locations(report: &[u8]) -> io::Result<Vec<u8>> {
    let report = configuration_response(report, CMD_MAINBOARD)
        .ok_or_else(|| invalid_data("not an Aura mainboard configuration response"))?;
    let count = usize::from(report[4]);
    if count > 8 {
        return Err(invalid_data(
            "onboard LED count exceeds the traced configuration table",
        ));
    }
    (0..count)
        .map(|index| {
            if report[7 + index * 2] != 0 {
                return Err(invalid_data(
                    "onboard location exceeds the HAL's one-byte selector",
                ));
            }
            Ok(report[6 + index * 2])
        })
        .collect()
}

fn wdl_enabled(report: &[u8]) -> io::Result<bool> {
    let report = configuration_response(report, CMD_WDL)
        .ok_or_else(|| invalid_data("not an Aura WDL state response"))?;
    Ok(report[3] != 0)
}

/// Highest effect ID of the HAL's effect table (Starry_Night); 0xFF ("Default") is not sent.
pub const MAX_EFFECT: u8 = 13;

fn check_effect(effect: u8) -> io::Result<()> {
    if (1..=MAX_EFFECT).contains(&effect) {
        Ok(())
    } else {
        Err(invalid_input("Aura effect ID must be 1-13"))
    }
}

/// MyAacArgbLedDevice::SetEffect: command 05, strip 0, effect ID, G,R,B byte order.
/// Static and Breathing (ID 2) have been physically verified on the B850-A controller.
pub fn argb_effect_payload(header: u8, effect: u8, rgb: Rgb) -> io::Result<Payload> {
    if header >= MAX_ARGB_HEADERS {
        return Err(invalid_input("ARGB header index must be 0-4"));
    }
    check_effect(effect)?;
    let mut payload = command(CMD_EFFECT);
    payload[3] = header;
    payload[7] = effect;
    payload[8..11].copy_from_slice(&[rgb[1], rgb[0], rgb[2]]);
    Ok(payload)
}

pub fn argb_static_payload(header: u8, rgb: Rgb) -> io::Result<Payload> {
    argb_effect_payload(header, EFFECT_STATIC, rgb)
}

/// HAL SetLEDs onboard deviceType=1 branch; location is the low byte of its configuration entry.
pub fn onboard_effect_payload(location: u8, effect: u8, rgb: Rgb) -> io::Result<Payload> {
    check_effect(effect)?;
    let mut payload = command(CMD_EFFECT);
    payload[1] = 1;
    payload[5] = location;
    payload[7] = effect;
    payload[8..11].copy_from_slice(&[rgb[1], rgb[0], rgb[2]]);
    Ok(payload)
}

pub fn onboard_static_payload(location: u8, rgb: Rgb) -> Payload {
    onboard_effect_payload(location, EFFECT_STATIC, rgb).expect("Static is a valid effect")
}

/// Validate the exact device ID and Output Report layout observed on the target controller.
fn validate_interface(device: &HidDevice, descriptor: &HidReportDescriptorInfo) -> io::Result<()> {
    if !device.is(DEVICE) {
        return Err(invalid_data("not the supported Aura HID device ID"));
    }
    let outputs: Vec<_> = descriptor
        .reports
        .iter()
        .filter(|report| report.report_type == HidReportType::Output)
        .collect();
    if outputs.len() != 1
        || outputs[0].report_id != Some(REPORT_ID)
        || outputs[0].payload_bits != (PAYLOAD_BYTES * 8) as u64
        || outputs[0].transfer_bytes != REPORT_BYTES as u64
    {
        return Err(invalid_data("Aura HID output report layout does not match"));
    }
    hidraw_path(device)?;
    Ok(())
}

/// Read and validate the descriptor from sysfs. This does not access hidraw.
pub fn inspect(device: &HidDevice, sysfs_root: &Path) -> io::Result<HidReportDescriptorInfo> {
    let info = hid::report_descriptor(sysfs_root, &device.device_id)?;
    validate_interface(device, &info)?;
    Ok(info)
}

/// Send one vendor query, with bounded stale-input draining and a two-second read timeout.
fn query(device: &HidDevice, sysfs_root: &Path, payload: &Payload) -> io::Result<Response> {
    let info = inspect(device, sysfs_root)?;
    let inputs: Vec<_> = info
        .reports
        .iter()
        .filter(|report| report.report_type == HidReportType::Input)
        .collect();
    if inputs.len() != 1
        || inputs[0].report_id != Some(REPORT_ID)
        || inputs[0].payload_bits != (PAYLOAD_BYTES * 8) as u64
        || inputs[0].transfer_bytes != REPORT_BYTES as u64
    {
        return Err(invalid_data("Aura HID input report layout does not match"));
    }
    let mut node = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(hidraw_path(device)?)?;
    let disconnected = || io::Error::new(io::ErrorKind::UnexpectedEof, "HID device disconnected");
    let mut buffer = [0_u8; REPORT_BYTES];
    let mut drained = false;
    for _ in 0..32 {
        match node.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                drained = true;
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
            Ok(0) => return Err(disconnected()),
            Ok(_) => {}
        }
    }
    if !drained {
        return Err(io::Error::other(
            "HID input queue did not drain; no query sent",
        ));
    }
    write_report(&mut node, payload)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match node.read(&mut buffer) {
            Ok(0) => return Err(disconnected()),
            Ok(length) => {
                if let Some(report) = configuration_response(&buffer[..length], payload[0]) {
                    return Ok(report);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "no 64-byte Aura configuration response (41 {:02x}) within 2 seconds",
            payload[0].wrapping_add(0x80)
        ),
    ))
}

/// Query the base configuration once. The descriptor alone does not establish semantics.
pub fn query_configuration(device: &HidDevice, sysfs_root: &Path) -> io::Result<Response> {
    query(device, sysfs_root, &configuration_query_payload())
}

/// Vendor getters traced in the same HAL: base, mainboard, each ARGB header, and WDL state.
pub fn query_configuration_details(
    device: &HidDevice,
    sysfs_root: &Path,
) -> io::Result<Vec<(String, Response)>> {
    let base = query_configuration(device, sysfs_root)?;
    let count = argb_header_count(&base)?;
    let mut reports = vec![("base".to_owned(), base)];
    reports.push((
        "mainboard".to_owned(),
        query(device, sysfs_root, &command(CMD_MAINBOARD))?,
    ));
    for index in 0..count {
        let mut payload = command(CMD_CONFIG);
        payload[1] = 1;
        payload[2] = index;
        reports.push((format!("argb{index}"), query(device, sysfs_root, &payload)?));
    }
    reports.push((
        "wdl-state".to_owned(),
        query(device, sysfs_root, &command(CMD_WDL))?,
    ));
    Ok(reports)
}

fn ensure_wdl_disabled(device: &HidDevice, sysfs_root: &Path, refusal: &str) -> io::Result<()> {
    if wdl_enabled(&query(device, sysfs_root, &command(CMD_WDL))?)? {
        return Err(io::Error::other(refusal.to_owned()));
    }
    Ok(())
}

fn send_reports(
    device: &HidDevice,
    reports: impl IntoIterator<Item = (String, Payload)>,
) -> io::Result<()> {
    let mut node = open_hidraw(device)?;
    for (label, payload) in reports {
        write_report(&mut node, &payload).map_err(|error| {
            io::Error::new(error.kind(), format!("{label} write failed: {error}"))
        })?;
    }
    Ok(())
}

/// Static color on advertised ARGB headers, after re-checking the header range and WDL state.
/// Does not send WDL changes, S5 effects, or explicit persistent-save commands.
pub fn write_argb_static(
    device: &HidDevice,
    sysfs_root: &Path,
    headers: &[u8],
    rgb: Rgb,
) -> io::Result<()> {
    write_argb_effect(device, sysfs_root, headers, EFFECT_STATIC, rgb)
}

/// One HAL effect (by ID) on the given ARGB headers; otherwise as `write_argb_static`.
pub fn write_argb_effect(
    device: &HidDevice,
    sysfs_root: &Path,
    headers: &[u8],
    effect: u8,
    rgb: Rgb,
) -> io::Result<()> {
    if headers.is_empty() {
        return Err(invalid_input("select at least one ARGB header"));
    }
    let payloads = headers
        .iter()
        .map(|&header| {
            Ok((
                format!("ARGB header {header}"),
                argb_effect_payload(header, effect, rgb)?,
            ))
        })
        .collect::<io::Result<Vec<_>>>()?;
    let count = argb_header_count(&query_configuration(device, sysfs_root)?)?;
    if headers.iter().any(|header| *header >= count) {
        return Err(invalid_input(
            "selected ARGB header is not advertised by the controller",
        ));
    }
    ensure_wdl_disabled(
        device,
        sysfs_root,
        "WDL is enabled; refusing to change ownership or send color reports",
    )?;
    send_reports(device, payloads)
}

/// Static color on advertised onboard locations, after re-checking them and WDL state.
pub fn write_onboard_static(
    device: &HidDevice,
    sysfs_root: &Path,
    locations: &[u8],
    rgb: Rgb,
) -> io::Result<()> {
    write_onboard_effect(device, sysfs_root, locations, EFFECT_STATIC, rgb)
}

/// One HAL effect (by ID) on advertised onboard locations; otherwise as
/// `write_onboard_static`.
pub fn write_onboard_effect(
    device: &HidDevice,
    sysfs_root: &Path,
    locations: &[u8],
    effect: u8,
    rgb: Rgb,
) -> io::Result<()> {
    check_effect(effect)?;
    if locations.is_empty() {
        return Err(invalid_input("select at least one onboard location"));
    }
    let advertised = onboard_locations(&query(device, sysfs_root, &command(CMD_MAINBOARD))?)?;
    if locations
        .iter()
        .any(|location| !advertised.contains(location))
    {
        return Err(invalid_data(
            "selected onboard location is not advertised by the controller",
        ));
    }
    ensure_wdl_disabled(
        device,
        sysfs_root,
        "WDL is enabled; refusing to change lighting ownership",
    )?;
    let payloads = locations
        .iter()
        .map(|&location| {
            Ok((
                format!("onboard location {location}"),
                onboard_effect_payload(location, effect, rgb)?,
            ))
        })
        .collect::<io::Result<Vec<_>>>()?;
    send_reports(device, payloads)
}

/// Convert an explicitly supplied byte slice into one Output Report payload.
pub fn payload_from_slice(bytes: &[u8]) -> io::Result<Payload> {
    bytes
        .try_into()
        .map_err(|_| invalid_input("Aura output payload must be exactly 63 bytes"))
}

/// Send an explicitly supplied vendor payload as one HID Output Report.
/// This is a protocol research primitive, not an RGB color API.
pub fn write_raw_output(
    device: &HidDevice,
    sysfs_root: &Path,
    payload: &Payload,
) -> io::Result<()> {
    inspect(device, sysfs_root)?;
    write_report(&mut open_hidraw(device)?, payload)
}

fn write_report(writer: &mut impl Write, payload: &Payload) -> io::Result<()> {
    crate::util::write_once(writer, &frame(payload), "HID Output Report")
}

#[cfg(test)]
mod tests {
    use super::*;
    use asusio_discovery::hid_descriptor::parse_hid_report_descriptor;

    #[test]
    fn onboard_configuration_locations_and_packet_layout() {
        let mut response = [0; 64];
        response[..10].copy_from_slice(&[0x41, 0x88, 0, 0, 2, 0, 0, 0, 1, 0]);
        assert_eq!(onboard_locations(&response).unwrap(), vec![0, 1]);
        let payload = onboard_static_payload(0x12, [0x12, 0x34, 0x56]);
        assert_eq!(
            &payload[..11],
            &[5, 1, 0, 0, 0, 0x12, 0, 1, 0x34, 0x12, 0x56]
        );
        let wave = onboard_effect_payload(0x12, 11, [0x12, 0x34, 0x56]).unwrap();
        assert_eq!(wave[7], 11, "effect ID in byte 8 of the report");
        assert_eq!(&wave[8..11], &[0x34, 0x12, 0x56]);
        let breathing = argb_effect_payload(1, 2, [0x80, 0xc0, 0xff]).unwrap();
        assert_eq!(
            &breathing[..11],
            &[5, 0, 0, 1, 0, 0, 0, 2, 0xc0, 0x80, 0xff],
            "the report confirmed on the hardware, header 1"
        );
        for bad in [0, MAX_EFFECT + 1, 0xff] {
            assert!(argb_effect_payload(0, bad, [0; 3]).is_err());
            assert!(onboard_effect_payload(0, bad, [0; 3]).is_err());
        }
        response[4] = 9;
        assert!(onboard_locations(&response).is_err());
        assert!(onboard_locations(&response[..8]).is_err());
    }

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
        assert!(payload_from_slice(&[0; 62]).is_err());
        assert!(payload_from_slice(&[0; 63]).is_ok());
    }

    #[test]
    fn vendor_configuration_query_is_bounded_to_the_observed_report() {
        let mut report = frame(&configuration_query_payload());
        assert_eq!(&report[..3], &[0x41, 0x03, 0]);
        assert!(report[3..].iter().all(|byte| *byte == 0));
        report[1] = 0x83;
        assert!(configuration_response(&report, 0x03).is_some());
        assert!(configuration_response(&report[..63], 0x03).is_none());
        report[1] = 0xa3;
        assert!(configuration_response(&report, 0x03).is_none());
    }

    #[test]
    fn observed_base_configuration_advertises_three_argb_headers() {
        let mut report = [0; 64];
        report[..22].copy_from_slice(&[
            0x41, 0x83, 0x00, 0x03, 0x02, 0x02, 0xf4, 0x01, 0x32, 0x36, 0x30, 0x31, 0x31, 0x35,
            0x32, 0x30, 0x30, 0x30, 0x30, 0x35, 0x01, 0x02,
        ]);
        assert_eq!(argb_header_count(&report).unwrap(), 3);
        report[3] = 6;
        assert!(argb_header_count(&report).is_err());
        assert!(argb_header_count(&report[..63]).is_err());
    }

    #[test]
    fn static_packet_matches_vendor_hal_header_effect_and_grb_offsets() {
        let report = frame(&argb_static_payload(2, [0x12, 0x34, 0x56]).unwrap());
        assert_eq!(
            &report[..12],
            &[0x41, 0x05, 0, 0, 2, 0, 0, 0, 1, 0x34, 0x12, 0x56]
        );
        assert!(report[12..].iter().all(|byte| *byte == 0));
        assert!(argb_static_payload(5, [0; 3]).is_err());
    }

    #[test]
    fn observed_wdl_flag_uses_byte_three_not_aura_state_byte_two() {
        let mut report = [0; 64];
        report[..6].copy_from_slice(&[0x41, 0xa4, 1, 0, 1, 1]);
        assert!(!wdl_enabled(&report).unwrap());
        report[3] = 1;
        assert!(wdl_enabled(&report).unwrap());
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
        validate_interface(&device, &info).unwrap();
        device.product_id = "1cd3".into();
        assert!(validate_interface(&device, &info).is_err());
        device.product_id = "1cd2".into();
        let mut wrong_layout = AURA_DESCRIPTOR;
        wrong_layout[27] = 0x3e;
        let wrong_info = parse_hid_report_descriptor(&wrong_layout).unwrap();
        assert!(validate_interface(&device, &wrong_info).is_err());
    }
}
