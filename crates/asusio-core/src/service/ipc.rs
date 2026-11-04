//! Newline-delimited JSON protocol between clients (CLI, GUI) and `asusiod`.
//! One request and one response per connection.
use crate::{
    cpu_profile::{Custom, FanPlan, Mode, Profile},
    lighting::{Availability, Effect, EffectInfo, TargetKind},
    service::{LightingMode, LightingState},
};
use asusio_policy::Operation;
use asusio_types::Rgb;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

const MAX_REQUEST_BYTES: u64 = 64 * 1024;
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
/// Lighting writes can wait on several two-second HID queries.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Request {
    Status,
    /// Preview (or, with `execute`, apply and record) a lighting request.
    /// `lighting_config` must be an absolute path owned by the requesting user.
    SetLighting {
        lighting_config: PathBuf,
        target: String,
        rgb: Rgb,
        brightness: u8,
        calibrated: bool,
        #[serde(default)]
        effect: Effect,
        execute: bool,
    },
    /// Reapply the recorded lighting state now.
    RestoreLighting,
    /// Every target of `lighting_config` with kind, recorded and applied settings,
    /// supported effects and readiness, plus the current mode and Sync settings.
    GetLighting {
        lighting_config: PathBuf,
    },
    /// Preview (or, with `execute`, apply and record) one setting for every target and
    /// switch to Sync mode. Individual settings are kept.
    SetSync {
        lighting_config: PathBuf,
        rgb: Rgb,
        brightness: u8,
        calibrated: bool,
        #[serde(default)]
        effect: Effect,
        execute: bool,
    },
    /// Preview (or, with `execute`, apply and record) a switch between Sync and
    /// individual mode, using the settings recorded for that mode.
    SetLightingMode {
        lighting_config: PathBuf,
        mode: LightingMode,
        execute: bool,
    },
    /// Built-in and custom CPU profiles, the recorded selection and the CPU state.
    GetProfiles,
    /// Preview (or, with `execute`, apply and record) a CPU profile. For `custom`, the
    /// given settings replace the stored custom profile; when omitted the stored one is
    /// used. Fan presets are selected by name only; curves cannot be sent.
    SetProfile {
        mode: Mode,
        #[serde(default)]
        custom: Option<Custom>,
        execute: bool,
    },
    /// Live readings: CPU state, Tctl, fans, package power and the active profile.
    Telemetry,
}

