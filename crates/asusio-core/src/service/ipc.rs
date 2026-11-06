//! Newline-delimited JSON protocol between clients (CLI, GUI) and `asusiod`.
//! One request and one response per connection.
pub use crate::board::FanKind;
pub use crate::fan_map::{FanRole, RoleSource};
use crate::{
    cpu_profile::{Custom, FanPlan, Mode, Profile},
    fan_curve::FanCurve,
    fan_map::ProbedChannel,
    lighting::{Availability, Effect, EffectInfo, TargetKind},
    service::{LightingMode, LightingSource, LightingState},
};
pub use asusio_backends::nvidia_nvml::{GpuFan, GpuReading};
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
    /// Preview (or, with `execute`, apply and record) whether one target follows the Sync
    /// settings or keeps its own. From individual mode this switches to Sync; targets with
    /// settings of their own keep them.
    SetLightingSource {
        lighting_config: PathBuf,
        target: String,
        source: LightingSource,
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
    /// Fan channels, presets and the limits curves must respect.
    GetFans,
    /// Preview (or, with `execute`, save and apply) a fan preset. Saving takes effect at
    /// once when the active profile uses the preset. Curves are checked against
    /// [`FanLimits`]; a rejected preset changes nothing.
    SetFanPreset {
        name: String,
        curves: Vec<FanCurve>,
        execute: bool,
    },
    /// Preview (or delete) a preset saved through the socket.
    DeleteFanPreset {
        name: String,
        execute: bool,
    },
    /// Preview, or with `execute` start, fan detection: every PWM output that is not a
    /// pump is raised in turn (never lowered) and restored, to see which fan inputs follow
    /// it. It runs in the background; follow it in [`FanSetup::detection`].
    DetectFans {
        execute: bool,
    },
    /// Confirm what is connected to a PWM output (`None` removes the confirmation). A
    /// board profile's role cannot be overridden.
    SetFanRole {
        pwm: u8,
        role: Option<FanRole>,
    },
    /// Preview, or with `execute` perform, a reset to the system defaults: every saved
    /// preset is deleted and the fans go back to firmware control. With `everything` the
    /// detection result and the confirmed roles are forgotten too.
    ResetFans {
        execute: bool,
        everything: bool,
    },
}

