//! Configuration and persisted state of the `asusiod` system service.
pub mod ipc;

use crate::{fan_curve::FanCurve, lighting::Request, persist};
use asusio_types::Rgb;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

pub const DEFAULT_CONFIG: &str = "/etc/asusio/asusiod.json";
pub const DEFAULT_SOCKET: &str = "/run/asusio/asusiod.sock";
pub const DEFAULT_STATE: &str = "/var/lib/asusio/state.json";
pub const DEFAULT_GROUP: &str = "asusio";

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn default_socket() -> PathBuf {
    DEFAULT_SOCKET.into()
}
fn default_state() -> PathBuf {
    DEFAULT_STATE.into()
}
fn default_true() -> bool {
    true
}
fn default_interval() -> u64 {
    2000
}
fn schema_v1() -> u32 {
    1
}

/// Root-owned daemon configuration. Fan curves are only accepted from this file,
/// never over the socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub schema_version: u32,
    /// Host-local lighting configuration (normally the user's `lighting.json`).
    pub lighting_config: PathBuf,
    /// Group whose members may control lighting through the socket.
    #[serde(default)]
    pub client_group: Option<String>,
    #[serde(default = "default_socket")]
    pub socket: PathBuf,
    #[serde(default = "default_state")]
    pub state_file: PathBuf,
    /// Reapply recorded lighting when the daemon starts (at boot).
    #[serde(default = "default_true")]
    pub restore_lighting: bool,
    #[serde(default)]
    pub fan_curves: Vec<FanCurve>,
    #[serde(default = "default_interval")]
    pub fan_interval_ms: u64,
}

impl ServiceConfig {
    /// A configuration with defaults and no fan curves.
    pub fn new(lighting_config: impl Into<PathBuf>) -> Self {
        Self {
            schema_version: 1,
            lighting_config: lighting_config.into(),
            client_group: Some(DEFAULT_GROUP.to_owned()),
            socket: default_socket(),
            state_file: default_state(),
            restore_lighting: true,
            fan_curves: Vec::new(),
            fan_interval_ms: default_interval(),
        }
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let config: Self = serde_json::from_slice(&fs::read(path)?)
            .map_err(|error| invalid(format!("{}: {error}", path.display())))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.schema_version != 1 {
            return Err(invalid("unsupported asusiod configuration schema"));
        }
        for (name, path) in [
            ("lighting_config", &self.lighting_config),
            ("socket", &self.socket),
            ("state_file", &self.state_file),
        ] {
            if !path.is_absolute() {
                return Err(invalid(format!("{name} must be an absolute path")));
            }
        }
        if self.client_group.as_deref() == Some("") {
            return Err(invalid("client_group must not be empty"));
        }
        if !(500..=10_000).contains(&self.fan_interval_ms) {
            return Err(invalid("fan_interval_ms must be 500..10000"));
        }
        let mut channels = Vec::new();
        for curve in &self.fan_curves {
            curve
                .validate()
                .map_err(|error| invalid(format!("pwm{}: {error}", curve.channel)))?;
            if channels.contains(&curve.channel) {
                return Err(invalid(format!(
                    "pwm{} has more than one fan curve",
                    curve.channel
                )));
            }
            channels.push(curve.channel);
        }
        Ok(())
    }
}

/// Last requested lighting for one alias. Calibration is reapplied at restore time,
/// so later white-balance changes take effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightingState {
    pub rgb: Rgb,
    pub brightness: u8,
    pub calibrated: bool,
}

impl From<&Request<'_>> for LightingState {
    fn from(request: &Request<'_>) -> Self {
        Self {
            rgb: request.rgb(),
            brightness: request.brightness,
            calibrated: request.calibrated,
        }
    }
}

impl LightingState {
    pub fn request<'a>(&self, alias: &'a str) -> Request<'a> {
        Request::new(alias, self.rgb)
            .brightness(self.brightness)
            .calibrated(self.calibrated)
    }
}

/// Desired hardware state recorded by the daemon for restoration at boot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    #[serde(default = "schema_v1")]
    pub schema_version: u32,
    #[serde(default)]
    pub lighting: BTreeMap<String, LightingState>,
}

impl State {
    /// A missing file is an empty state.
    pub fn load(path: &Path) -> io::Result<Self> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        let state: Self = serde_json::from_slice(&bytes)
            .map_err(|error| invalid(format!("{}: {error}", path.display())))?;
        if state.schema_version != 1 {
            return Err(invalid("unsupported asusiod state schema"));
        }
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let state = Self {
            schema_version: 1,
            ..self.clone()
        };
        persist::write_json(path, &state, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_validation() {
        let config: ServiceConfig = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "lighting_config": "/home/user/.config/asusio/lighting.json"
        }))
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.socket, Path::new(DEFAULT_SOCKET));
        assert!(config.restore_lighting && config.fan_curves.is_empty());
        assert_eq!(config.client_group, None);

        let mut relative = config.clone();
        relative.state_file = "state.json".into();
        assert!(relative.validate().is_err());

        let curve: FanCurve = serde_json::from_value(serde_json::json!({
            "channel": 2, "sensor": {"chip": "k10temp", "label": "Tctl"},
            "points": [{"celsius": 40.0, "percent": 30}, {"celsius": 80.0, "percent": 100}]
        }))
        .unwrap();
        let mut duplicate = config.clone();
        duplicate.fan_curves = vec![curve.clone(), curve];
        assert!(duplicate.validate().is_err());
        assert!(ServiceConfig::new("/a/lighting.json").validate().is_ok());
    }

    #[test]
    fn state_round_trips_and_missing_file_is_empty() {
        let directory =
            std::env::temp_dir().join(format!("asusio-state-test-{}", std::process::id()));
        let path = directory.join("state.json");
        assert_eq!(State::load(&path).unwrap(), State::default());
        let mut state = State::default();
        state.lighting.insert(
            "gpu".into(),
            LightingState {
                rgb: [255; 3],
                brightness: 50,
                calibrated: true,
            },
        );
        state.save(&path).unwrap();
        state.save(&path).unwrap();
        let loaded = State::load(&path).unwrap();
        assert_eq!(loaded.lighting, state.lighting);
        assert_eq!(loaded.schema_version, 1);
        let request = loaded.lighting["gpu"].request("gpu");
        assert_eq!((request.target(), request.rgb()), ("gpu", [255; 3]));
        assert_eq!(LightingState::from(&request), loaded.lighting["gpu"]);
        fs::remove_dir_all(directory).unwrap();
    }
}
