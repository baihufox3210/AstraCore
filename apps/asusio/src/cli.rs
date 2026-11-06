//! Command-line schema. Colors, addresses and payloads are validated while parsing.
use crate::output::parse;
use asusio_backends::aura_hid;
use asusio_core::{
    cpu_profile::{Epp, Governor, Mode},
    fan_curve::FanCurve,
    lighting::Effect,
    service::{LightingMode, LightingSource},
};
use asusio_types::Rgb;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "asusio",
    version,
    about = "ASUS Hardware I/O Framework for Linux"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Display motherboard information
    Info,

    /// Display hardware sensor readings
    Sensors {
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },

    /// Inspect or control nct6799 fan PWM channels
    Fans {
        #[command(subcommand)]
        action: FanAction,
    },

    /// List detected PCI and USB devices
    Devices {
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,

        /// Include HID devices
        #[arg(long)]
        hid: bool,
    },

    /// Display capabilities inferred from readable sensors and device IDs
    Capabilities {
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },

    /// Inspect a HID report descriptor from sysfs without accessing hidraw
    HidDescriptor {
        /// HID sysfs device ID, as shown by `asusio devices --hid`
        device_id: String,
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },

    /// Inspect the asusiod system service (boot restore, fan curves)
    Daemon {
        #[arg(long, global = true, default_value = asusio_core::service::DEFAULT_SOCKET)]
        socket: PathBuf,
        #[command(subcommand)]
        action: DaemonAction,
    },

    /// CPU performance profiles (governor, EPP, boost, fan preset) through asusiod
    Profile {
        #[arg(long, global = true, default_value = asusio_core::service::DEFAULT_SOCKET)]
        socket: PathBuf,
        #[command(subcommand)]
        action: ProfileAction,
    },

    /// Control lighting through native hardware backends
    Lighting {
        /// Host-local lighting configuration (required explicitly under sudo)
        #[arg(long, global = true)]
        config: Option<PathBuf>,
        #[command(subcommand)]
        action: LightingAction,
    },
}