impl Request {
    pub fn operation(&self) -> Operation {
        match self {
            Self::Status => Operation::ReadStatus,
            Self::SetLighting { execute: false, .. } => Operation::PreviewLighting,
            Self::SetLighting { execute: true, .. } => Operation::SetLighting,
            Self::RestoreLighting => Operation::RestoreLighting,
            Self::GetLighting { .. } => Operation::ReadStatus,
            Self::SetSync { execute, .. } | Self::SetLightingMode { execute, .. } => {
                if *execute {
                    Operation::SetLighting
                } else {
                    Operation::PreviewLighting
                }
            }
            Self::GetProfiles => Operation::ReadStatus,
            Self::SetProfile { execute: false, .. } => Operation::PreviewProfile,
            Self::SetProfile { execute: true, .. } => Operation::SetProfile,
            Self::Telemetry => Operation::ReadTelemetry,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok { data: serde_json::Value },
    Error { message: String },
}

impl Response {
    pub fn from_result<T: Serialize>(result: io::Result<T>) -> Self {
        match result.and_then(|data| serde_json::to_value(data).map_err(io::Error::other)) {
            Ok(data) => Self::Ok { data },
            Err(error) => Self::Error {
                message: error.to_string(),
            },
        }
    }
}

/// Per-target result of a lighting request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetResult {
    pub alias: String,
    pub output: Rgb,
    /// `None` on success. Previews report targets that cannot be resolved.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LightingResult {
    pub executed: bool,
    pub targets: Vec<TargetResult>,
    /// Set when hardware was written but the state for boot restore could not be saved.
    pub persist_error: Option<String>,
    /// Lighting mode after the request.
    #[serde(default)]
    pub mode: LightingMode,
}

/// One configured lighting target as shown by a front end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetStatus {
    pub alias: String,
    pub kind: TargetKind,
    /// Configuration backend, e.g. `aura_argb` or `ene`.
    pub backend: String,
    /// Stored white-balance gains; `FFFFFF` means none.
    pub white_balance: Rgb,
    pub effects: Vec<EffectInfo>,
    /// Individual settings, kept while Sync is active.
    pub individual: Option<LightingState>,
    /// What the current mode applies to this target (`None` if nothing is recorded).
    pub applied: Option<LightingState>,
    pub availability: Availability,
    /// Error of the last write to this target, if it failed.
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LightingReport {
    pub mode: LightingMode,
    pub sync: Option<LightingState>,
    pub lighting_config: PathBuf,
    /// Whether this is the configuration the daemon records and restores.
    pub recorded: bool,
    pub targets: Vec<TargetStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreEntry {
    pub alias: String,
    pub applied: bool,
    pub attempts: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FanState {
    Starting,
    Active {
        celsius: f64,
        percent: u8,
    },
    /// Control was returned to firmware; the curve stays off until the daemon restarts.
    Released {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanStatus {
    pub channel: u8,
    pub sensor: String,
    #[serde(flatten)]
    pub state: FanState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    /// Configuration used for boot restore, once lighting has been recorded.
    pub lighting_config: Option<PathBuf>,
    pub state_file: PathBuf,
    pub restore: Vec<RestoreEntry>,
    /// Curves currently driven by the daemon; channels not listed are under firmware control.
    pub fans: Vec<FanStatus>,
    /// Recorded CPU profile, if any.
    #[serde(default)]
    pub profile: Option<Mode>,
    /// Which fan curves are selected (by the profile, or the defaults).
    #[serde(default)]
    pub fan: Option<FanPlan>,
    /// Last profile application error (boot restore or request).
    #[serde(default)]
    pub profile_error: Option<String>,
}

/// CPU frequency state reduced to what a status display needs. `None` values mean
/// unknown, or different between CPUs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuSummary {
    pub supported: bool,
    /// Why profiles cannot be applied, when unsupported.
    pub reason: Option<String>,
    pub governor: Option<String>,
    pub epp: Option<String>,
    pub boost: Option<bool>,
}

impl CpuSummary {
    /// Whether the observed state equals `profile`'s CPU settings.
    pub fn matches(&self, profile: &Profile) -> bool {
        self.governor.as_deref() == Some(profile.cpu.governor.as_str())
            && self.epp.as_deref() == Some(profile.cpu.epp.as_str())
            && self.boost == Some(profile.cpu.boost)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanReading {
    pub channel: u8,
    pub rpm: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PwmControl {
    /// Driven by an asusiod fan curve.
    Curve,
    /// Manual duty set outside the daemon.
    Manual,
    /// Firmware automatic control.
    Firmware,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PwmReading {
    pub channel: u8,
    pub percent: u8,
    /// Raw `pwmN_enable`.
    pub enable: u8,
    pub control: PwmControl,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Telemetry {
    pub mode: Option<Mode>,
    /// Whether the CPU still matches the recorded profile (`None` without one).
    pub in_sync: Option<bool>,
    pub cpu: CpuSummary,
    pub tctl_celsius: Option<f64>,
    /// Coarse package power average; only available through the root daemon.
    pub package_watts: Option<f64>,
    /// Active sched_ext scheduler, if one is loaded.
    pub scheduler: Option<String>,
    pub fans: Vec<FanReading>,
    pub pwm: Vec<PwmReading>,
    pub fan: Option<FanPlan>,
    /// Readings that could not be taken.
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileReport {
    pub active: Option<Mode>,
    pub in_sync: Option<bool>,
    /// Stored custom settings, if defined.
    pub custom: Option<Custom>,
    /// Resolved built-in profiles, plus custom when it is defined and valid.
    pub profiles: Vec<Profile>,
    /// Names of the fan presets in the daemon configuration.
    pub fan_presets: Vec<String>,
    pub cpu: CpuSummary,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileResult {
    pub executed: bool,
    pub profile: Profile,
    /// State after applying; `None` for previews.
    pub cpu: Option<CpuSummary>,
    /// Set when the profile was applied but could not be recorded for boot restore.
    pub persist_error: Option<String>,
}

fn read_line(reader: impl Read, limit: u64) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    BufReader::new(reader)
        .take(limit + 1)
        .read_until(b'\n', &mut line)?;
    if line.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message exceeds size limit",
        ));
    }
    if line.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incomplete message",
        ));
    }
    Ok(line)
}

fn write_line(mut writer: impl Write, value: &impl Serialize) -> io::Result<()> {
    let mut message = serde_json::to_vec(value).map_err(io::Error::other)?;
    message.push(b'\n');
    writer.write_all(&message)?;
    writer.flush()
}

/// Server side: read one bounded request.
pub fn read_request(reader: impl Read) -> io::Result<Request> {
    serde_json::from_slice(&read_line(reader, MAX_REQUEST_BYTES)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Server side: write one response.
pub fn write_response(writer: impl Write, response: &Response) -> io::Result<()> {
    write_line(writer, response)
}

/// Whether a daemon socket exists at `socket` (without connecting).
pub fn available(socket: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(socket).is_ok_and(|metadata| metadata.file_type().is_socket())
}

/// Client side: send `request` and decode the response data.
pub fn call<T: DeserializeOwned>(socket: &Path, request: &Request) -> io::Result<T> {
    let mut stream = UnixStream::connect(socket).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot connect to asusiod at {}: {error}", socket.display()),
        )
    })?;
    stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
    stream.set_write_timeout(Some(CLIENT_TIMEOUT))?;
    write_line(&mut stream, request)?;
    let response: Response = serde_json::from_slice(&read_line(&stream, MAX_RESPONSE_BYTES)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    match response {
        Response::Ok { data } => serde_json::from_value(data)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
        Response::Error { message } => Err(io::Error::other(format!("asusiod: {message}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wire_format_and_operations() {
        let request = Request::SetLighting {
            lighting_config: "/home/u/.config/asusio/lighting.json".into(),
            target: "all".into(),
            rgb: [1, 2, 3],
            brightness: 50,
            calibrated: true,
            effect: Effect::Static,
            execute: false,
        };
        let mut wire = Vec::new();
        write_line(&mut wire, &request).unwrap();
        assert!(wire.starts_with(br#"{"method":"set_lighting","lighting_config":"#));
        assert_eq!(read_request(wire.as_slice()).unwrap(), request);
        assert_eq!(request.operation(), Operation::PreviewLighting);
        assert_eq!(
            Request::RestoreLighting.operation(),
            Operation::RestoreLighting
        );
        assert!(
            read_request(br#"{"method":"status"}"#.as_slice()).is_err(),
            "no newline"
        );
        assert!(read_request(br#"{"method":"format_disk"}\n"#.as_slice()).is_err());
    }

    #[test]
    fn lighting_mode_requests_and_older_clients() {
        let old = r#"{"method":"set_lighting","lighting_config":"/c.json","target":"gpu","rgb":[1,2,3],"brightness":10,"calibrated":true,"execute":true}"#;
        let Request::SetLighting { effect, .. } = read_request(line(old).as_slice()).unwrap()
        else {
            panic!("set_lighting");
        };
        assert_eq!(
            effect,
            Effect::Static,
            "requests without effect stay static"
        );
        let sync = r#"{"method":"set_sync","lighting_config":"/c.json","rgb":[0,0,255],"brightness":80,"calibrated":true,"effect":"static","execute":true}"#;
        assert_eq!(
            read_request(line(sync).as_slice()).unwrap().operation(),
            Operation::SetLighting
        );
        let mode = r#"{"method":"set_lighting_mode","lighting_config":"/c.json","mode":"individual","execute":false}"#;
        assert_eq!(
            read_request(line(mode).as_slice()).unwrap().operation(),
            Operation::PreviewLighting
        );
        let get = r#"{"method":"get_lighting","lighting_config":"/c.json"}"#;
        assert_eq!(
            read_request(line(get).as_slice()).unwrap().operation(),
            Operation::ReadStatus
        );
        let effect = r#"{"method":"set_sync","lighting_config":"/c.json","rgb":[0,0,255],"brightness":80,"calibrated":true,"effect":"rainbow","execute":true}"#;
        assert!(read_request(line(effect).as_slice()).is_err());
    }

    fn line(json: &str) -> Vec<u8> {
        format!("{json}\n").into_bytes()
    }

    #[test]
    fn profile_requests_select_presets_by_name_only() {
        let custom = r#"{"method":"set_profile","mode":"custom","custom":{"governor":"powersave","epp":"power","boost":false,"fan_preset":"silent"},"execute":true}"#;
        let set = read_request(line(custom).as_slice()).unwrap();
        assert_eq!(set.operation(), Operation::SetProfile);
        let preview = r#"{"method":"set_profile","mode":"performance","execute":false}"#;
        let preview = read_request(line(preview).as_slice()).unwrap();
        assert_eq!(preview.operation(), Operation::PreviewProfile);
        assert_eq!(Request::Telemetry.operation(), Operation::ReadTelemetry);
        assert_eq!(Request::GetProfiles.operation(), Operation::ReadStatus);
        let with_curve = r#"{"method":"set_profile","mode":"custom","custom":{"governor":"powersave","epp":"power","boost":false,"fan_curves":[]},"execute":true}"#;
        assert!(
            read_request(line(with_curve).as_slice()).is_err(),
            "curves cannot be sent"
        );
        let unknown = r#"{"method":"set_profile","mode":"overclock","execute":true}"#;
        assert!(read_request(line(unknown).as_slice()).is_err());
    }

    #[test]
    fn responses_carry_typed_data_or_errors() {
        let ok = Response::from_result(Ok(FanStatus {
            channel: 2,
            sensor: "k10temp/Tctl".into(),
            state: FanState::Active {
                celsius: 50.0,
                percent: 40,
            },
        }));
        let value = serde_json::to_value(&ok).unwrap();
        assert_eq!(value["status"], "ok");
        assert_eq!(value["data"]["state"], "active");
        let error = Response::from_result::<()>(Err(io::Error::other("boom")));
        assert_eq!(serde_json::to_value(&error).unwrap()["message"], "boom");
    }
}
