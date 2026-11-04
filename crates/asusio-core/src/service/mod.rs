//! Configuration and persisted state of the `asusiod` system service.
pub mod ipc;

use crate::{
    cpu_profile::{self, Custom, FanPresets, Mode},
    fan_curve::FanCurve,
    lighting::{Effect, Request},
    persist,
};
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

/// Optional root-owned daemon configuration; a missing file means all defaults.
/// Fan curves are only accepted from this file, never over the socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    #[serde(default = "schema_v1")]
    pub schema_version: u32,
    /// Group whose members may control lighting. Ignored if the group does not exist.
    #[serde(default = "default_group")]
    pub client_group: Option<String>,
    /// Let users with an active local login session control lighting (like udev uaccess).
    #[serde(default = "default_true")]
    pub allow_active_sessions: bool,
    #[serde(default = "default_socket")]
    pub socket: PathBuf,
    #[serde(default = "default_state")]
    pub state_file: PathBuf,
    /// Reapply recorded lighting when the daemon starts (at boot).
    #[serde(default = "default_true")]
    pub restore_lighting: bool,
    /// Default curves, used when no profile is recorded or the profile names no
    /// (defined) preset. Empty means firmware control.
    #[serde(default)]
    pub fan_curves: Vec<FanCurve>,
    /// Named curve sets that CPU profiles may select: `silent` and `performance` are
    /// used by the built-in profiles, any other name only by `custom`. Clients can only
    /// choose a name; curves are defined here only.
    #[serde(default)]
    pub fan_presets: FanPresets,
    #[serde(default = "default_interval")]
    pub fan_interval_ms: u64,
}

fn default_group() -> Option<String> {
    Some(DEFAULT_GROUP.to_owned())
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            schema_version: 1,
            client_group: default_group(),
            allow_active_sessions: true,
            socket: default_socket(),
            state_file: default_state(),
            restore_lighting: true,
            fan_curves: Vec::new(),
            fan_presets: FanPresets::new(),
            fan_interval_ms: default_interval(),
        }
    }
}

impl ServiceConfig {
    /// Load and validate; a missing file yields the defaults.
    pub fn load(path: &Path) -> io::Result<Self> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        let config: Self = serde_json::from_slice(&bytes)
            .map_err(|error| invalid(format!("{}: {error}", path.display())))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.schema_version != 1 {
            return Err(invalid("unsupported asusiod configuration schema"));
        }
        for (name, path) in [("socket", &self.socket), ("state_file", &self.state_file)] {
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
        validate_curves("fan_curves", &self.fan_curves)?;
        for (name, curves) in &self.fan_presets {
            cpu_profile::validate_preset_name(name).map_err(|error| invalid(error.to_string()))?;
            if curves.is_empty() {
                return Err(invalid(format!(
                    "fan preset {name:?} has no curves; omit it to keep firmware control"
                )));
            }
            validate_curves(&format!("fan preset {name:?}"), curves)?;
        }
        Ok(())
    }

    /// Every channel any default or preset curve may drive; all are released on stop.
    pub fn fan_channels(&self) -> Vec<u8> {
        let mut channels: Vec<u8> = self
            .fan_curves
            .iter()
            .chain(self.fan_presets.values().flatten())
            .map(|curve| curve.channel)
            .collect();
        channels.sort_unstable();
        channels.dedup();
        channels
    }
}

fn validate_curves(what: &str, curves: &[FanCurve]) -> io::Result<()> {
    let mut channels = Vec::new();
    for curve in curves {
        curve
            .validate()
            .map_err(|error| invalid(format!("{what}: pwm{}: {error}", curve.channel)))?;
        if channels.contains(&curve.channel) {
            return Err(invalid(format!(
                "{what}: pwm{} has more than one fan curve",
                curve.channel
            )));
        }
        channels.push(curve.channel);
    }
    Ok(())
}

/// Recorded CPU profile selection. The custom settings are kept while a built-in mode
/// is active, so switching back restores them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileState {
    pub mode: Mode,
    #[serde(default)]
    pub custom: Option<Custom>,
}

/// Requested lighting for one alias, or for every alias in Sync mode. Calibration is
/// reapplied at restore time, so later white-balance changes take effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightingState {
    pub rgb: Rgb,
    pub brightness: u8,
    pub calibrated: bool,
    #[serde(default)]
    pub effect: Effect,
}

impl From<&Request<'_>> for LightingState {
    fn from(request: &Request<'_>) -> Self {
        Self {
            rgb: request.rgb(),
            brightness: request.brightness,
            calibrated: request.calibrated,
            effect: request.effect,
        }
    }
}