#[derive(Subcommand)]
pub enum FanAction {
    /// List tachometers and PWM channels without assuming their physical mapping
    List {
        #[arg(long)]
        json: bool,
    },
    /// Preview or set one PWM channel to a manual duty of 30-100%
    Set {
        channel: u8,
        percent: u8,
        #[arg(long)]
        execute: bool,
    },
    /// Preview or restore one PWM channel to firmware Smart Fan IV mode
    Auto {
        channel: u8,
        #[arg(long)]
        execute: bool,
    },
    /// Show fan channels, saved presets and the limits curves must respect (through asusiod)
    Presets {
        #[arg(long, default_value = asusio_core::service::DEFAULT_SOCKET)]
        socket: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Find which fan inputs follow which PWM output: every output that is not a pump is
    /// raised in turn (never lowered) and restored; runs in asusiod and takes about 25
    /// seconds per output
    Detect {
        #[arg(long, default_value = asusio_core::service::DEFAULT_SOCKET)]
        socket: PathBuf,
        /// Start it; without this only the plan is shown
        #[arg(long)]
        execute: bool,
        #[arg(long)]
        json: bool,
    },
    /// Confirm what is connected to a PWM output; an output nobody has identified cannot
    /// be given a curve, because it could be a pump
    Role {
        #[arg(long, default_value = asusio_core::service::DEFAULT_SOCKET)]
        socket: PathBuf,
        pwm: u8,
        /// fan, pump, or none to remove the confirmation
        #[arg(value_enum)]
        role: RoleArg,
    },
    /// Reset to the system defaults: delete every saved preset and return the fans to
    /// firmware control
    Reset {
        #[arg(long, default_value = asusio_core::service::DEFAULT_SOCKET)]
        socket: PathBuf,
        #[arg(long)]
        execute: bool,
        /// Also forget the detection result and the confirmed roles
        #[arg(long)]
        everything: bool,
    },
    /// Save or delete a fan preset through asusiod; no sudo, and a saved preset that the
    /// active profile uses drives the fans at once
    Preset {
        #[arg(long, default_value = asusio_core::service::DEFAULT_SOCKET)]
        socket: PathBuf,
        #[command(subcommand)]
        action: PresetAction,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum RoleArg {
    Fan,
    Pump,
    None,
}

#[derive(Subcommand)]
pub enum PresetAction {
    /// Validate (and with --execute save and apply) a preset
    Set {
        /// 1-32 characters of a-z, 0-9, - or _; `silent` and `performance` are used by
        /// those profiles
        name: String,
        /// PWM=TEMP:PERCENT,TEMP:PERCENT,... for the CPU temperature, e.g.
        /// 2=40:30,60:50,85:100; repeat for more channels
        #[arg(long = "curve", required = true, value_parser = parse::curve)]
        curves: Vec<FanCurve>,
        #[arg(long)]
        execute: bool,
        #[arg(long)]
        json: bool,
    },
    /// Delete a preset saved through asusiod
    Delete {
        name: String,
        #[arg(long)]
        execute: bool,
    },
}

#[derive(Subcommand)]
pub enum LightingAction {
    #[command(flatten)]
    Managed(ManagedAction),
    #[command(flatten)]
    Diagnostic(DiagnosticAction),
}

/// Named targets from the host-local configuration, with calibration.
#[derive(Subcommand)]
pub enum ManagedAction {
    /// Install a host-bound local target manifest without overwriting an existing file
    Setup {
        #[arg(long)]
        from: PathBuf,
    },
    /// List configured aliases and calibration (no device I/O)
    Targets {
        #[arg(long)]
        json: bool,
    },
    /// Preview or set an alias/all with device calibration and brightness
    /// (individual mode)
    Set(SetArgs),
    /// Preview or set one color/effect for every target (Sync mode, through asusiod)
    Sync(SyncArgs),
    /// Preview or switch between Sync and individual mode (through asusiod)
    Mode {
        /// sync or individual
        #[arg(value_parser = parse::lighting_mode)]
        mode: LightingMode,
        #[arg(long)]
        execute: bool,
    },
    /// Preview or set whether one target follows Sync or keeps its own settings (through
    /// asusiod)
    Source {
        target: String,
        /// sync or custom
        #[arg(value_parser = parse::lighting_source)]
        source: LightingSource,
        #[arg(long)]
        execute: bool,
    },
    /// Show every target: kind, recorded settings, effects and readiness (through asusiod)
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Persist RGB channel gains as a hex triplet; FFFFFF resets compensation
    Calibrate {
        target: String,
        #[arg(value_name = "RRGGBB", value_parser = parse::rgb)]
        gains: Rgb,
    },
}

/// Color, brightness, calibration and effect shared by `set` and `sync`.
#[derive(Args)]
pub struct LightArgs {
    #[arg(value_name = "RRGGBB", value_parser = parse::rgb)]
    pub color: Rgb,
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u8).range(0..=100))]
    pub brightness: u8,
    /// Bypass local compensation for comparison; does not change stored calibration
    #[arg(long)]
    pub uncalibrated: bool,
    /// Controller effect: static everywhere; on Aura (board, ARGB) also breathing, strobing,
    /// color_cycle, rainbow, breath_rainbow and starry_night
    #[arg(long, default_value = "static", value_parser = parse::from_str::<Effect>)]
    pub effect: Effect,
}

/// A named lighting request. With `--execute`, it goes through asusiod when that is
/// running (no sudo; recorded for boot restore), otherwise directly to hardware.
#[derive(Args)]
pub struct SetArgs {
    /// Configured alias, or `all`
    pub target: String,
    #[command(flatten)]
    pub light: LightArgs,
    #[arg(long)]
    pub execute: bool,
    /// Write hardware directly even when asusiod is running (not recorded for boot)
    #[arg(long)]
    pub direct: bool,
}

/// Sync mode: one setting for every target, recorded by asusiod.
#[derive(Args)]
pub struct SyncArgs {
    #[command(flatten)]
    pub light: LightArgs,
    #[arg(long)]
    pub execute: bool,
}

#[derive(Subcommand)]
pub enum ProfileAction {
    /// List built-in and custom profiles with their fan presets
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show live CPU state, Tctl, fans and package power
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Preview or apply a profile; applied profiles are restored at boot
    Set(ProfileSetArgs),
}

