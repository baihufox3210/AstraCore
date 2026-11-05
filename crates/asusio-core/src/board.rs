//! Board-specific hardware descriptions from `hardware/boards/`: which Super I/O fan
//! input belongs to which physical header (and whether it drives a pump), and how the
//! voltage inputs are named and scaled. Profiles match the DMI board name exactly.
use asusio_types::BoardInfo;
use serde::{Deserialize, Serialize};
use std::io;

const PROFILES: &[&str] = &[include_str!(
    "../../../hardware/boards/asus-rog-strix-b850-a-gaming-wifi7-s-neo.json"
)];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FanKind {
    Fan,
    /// A water pump; never driven by fan curves.
    Pump,
    /// No board profile describes this input.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FanHeader {
    /// `fanN_input` number.
    pub tach: u8,
    /// `pwmN` number of the same header.
    pub pwm: Option<u8>,
    /// Silkscreen name, e.g. `AIO_PUMP`.
    pub header: String,
    pub label: String,
    pub kind: FanKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoltageInput {
    /// `inN_input` number.
    pub input: u8,
    pub label: String,
    /// External divider factor applied to the kernel value.
    pub scale: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardProfile {
    pub schema_version: u32,
    pub vendor: String,
    pub boards: Vec<String>,
    /// hwmon chip name the inputs belong to.
    pub controller: String,
    pub sources: Vec<String>,
    pub verification: String,
    pub fan_headers: Vec<FanHeader>,
    pub voltages: Vec<VoltageInput>,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

impl BoardProfile {
    fn parse(json: &str) -> io::Result<Self> {
        let profile: Self = serde_json::from_str(json).map_err(|e| invalid(e.to_string()))?;
        profile.validate()?;
        Ok(profile)
    }

    fn validate(&self) -> io::Result<()> {
        if self.schema_version != 1 || self.boards.is_empty() {
            return Err(invalid("unsupported board profile"));
        }
        let mut tachs: Vec<u8> = self.fan_headers.iter().map(|h| h.tach).collect();
        tachs.sort_unstable();
        tachs.dedup();
        if tachs.len() != self.fan_headers.len() {
            return Err(invalid("duplicate fan input in board profile"));
        }
        if self.fan_headers.iter().any(|h| h.kind == FanKind::Unknown) {
            return Err(invalid("board profiles must state fan or pump"));
        }
        if self
            .voltages
            .iter()
            .any(|v| !(v.scale.is_finite() && (1.0..=20.0).contains(&v.scale)))
        {
            return Err(invalid("voltage scale must be 1..20"));
        }
        Ok(())
    }

    pub fn header_for_tach(&self, tach: u8) -> Option<&FanHeader> {
        self.fan_headers.iter().find(|h| h.tach == tach)
    }

    /// PWM channels that drive pumps; curves must never use them.
    pub fn pump_pwms(&self) -> Vec<u8> {
        self.fan_headers
            .iter()
            .filter(|h| h.kind == FanKind::Pump)
            .filter_map(|h| h.pwm)
            .collect()
    }
}

/// Every embedded profile; a malformed file is a build-time mistake caught by tests.
pub fn profiles() -> Vec<BoardProfile> {
    PROFILES
        .iter()
        .filter_map(|json| BoardProfile::parse(json).ok())
        .collect()
}

/// The profile for this exact board, if one exists.
pub fn find(board: &BoardInfo) -> Option<BoardProfile> {
    profiles()
        .into_iter()
        .find(|p| p.vendor == board.vendor && p.boards.contains(&board.model))
}

/// The profile of the running machine, from DMI.
pub fn detect() -> Option<BoardProfile> {
    asusio_discovery::discover_board()
        .ok()
        .as_ref()
        .and_then(find)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board(model: &str) -> BoardInfo {
        BoardInfo {
            vendor: "ASUSTeK COMPUTER INC.".into(),
            model: model.into(),
            bios_version: None,
        }
    }

    #[test]
    fn embedded_profiles_are_valid() {
        for json in PROFILES {
            BoardProfile::parse(json).unwrap();
        }
        assert_eq!(profiles().len(), PROFILES.len());
    }

    #[test]
    fn b850_a_s_neo_headers_and_pump() {
        let profile = find(&board("ROG STRIX B850-A GAMING WIFI7 S NEO")).unwrap();
        assert_eq!(profile.controller, "nct6799");
        assert_eq!(profile.fan_headers.len(), 7);
        assert_eq!(profile.header_for_tach(1).unwrap().header, "CPU_FAN");
        assert_eq!(profile.header_for_tach(2).unwrap().header, "CPU_OPT");
        let pump = profile.header_for_tach(7).unwrap();
        assert_eq!(
            (pump.header.as_str(), pump.kind),
            ("AIO_PUMP", FanKind::Pump)
        );
        assert_eq!(profile.pump_pwms(), [7]);
        let twelve = profile.voltages.iter().find(|v| v.label == "+12V").unwrap();
        assert_eq!((twelve.input, twelve.scale), (4, 11.98));
    }

    #[test]
    fn only_exact_board_names_match() {
        assert!(find(&board("ROG STRIX B850-A GAMING WIFI")).is_none());
        assert!(find(&board("ROG STRIX B850-A GAMING WIFI7 S NEO ")).is_none());
        let mut other = board("ROG STRIX B850-A GAMING WIFI7 S NEO");
        other.vendor = "Other".into();
        assert!(find(&other).is_none());
    }

    #[test]
    fn rejects_malformed_profiles() {
        let base: serde_json::Value = serde_json::from_str(PROFILES[0]).unwrap();
        let mut duplicate = base.clone();
        duplicate["fan_headers"][1]["tach"] = 1.into();
        assert!(BoardProfile::parse(&duplicate.to_string()).is_err());
        let mut unknown = base.clone();
        unknown["fan_headers"][0]["kind"] = "unknown".into();
        assert!(BoardProfile::parse(&unknown.to_string()).is_err());
        let mut scale = base;
        scale["voltages"][0]["scale"] = 0.into();
        assert!(BoardProfile::parse(&scale.to_string()).is_err());
    }
}