impl LightingState {
    pub fn request<'a>(&self, alias: &'a str) -> Request<'a> {
        Request::new(alias, self.rgb)
            .brightness(self.brightness)
            .calibrated(self.calibrated)
            .effect(self.effect)
    }
}

/// Whether every target shows the Sync settings, or its own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightingMode {
    /// Each target uses its own recorded settings (the behavior before Sync existed).
    #[default]
    Individual,
    /// Every configured target uses the one Sync setting.
    Sync,
}

/// Desired hardware state recorded by the daemon for restoration at boot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    #[serde(default = "schema_v1")]
    pub schema_version: u32,
    /// The lighting configuration the recorded aliases refer to.
    #[serde(default)]
    pub lighting_config: Option<PathBuf>,
    /// Owner the configuration must still belong to at restore time (`None` for root).
    #[serde(default)]
    pub config_owner: Option<u32>,
    /// Individual settings per alias; kept while Sync is active.
    #[serde(default)]
    pub lighting: BTreeMap<String, LightingState>,
    #[serde(default)]
    pub lighting_mode: LightingMode,
    /// Sync settings; kept while individual mode is active.
    #[serde(default)]
    pub sync: Option<LightingState>,
    /// CPU profile to reapply at boot.
    #[serde(default)]
    pub profile: Option<ProfileState>,
}

impl State {
    /// What the current mode wants `alias` to show, if anything is recorded.
    pub fn desired(&self, alias: &str) -> Option<LightingState> {
        match self.lighting_mode {
            LightingMode::Individual => self.lighting.get(alias).copied(),
            LightingMode::Sync => self.sync,
        }
    }