#[derive(Args)]
pub struct ProfileSetArgs {
    /// silent, balanced, performance or custom
    #[arg(value_parser = parse::from_str::<Mode>)]
    pub mode: Mode,
    /// Custom only: performance or powersave
    #[arg(long, value_parser = parse::from_str::<Governor>)]
    pub governor: Option<Governor>,
    /// Custom only: default, performance, balance_performance, balance_power or power
    #[arg(long, value_parser = parse::from_str::<Epp>)]
    pub epp: Option<Epp>,
    /// Custom only: on or off
    #[arg(long, value_parser = parse::on_off)]
    pub boost: Option<bool>,
    /// Custom only: a fan preset from /etc/asusio/asusiod.json, or `default`
    #[arg(long)]
    pub fan_preset: Option<String>,
    #[arg(long)]
    pub execute: bool,
    #[arg(long)]
    pub json: bool,
}

/// Requests to the asusiod system service; no sudo needed for members of its group.
#[derive(Subcommand)]
pub enum DaemonAction {
    /// Show restore progress and fan-curve state
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Reapply the recorded lighting now (e.g. after logging in)
    Restore,
}

/// Raw per-device commands addressed by explicit IDs; no calibration is applied.
#[derive(Subcommand)]
pub enum DiagnosticAction {
    /// List native USB/HID lighting interfaces without accessing hardware nodes
    List {
        #[arg(long)]
        json: bool,
    },
    /// List I2C adapter metadata without probing hardware
    I2cList,
    /// Identify an ENE controller at an explicit adapter/address (no remapping)
    EneProbe {
        bus: u32,
        /// 7-bit RGB controller address in hexadecimal, e.g. 67 or 70 (not SPD)
        #[arg(value_parser = parse::hex_u8)]
        address: u8,
        #[arg(long)]
        execute: bool,
    },
    /// Read ENE mode and RGB buffers without changing colors (requires pointer selection)
    EneState {
        bus: u32,
        #[arg(value_parser = parse::hex_u8)]
        address: u8,
        #[arg(long)]
        execute: bool,
    },
    /// Static color on an identified ENE GPU/DRAM controller
    EneStatic {
        bus: u32,
        #[arg(value_parser = parse::hex_u8)]
        address: u8,
        #[arg(value_name = "RRGGBB", value_parser = parse::rgb)]
        color: Rgb,
        #[arg(long)]
        execute: bool,
    },
    /// Static color on advertised onboard Aura PWM LED locations
    AuraBoardStatic {
        device_id: String,
        #[arg(value_name = "RRGGBB", value_parser = parse::rgb)]
        color: Rgb,
        #[arg(long, value_delimiter = ',', required = true)]
        locations: Vec<u8>,
        #[arg(long)]
        execute: bool,
    },
    /// Static color on the separate 0416:8050 Universal Screen LED USB device
    ScreenStatic {
        device_id: String,
        #[arg(value_name = "RRGGBB", value_parser = parse::rgb)]
        color: Rgb,
        /// Use the owning lianli-daemon's native IPC instead of claiming USB
        #[arg(long)]
        daemon_socket: Option<PathBuf>,
        #[arg(long)]
        execute: bool,
    },
    /// Static lighting on the Scope II RX vendor HID interface
    KeyboardStatic {
        device_id: String,
        #[arg(value_name = "RRGGBB", value_parser = parse::rgb)]
        color: Rgb,
        #[arg(long)]
        execute: bool,
    },
    /// Preview or send static color reports traced in the official 1cd2 HAL
    AuraStatic {
        device_id: String,
        #[arg(value_name = "RRGGBB", value_parser = parse::rgb)]
        color: Rgb,
        /// ARGB header indexes, e.g. 0,1,2; not hwmon PWM numbers
        #[arg(long, value_delimiter = ',', required = true)]
        headers: Vec<u8>,
        #[arg(long)]
        execute: bool,
    },
    /// Preview or send the vendor-derived Aura configuration query (not a color command)
    AuraConfig {
        device_id: String,
        /// Also query mainboard, each advertised ARGB header, and WDL state
        #[arg(long)]
        details: bool,
        /// Send one configuration query and wait up to two seconds for its response
        #[arg(long)]
        execute: bool,
    },
    /// Validate the Aura HID interface without writing to it
    Probe { device_id: String },
    /// Send a 63-byte vendor payload; requires --execute
    RawOutput {
        device_id: String,
        /// Exactly 126 hexadecimal digits (63 payload bytes, excluding Report ID)
        #[arg(value_parser = parse::aura_payload)]
        payload_hex: aura_hid::Payload,
        /// Actually write to the HID device
        #[arg(long)]
        execute: bool,
    },
}