impl Request {
    pub fn operation(&self) -> Operation {
        match self {
            Self::Status => Operation::ReadStatus,
            Self::SetLighting { execute: false, .. } => Operation::PreviewLighting,
            Self::SetLighting { execute: true, .. } => Operation::SetLighting,
            Self::RestoreLighting => Operation::RestoreLighting,
            Self::GetLighting { .. } => Operation::ReadStatus,
            Self::SetSync { execute, .. }
            | Self::SetLightingMode { execute, .. }
            | Self::SetLightingSource { execute, .. } => {
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
            Self::GetFans => Operation::ReadStatus,
            Self::SetFanPreset { execute, .. }
            | Self::DeleteFanPreset { execute, .. }
            | Self::DetectFans { execute }
            | Self::ResetFans { execute, .. } => {
                if *execute {
                    Operation::SetFanPreset
                } else {
                    Operation::PreviewFanPreset
                }
            }
            Self::SetFanRole { .. } => Operation::SetFanPreset,
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
    /// Whether the target follows the Sync settings or shows its own.
    #[serde(default)]
    pub source: LightingSource,
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

/// One connected fan or pump header. Headers that never reported a speed are listed
/// in [`Telemetry::unused_headers`] instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanHeaderReading {
    /// `fanN_input` number.
    pub channel: u8,
    /// Board header, e.g. `AIO_PUMP`, when a board profile describes it.
    pub header: Option<String>,
    pub label: Option<String>,
    pub kind: FanKind,
    /// Name of another header on the same PWM output.
    #[serde(default)]
    pub shares_pwm_with: Option<String>,
    pub rpm: u64,
    /// The header's PWM output, when the driver exposes one.
    pub pwm: Option<PwmReading>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpuUsage {
    pub total_percent: f64,
    /// Per logical CPU, in CPU number order.
    pub cores_percent: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuFrequency {
    pub average_mhz: u32,
    pub max_mhz: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryUsage {
    pub used_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabeledValue {
    pub label: String,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Telemetry {
    pub mode: Option<Mode>,
    /// Whether the CPU still matches the recorded profile (`None` without one).
    pub in_sync: Option<bool>,
    pub cpu: CpuSummary,
    pub tctl_celsius: Option<f64>,
    /// Per-CCD temperatures (`Tccd1`, ...), in degrees Celsius.
    pub ccd_celsius: Vec<LabeledValue>,
    pub cpu_usage: Option<CpuUsage>,
    pub cpu_frequency: Option<CpuFrequency>,
    /// Coarse package power average; only available through the root daemon.
    pub package_watts: Option<f64>,
    /// Coarse core power average; only available through the root daemon.
    pub core_watts: Option<f64>,
    pub memory: Option<MemoryUsage>,
    /// Board voltages named and scaled by the board profile, in volts.
    pub voltages: Vec<LabeledValue>,
    /// Active sched_ext scheduler, if one is loaded.
    pub scheduler: Option<String>,
    /// Board described by an embedded profile, if any.
    pub board: Option<String>,
    /// Connected fan and pump headers.
    pub fans: Vec<FanHeaderReading>,
    /// Headers that have not reported a speed (nothing connected).
    pub unused_headers: Vec<String>,
    pub fan: Option<FanPlan>,
    pub gpus: Vec<GpuReading>,
    /// Why GPU readings are missing, e.g. no NVIDIA driver.
    pub gpu_error: Option<String>,
    /// Readings that could not be taken.
    pub errors: Vec<String>,
}

/// A PWM output of the machine's fan controller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanChannelInfo {
    pub pwm: u8,
    /// Every header on this output from the board profile (`CPU_OPT`, `CHA_FAN3`), or the
    /// fan inputs found to follow it (`fan2`, `fan5`), or `pwmN` when nothing is known.
    pub headers: Vec<String>,
    /// Fan inputs known to follow this output.
    pub tachs: Vec<u8>,
    /// At least one of those inputs has reported a speed.
    pub connected: bool,
    /// The kernel exposes this PWM output.
    pub available: bool,
    /// What is connected, once confirmed or known from the board profile.
    pub role: Option<FanRole>,
    pub role_source: Option<RoleSource>,
    /// What detection suggests; applied only when the user confirms it.
    pub suggested_role: Option<FanRole>,
    /// A curve may drive this output: role `fan`, and the kernel exposes it.
    pub assignable: bool,
    /// The last detection of this output, if any.
    pub probe: Option<ProbedChannel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectState {
    Idle,
    Running,
    Finished,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanDetection {
    pub state: DetectState,
    /// Output being tested while running.
    pub pwm: Option<u8>,
    pub done: usize,
    pub total: usize,
    pub message: Option<String>,
}

impl FanDetection {
    pub fn idle() -> Self {
        Self {
            state: DetectState::Idle,
            pwm: None,
            done: 0,
            total: 0,
            message: None,
        }
    }
}

/// One output in a detection preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectPlanItem {
    pub pwm: u8,
    pub baseline_percent: u8,
    pub will_test: bool,
    /// Why it is skipped, when it is.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanDetectResult {
    pub executed: bool,
    pub plan: Vec<DetectPlanItem>,
    pub estimated_seconds: u64,
    pub detection: FanDetection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanResetResult {
    pub executed: bool,
    /// Saved presets that were (or would be) deleted.
    pub removed_presets: Vec<String>,
    /// The detection result and roles were (or would be) forgotten too.
    pub cleared_knowledge: bool,
    /// Outputs handed back to firmware.
    pub released: Vec<u8>,
    /// Outputs that could not be handed back, with the reason. The fan loop and the
    /// service's stop hook try again, but the user should know.
    #[serde(default)]
    pub release_errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanSensorInfo {
    pub chip: String,
    pub label: String,
    pub celsius: Option<f64>,
}

/// What a curve must satisfy; the editor uses it to stay inside the rules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanLimits {
    pub min_percent: u8,
    pub max_points: usize,
    pub max_full_speed_celsius: f64,
    pub min_celsius: f64,
    pub max_hysteresis_celsius: f64,
    pub max_user_presets: usize,
    pub thermal_release_celsius: f64,
    pub thermal_resume_celsius: f64,
    pub sensors: Vec<FanSensorInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetSource {
    /// Defined in `/etc/asusio/asusiod.json`; read-only here.
    Config,
    /// Saved through the socket.
    User,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanPresetInfo {
    pub name: String,
    pub source: PresetSource,
    /// A user preset replaces a configuration preset of the same name.
    pub overrides_config: bool,
    pub curves: Vec<FanCurve>,
    /// Profiles that select this preset.
    pub used_by: Vec<Mode>,
    /// The preset is valid on this machine. A preset saved elsewhere stays saved but is not
    /// applied until its outputs are confirmed as fans here.
    pub usable: bool,
    pub problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanSetup {
    pub board: Option<String>,
    /// hwmon name of the fan chip, e.g. `nct6799`.
    pub controller: Option<String>,
    /// The chip has been checked on real hardware; for others, test with care.
    pub controller_verified: bool,
    /// Neither a board profile nor a detection result describes this machine yet.
    pub needs_detection: bool,
    pub detection: FanDetection,
    /// Unix time of the last detection.
    pub detected_at: Option<u64>,
    /// False when curves cannot be edited, e.g. the board is unknown.
    pub editable: bool,
    pub reason: Option<String>,
    pub channels: Vec<FanChannelInfo>,
    pub presets: Vec<FanPresetInfo>,
    /// Preset the active profile runs, if any.
    pub active_preset: Option<String>,
    /// The over-temperature guard has handed the fans back to firmware.
    pub thermal_hold: bool,
    pub limits: FanLimits,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanPresetResult {
    pub executed: bool,
    /// The saved preset; `None` after a delete.
    pub preset: Option<FanPresetInfo>,
    /// The running curves changed as a result.
    pub applied: bool,
    /// Set when the change is active but could not be saved for the next boot.
    pub persist_error: Option<String>,
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
        let source = r#"{"method":"set_lighting_source","lighting_config":"/c.json","target":"gpu","source":"custom","execute":true}"#;
        assert_eq!(
            read_request(line(source).as_slice()).unwrap().operation(),
            Operation::SetLighting
        );
        let preview = source.replace("true}", "false}");
        assert_eq!(
            read_request(line(&preview).as_slice()).unwrap().operation(),
            Operation::PreviewLighting
        );
        let bad = source.replace("custom", "both");
        assert!(read_request(line(&bad).as_slice()).is_err());
        let get = r#"{"method":"get_lighting","lighting_config":"/c.json"}"#;
        assert_eq!(
            read_request(line(get).as_slice()).unwrap().operation(),
            Operation::ReadStatus
        );
        let effect = r#"{"method":"set_sync","lighting_config":"/c.json","rgb":[0,0,255],"brightness":80,"calibrated":true,"effect":"rainbow","execute":true}"#;
        assert!(
            read_request(line(effect).as_slice()).is_ok(),
            "rainbow is known"
        );
        let unknown = effect.replace("rainbow", "sparkle");
        assert!(read_request(line(&unknown).as_slice()).is_err());
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