    /// Aliases the current mode restores: the recorded ones individually, or every
    /// configured alias in Sync mode.
    pub fn restore_aliases<'a>(
        &self,
        configured: impl IntoIterator<Item = &'a str>,
    ) -> Vec<String> {
        match (self.lighting_mode, self.sync) {
            (LightingMode::Individual, _) => self.lighting.keys().cloned().collect(),
            (LightingMode::Sync, Some(_)) => configured.into_iter().map(str::to_owned).collect(),
            (LightingMode::Sync, None) => Vec::new(),
        }
    }

    /// Point the recorded aliases at `config`. Individual settings belong to one
    /// configuration, so they are dropped when it changes; Sync settings are not.
    pub fn bind_lighting_config(&mut self, config: PathBuf, owner: Option<u32>) {
        if self.lighting_config.as_ref() != Some(&config) {
            self.lighting.clear();
        }
        self.lighting_config = Some(config);
        self.config_owner = owner;
    }

    /// Switch to `mode`. Leaving Sync fills each configured alias without individual
    /// settings with the Sync settings, so every target keeps what it shows now and is
    /// restored at boot; existing individual settings are never overwritten.
    pub fn switch_lighting_mode<'a>(
        &mut self,
        mode: LightingMode,
        configured: impl IntoIterator<Item = &'a str>,
    ) -> io::Result<()> {
        if mode == LightingMode::Sync && self.sync.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no Sync settings recorded; set them with `lighting sync` first",
            ));
        }
        if mode == LightingMode::Individual
            && self.lighting_mode == LightingMode::Sync
            && let Some(sync) = self.sync
        {
            for alias in configured {
                self.lighting.entry(alias.to_owned()).or_insert(sync);
            }
        }
        self.lighting_mode = mode;
        Ok(())
    }

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
        let missing = Path::new("/nonexistent/asusiod.json");
        let config = ServiceConfig::load(missing).unwrap();
        config.validate().unwrap();
        assert_eq!(config.socket, Path::new(DEFAULT_SOCKET));
        assert!(config.restore_lighting && config.allow_active_sessions);
        assert!(config.fan_curves.is_empty());
        assert_eq!(config.client_group.as_deref(), Some(DEFAULT_GROUP));

        let parsed: ServiceConfig =
            serde_json::from_value(serde_json::json!({"allow_active_sessions": false})).unwrap();
        assert!(!parsed.allow_active_sessions && parsed.schema_version == 1);

        let mut relative = config.clone();
        relative.state_file = "state.json".into();
        assert!(relative.validate().is_err());

        let curve: FanCurve = serde_json::from_value(serde_json::json!({
            "channel": 2, "sensor": {"chip": "k10temp", "label": "Tctl"},
            "points": [{"celsius": 40.0, "percent": 30}, {"celsius": 80.0, "percent": 100}]
        }))
        .unwrap();
        let mut duplicate = config.clone();
        duplicate.fan_curves = vec![curve.clone(), curve.clone()];
        assert!(duplicate.validate().is_err());

        let mut presets = config.clone();
        presets.fan_curves = vec![curve.clone()];
        let mut quiet = curve.clone();
        quiet.channel = 3;
        presets
            .fan_presets
            .insert("silent".into(), vec![quiet.clone()]);
        presets.validate().unwrap();
        assert_eq!(presets.fan_channels(), [2, 3]);
        presets
            .fan_presets
            .insert("Bad Name".into(), vec![quiet.clone()]);
        assert!(presets.validate().is_err());
        presets.fan_presets.remove("Bad Name");
        presets.fan_presets.insert("empty".into(), Vec::new());
        assert!(presets.validate().is_err());
        presets.fan_presets.remove("empty");
        quiet.points[1].percent = 90;
        presets.fan_presets.insert("unsafe".into(), vec![quiet]);
        assert!(
            presets.validate().is_err(),
            "preset curves must end at 100%"
        );
    }

    #[test]
    fn older_state_files_load_as_individual_static() {
        let old = br#"{"schema_version":1,"lighting_config":"/home/u/.config/asusio/lighting.json","config_owner":1000,"lighting":{"gpu":{"rgb":[255,255,255],"brightness":100,"calibrated":true}}}"#;
        let state: State = serde_json::from_slice(old).unwrap();
        assert_eq!(state.lighting_mode, LightingMode::Individual);
        assert_eq!(state.lighting["gpu"].effect, Effect::Static);
        assert_eq!(state.sync, None);
    }

    #[test]
    fn modes_keep_their_own_settings() {
        let individual = LightingState {
            rgb: [255, 0, 0],
            brightness: 100,
            calibrated: true,
            effect: Effect::Static,
        };
        let sync = LightingState {
            rgb: [0, 0, 255],
            brightness: 40,
            ..individual
        };
        let mut state = State::default();
        state.bind_lighting_config("/a/lighting.json".into(), Some(1000));
        state.lighting.insert("gpu".into(), individual);
        state.sync = Some(sync);
        let configured = ["board", "gpu"];
        assert_eq!(state.desired("gpu"), Some(individual));
        assert_eq!(state.desired("board"), None);
        assert_eq!(state.restore_aliases(configured), ["gpu"]);
        state.lighting_mode = LightingMode::Sync;
        assert_eq!(state.desired("board"), Some(sync));
        assert_eq!(state.restore_aliases(configured), ["board", "gpu"]);
        state.lighting_mode = LightingMode::Individual;
        assert_eq!(
            state.desired("gpu"),
            Some(individual),
            "Sync did not overwrite it"
        );

        state.bind_lighting_config("/a/lighting.json".into(), Some(1000));
        assert_eq!(state.lighting.len(), 1, "same configuration keeps entries");
        state.bind_lighting_config("/b/lighting.json".into(), Some(1000));
        assert!(state.lighting.is_empty() && state.sync == Some(sync));
        state.lighting_mode = LightingMode::Sync;
        state.sync = None;
        assert!(state.restore_aliases(configured).is_empty());
    }

    #[test]
    fn leaving_sync_fills_only_missing_individual_settings() {
        let red = LightingState {
            rgb: [255, 0, 0],
            brightness: 100,
            calibrated: true,
            effect: Effect::Static,
        };
        let blue = LightingState {
            rgb: [0, 0, 255],
            ..red
        };
        let mut state = State::default();
        assert!(
            state
                .switch_lighting_mode(LightingMode::Sync, ["gpu"])
                .is_err(),
            "Sync needs recorded settings"
        );
        state.lighting.insert("gpu".into(), red);
        state.sync = Some(blue);
        state
            .switch_lighting_mode(LightingMode::Sync, ["gpu", "board"])
            .unwrap();
        assert_eq!(
            state.lighting.len(),
            1,
            "entering Sync changes nothing else"
        );
        state
            .switch_lighting_mode(LightingMode::Individual, ["gpu", "board"])
            .unwrap();
        assert_eq!(state.lighting["gpu"], red, "kept");
        assert_eq!(state.lighting["board"], blue, "filled from Sync");
        assert_eq!(state.sync, Some(blue), "Sync settings kept");
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
                effect: Effect::Static,
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
        assert_eq!(loaded.profile, None, "older state files have no profile");
        state.profile = Some(ProfileState {
            mode: Mode::Silent,
            custom: Some(Custom::default()),
        });
        state.save(&path).unwrap();
        assert_eq!(State::load(&path).unwrap().profile, state.profile);
        state.lighting_mode = LightingMode::Sync;
        state.sync = Some(LightingState {
            rgb: [1, 2, 3],
            ..state.lighting["gpu"]
        });
        state.save(&path).unwrap();
        let loaded = State::load(&path).unwrap();
        assert_eq!(
            (loaded.lighting_mode, loaded.sync),
            (LightingMode::Sync, state.sync)
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
