//! Shared presentation and argument parsing for CLI commands.
use astracore_backends::aura_hid;
use astracore_types::{DiscoveryWarning, Rgb};
use serde_json::Value;

pub type CliResult = Result<(), Box<dyn std::error::Error>>;

/// Every JSON document carries `schema_version: 1`.
pub fn print_json(mut document: Value) -> CliResult {
    if let Value::Object(fields) = &mut document {
        fields.insert("schema_version".into(), 1.into());
    }
    println!("{}", serde_json::to_string_pretty(&document)?);
    Ok(())
}

pub fn print_warnings(warnings: &[DiscoveryWarning]) {
    for warning in warnings {
        eprintln!("warning: {}: {}", warning.path.display(), warning.reason);
    }
}

pub fn preview_only(action: &str) {
    println!("Preview only; add --execute to {action}");
}

pub fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) || !value.is_ascii() {
        return None;
    }
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).ok())
        .collect()
}

/// Clap value parsers: invalid input is rejected before any command runs.
pub mod parse {
    use super::*;

    /// Any whitelisted name with a `FromStr` implementation, e.g. a profile mode.
    pub fn from_str<T: std::str::FromStr<Err = std::io::Error>>(value: &str) -> Result<T, String> {
        value
            .parse()
            .map_err(|error: std::io::Error| error.to_string())
    }

    pub fn lighting_mode(value: &str) -> Result<astracore_core::service::LightingMode, String> {
        match value {
            "sync" => Ok(astracore_core::service::LightingMode::Sync),
            "individual" => Ok(astracore_core::service::LightingMode::Individual),
            _ => Err("expected sync or individual".to_owned()),
        }
    }

    pub fn lighting_source(value: &str) -> Result<astracore_core::service::LightingSource, String> {
        match value {
            "sync" => Ok(astracore_core::service::LightingSource::Sync),
            "custom" => Ok(astracore_core::service::LightingSource::Custom),
            _ => Err("expected sync or custom".to_owned()),
        }
    }

    /// `PWM=TEMP:PERCENT,...` for the CPU temperature (`k10temp/Tctl`), default hysteresis.
    pub fn curve(value: &str) -> Result<astracore_core::fan_curve::FanCurve, String> {
        use astracore_core::fan_curve::{CurvePoint, FanCurve, SensorRef};
        let (channel, points) = value
            .split_once('=')
            .ok_or("expected PWM=TEMP:PERCENT,TEMP:PERCENT,...")?;
        let channel: u8 = channel
            .parse()
            .map_err(|_| format!("{channel:?} is not a PWM number"))?;
        let points = points
            .split(',')
            .map(|point| {
                let (celsius, percent) = point
                    .split_once(':')
                    .ok_or_else(|| format!("{point:?} is not TEMP:PERCENT"))?;
                Ok(CurvePoint {
                    celsius: celsius
                        .parse()
                        .map_err(|_| format!("{celsius:?} is not a temperature"))?,
                    percent: percent
                        .parse()
                        .map_err(|_| format!("{percent:?} is not a percentage"))?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(FanCurve {
            channel,
            sensor: SensorRef {
                chip: "k10temp".into(),
                label: "Tctl".into(),
            },
            points,
            hysteresis_celsius: 3.0,
        })
    }

    pub fn on_off(value: &str) -> Result<bool, String> {
        match value {
            "on" => Ok(true),
            "off" => Ok(false),
            _ => Err("expected on or off".to_owned()),
        }
    }

    pub fn rgb(value: &str) -> Result<Rgb, String> {
        astracore_core::lighting::parse_hex(value).map_err(|error| error.to_string())
    }

    /// Hexadecimal byte with an optional `0x` prefix, e.g. `67` or `0x70`.
    pub fn hex_u8(value: &str) -> Result<u8, String> {
        u8::from_str_radix(value.trim_start_matches("0x"), 16)
            .map_err(|_| "expected a hexadecimal byte, e.g. 67 or 0x70".to_owned())
    }

    /// Exactly 126 hexadecimal digits: one 63-byte Aura payload without the Report ID.
    pub fn aura_payload(value: &str) -> Result<aura_hid::Payload, String> {
        decode_hex(value)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| {
                format!(
                    "payload must contain exactly {} hexadecimal digits",
                    aura_hid::PAYLOAD_BYTES * 2
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::parse;
    use astracore_types::SensorValue;

    #[test]
    fn sensor_json_carries_type_and_units() {
        let temperature = serde_json::to_value(SensorValue::Temperature {
            milli_celsius: 42500,
        })
        .unwrap();
        let fan = serde_json::to_value(SensorValue::FanSpeedRpm { rpm: 1200 }).unwrap();
        assert_eq!(
            temperature,
            serde_json::json!({"kind": "temperature", "milli_celsius": 42500})
        );
        assert_eq!(
            fan,
            serde_json::json!({"kind": "fan_speed_rpm", "rpm": 1200})
        );
    }

    #[test]
    fn raw_payload_requires_exact_hex_length() {
        assert_eq!(parse::aura_payload(&"ab".repeat(63)).unwrap(), [0xab; 63]);
        assert!(parse::aura_payload(&"ab".repeat(62)).is_err());
        assert!(parse::aura_payload(&"gg".repeat(63)).is_err());
    }

    #[test]
    fn parses_colors_and_addresses() {
        assert_eq!(parse::rgb("FF8000").unwrap(), [0xff, 0x80, 0]);
        assert!(parse::rgb("FF80").is_err());
        assert_eq!(parse::hex_u8("0x67").unwrap(), 0x67);
        assert_eq!(parse::hex_u8("70").unwrap(), 0x70);
        assert!(parse::hex_u8("100").is_err());
        let curve = parse::curve("2=40:30,60:50,85:100").unwrap();
        assert_eq!((curve.channel, curve.points.len()), (2, 3));
        assert_eq!(curve.points[1].percent, 50);
        assert_eq!(curve.sensor.label, "Tctl");
        for bad in ["", "2", "x=40:30", "2=40", "2=40:x", "2=a:30", "2=40:300"] {
            assert!(parse::curve(bad).is_err(), "{bad}");
        }
        assert_eq!(parse::on_off("off"), Ok(false));
        assert!(parse::lighting_mode("sync").is_ok() && parse::lighting_mode("all").is_err());
        assert!(parse::on_off("1").is_err());
        assert!(parse::from_str::<astracore_core::cpu_profile::Mode>("overclock").is_err());
    }
}
