//! Optional native lianli-daemon IPC when that daemon already owns the LED USB interface.
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

use crate::util::invalid_data as invalid;
use astracore_types::Rgb;

const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

fn read_response(reader: impl Read) -> io::Result<Value> {
    let mut line = Vec::new();
    BufReader::new(reader)
        .take(MAX_RESPONSE_BYTES + 1)
        .read_until(b'\n', &mut line)?;
    if line.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(invalid("lianli-daemon response exceeds 1 MiB"));
    }
    if line.last() != Some(&b'\n') {
        return Err(invalid("incomplete lianli-daemon response"));
    }
    let response: Value =
        serde_json::from_slice(&line).map_err(|error| invalid(error.to_string()))?;
    match response.get("status").and_then(Value::as_str) {
        Some("ok") => response
            .get("data")
            .cloned()
            .ok_or_else(|| invalid("missing daemon response data")),
        Some("error") => Err(io::Error::other(format!(
            "lianli-daemon rejected request: {}",
            response
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unspecified error")
        ))),
        _ => Err(invalid("unexpected lianli-daemon response status")),
    }
}

fn request(socket: &Path, request: &Value) -> io::Result<Value> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let mut message = serde_json::to_vec(request).map_err(|error| invalid(error.to_string()))?;
    message.push(b'\n');
    stream.write_all(&message)?;
    read_response(stream)
}

/// Checks the device and that it offers `mode`; returns how many colors the mode takes.
fn validate_target(capabilities: &Value, device_id: &str, mode: &str) -> io::Result<u64> {
    let devices = capabilities
        .as_array()
        .ok_or_else(|| invalid("expected daemon device capability array"))?;
    let device = devices
        .iter()
        .find(|device| device.get("device_id").and_then(Value::as_str) == Some(device_id))
        .ok_or_else(|| {
            invalid("selected Universal Screen LED device is not listed by lianli-daemon")
        })?;
    if device
        .pointer("/render_profile/family")
        .and_then(Value::as_str)
        != Some("UniversalScreen")
        || device.pointer("/zones/0/led_count").and_then(Value::as_u64) != Some(60)
        || !device
            .get("supported_modes")
            .and_then(Value::as_array)
            .is_some_and(|modes| modes.iter().any(|value| value.as_str() == Some(mode)))
    {
        return Err(invalid(format!(
            "daemon capabilities do not match the 60-LED Universal Screen with mode {mode}"
        )));
    }
    let max_colors = device
        .get("effect_parameters")
        .and_then(Value::as_array)
        .and_then(|effects| {
            effects
                .iter()
                .find(|effect| effect.get("mode").and_then(Value::as_str) == Some(mode))
        })
        .and_then(|effect| effect.get("max_colors"))
        .and_then(Value::as_u64)
        .unwrap_or(1);
    Ok(max_colors)
}

/// Modes AstraCore sends to the screen (lianli-daemon renders them in software).
pub const MODES: [&str; 5] = ["Static", "Breathing", "RainbowMorph", "Rainbow", "Twinkle"];

/// One color for modes that take colors, none for those that make their own; speed and
/// direction are left to the daemon's defaults.
fn effect_request(device_id: &str, mode: &str, rgb: Rgb, colors: u64) -> Value {
    let colors: Vec<Rgb> = if colors == 0 { Vec::new() } else { vec![rgb] };
    json!({"method":"SetRgbEffect","params":{"device_id":device_id,"zone":0,"effect":{"mode":mode,"colors":colors,"brightness":4}}})
}

#[cfg(test)]
fn static_request(device_id: &str, rgb: Rgb) -> Value {
    effect_request(device_id, "Static", rgb, 1)
}

// This adapter implements the published 1.2.4 IPC schema, not arbitrary future versions.
const SUPPORTED_DAEMON_VERSION: &str = "1.2.4";

fn guarded_request(identity: &Value, inner: Value) -> io::Result<Value> {
    if identity.get("version").and_then(Value::as_str) != Some(SUPPORTED_DAEMON_VERSION)
        || identity.get("protocol_version").and_then(Value::as_u64) != Some(1)
        || !identity
            .get("capabilities")
            .and_then(Value::as_array)
            .is_some_and(|values| {
                values
                    .iter()
                    .any(|value| value.as_str() == Some("guarded_writes"))
            })
    {
        return Err(invalid(
            "this IPC adapter supports lianli-daemon 1.2.4 protocol 1 with guarded_writes only",
        ));
    }
    let instance = identity
        .get("instance_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .ok_or_else(|| invalid("missing or invalid daemon instance identity"))?;
    Ok(
        json!({"method":"Guarded","params":{"guard":{"client_version":SUPPORTED_DAEMON_VERSION,"protocol_version":1,"instance_id":instance},"request":inner}}),
    )
}

/// A successful reply acknowledges acceptance, not physical rendering completion.
pub fn set_static(socket: &Path, usb_id: &str, rgb: Rgb) -> io::Result<()> {
    set_effect(socket, usb_id, "Static", rgb)
}

/// One of [`MODES`] with `rgb` (ignored by the modes that make their own colors).
pub fn set_effect(socket: &Path, usb_id: &str, mode: &str, rgb: Rgb) -> io::Result<()> {
    if !MODES.contains(&mode) {
        return Err(invalid(format!("unsupported screen effect mode {mode}")));
    }
    let id = crate::lianli_screen::DEVICE;
    let device_id = format!("hid:{:04x}:{:04x}:{usb_id}", id.vendor, id.product);
    let identity = request(socket, &json!({"method":"GetDaemonInfo"}))?;
    let capabilities = request(socket, &json!({"method":"GetRgbCapabilities"}))?;
    let colors = validate_target(&capabilities, &device_id, mode)?;
    let guarded = guarded_request(&identity, effect_request(&device_id, mode, rgb, colors))?;
    request(socket, &guarded)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guard_is_bound_to_supported_version_and_instance() {
        let mut identity = json!({"version":"1.2.4","protocol_version":1,"instance_id":"current-instance","capabilities":["guarded_writes"]});
        let value = guarded_request(&identity, static_request("target", [0; 3])).unwrap();
        assert_eq!(value["params"]["guard"]["instance_id"], "current-instance");
        identity["version"] = json!("future");
        assert!(guarded_request(&identity, static_request("target", [0; 3])).is_err());
        identity["version"] = json!("1.2.4");
        identity["capabilities"] = json!([]);
        assert!(guarded_request(&identity, static_request("target", [0; 3])).is_err());
    }
    #[test]
    fn rejects_daemon_errors_and_incomplete_frames() {
        assert!(
            read_response(b"{\"status\":\"error\",\"message\":\"write gate closed\"}\n".as_slice())
                .unwrap_err()
                .to_string()
                .contains("write gate closed")
        );
        assert!(read_response(b"{\"status\":\"ok\",\"data\":null}".as_slice()).is_err());
        assert!(read_response(vec![b' '; MAX_RESPONSE_BYTES as usize + 1].as_slice()).is_err());
    }
    #[test]
    fn validates_exact_device_family_count_and_static_mode() {
        let id = "hid:0416:8050:6-9.3";
        let mut devices = json!([{"device_id":id,"render_profile":{"family":"UniversalScreen"},"zones":[{"led_count":60}],"supported_modes":["Static","Rainbow"],"effect_parameters":[{"mode":"Static","max_colors":1},{"mode":"Rainbow","max_colors":0}]}]);
        assert_eq!(validate_target(&devices, id, "Static").unwrap(), 1);
        assert_eq!(validate_target(&devices, id, "Rainbow").unwrap(), 0);
        assert!(
            validate_target(&devices, id, "Twinkle").is_err(),
            "mode not offered"
        );
        assert!(validate_target(&devices, "hid:0416:8050:6-9.4", "Static").is_err());
        devices[0]["zones"][0]["led_count"] = json!(20);
        assert!(validate_target(&devices, id, "Static").is_err());
    }
    #[test]
    fn effect_requests_send_colors_only_to_modes_that_take_them() {
        let rainbow = effect_request("x", "Rainbow", [1, 2, 3], 0);
        assert_eq!(rainbow["params"]["effect"]["mode"], "Rainbow");
        assert_eq!(rainbow["params"]["effect"]["colors"], json!([]));
        let breathing = effect_request("x", "Breathing", [1, 2, 3], 1);
        assert_eq!(breathing["params"]["effect"]["colors"], json!([[1, 2, 3]]));
        assert!(
            breathing["params"]["effect"].get("speed").is_none(),
            "daemon default"
        );
    }
    #[test]
    fn static_request_uses_native_ipc_and_full_brightness() {
        let value = static_request("hid:0416:8050:6-9.3", [0x12, 0x34, 0x56]);
        assert_eq!(value["method"], "SetRgbEffect");
        assert_eq!(
            value["params"]["effect"]["colors"],
            json!([[0x12, 0x34, 0x56]])
        );
        assert_eq!(value["params"]["effect"]["brightness"], 4);
    }
}
