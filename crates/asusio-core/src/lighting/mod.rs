//! Reusable lighting configuration, planning and execution.
//!
//! ```no_run
//! use asusio_core::lighting::{self, Request, Store};
//! # fn main() -> std::io::Result<()> {
//! let store = Store::open(None)?;
//! let plan = store.plan(&Request::new(lighting::ALL, [128, 192, 255]).brightness(50))?;
//! for target in &plan {
//!     println!("{} -> {}", target.alias(), lighting::hex(target.output()));
//! }
//! for outcome in lighting::execute(plan) {
//!     outcome.result?;
//! }
//! # Ok(()) }
//! ```
//!
//! Planning resolves metadata only; hardware changes require explicit [`execute`].
pub mod color;
pub mod config;
pub mod info;
mod runtime;

pub use color::{hex, parse_hex};
pub use config::{Config, Device, Store, Target};
pub use info::{Availability, Effect, EffectInfo, TargetKind};
pub use runtime::{InterfaceKind, NativeInterface, discover_interfaces};

use asusio_types::Rgb;
use color::corrected;
use config::{invalid, neutral};
use std::io;

/// Target selector matching every configured alias.
pub const ALL: &str = "all";

/// What to set: a target alias (or [`ALL`]), a color, and how to adjust it.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    target: &'a str,
    rgb: Rgb,
    pub(crate) brightness: u8,
    pub(crate) calibrated: bool,
    pub(crate) effect: Effect,
}

impl<'a> Request<'a> {
    /// Full brightness with the target's stored white balance applied.
    pub fn new(target: &'a str, rgb: Rgb) -> Self {
        Self {
            target,
            rgb,
            brightness: 100,
            calibrated: true,
            effect: Effect::Static,
        }
    }

    /// Brightness percentage, 0..=100 (validated by [`plan`]).
    pub fn brightness(mut self, percent: u8) -> Self {
        self.brightness = percent;
        self
    }

    /// `false` bypasses stored compensation for this request only.
    pub fn calibrated(mut self, calibrated: bool) -> Self {
        self.calibrated = calibrated;
        self
    }

    /// Hardware effect; [`plan`] rejects effects the target does not support.
    pub fn effect(mut self, effect: Effect) -> Self {
        self.effect = effect;
        self
    }

    pub fn target(&self) -> &str {
        self.target
    }

    pub fn rgb(&self) -> Rgb {
        self.rgb
    }
}

/// A fully resolved operation, including the calibrated output.
/// Fields affecting execution are private so a preview cannot drift from execution.
pub struct PlannedTarget {
    alias: String,
    device: Device,
    endpoint: runtime::Endpoint,
    output: Rgb,
    effect: Effect,
}

impl PlannedTarget {
    pub fn alias(&self) -> &str {
        &self.alias
    }

    pub fn output(&self) -> Rgb {
        self.output
    }
}

/// Validate all selected bindings before any writes.
pub fn plan(config: &Config, request: &Request) -> io::Result<Vec<PlannedTarget>> {
    config.validate(&config::host()?, false)?;
    if request.brightness > 100 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "brightness must be 0..100",
        ));
    }
    let selected = |alias: &str| request.target == ALL || alias == request.target;
    if request.target != ALL && !config.targets.contains_key(request.target) {
        return Err(invalid(format!("unknown target: {}", request.target)));
    }
    config
        .targets
        .iter()
        .filter(|(alias, _)| selected(alias))
        .map(|(alias, configured)| {
            if !info::supports(&configured.device, request.effect) {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("{alias} does not support the {} effect", request.effect),
                ));
            }
            let gains = if request.calibrated {
                configured.white_balance
            } else {
                neutral()
            };
            Ok(PlannedTarget {
                alias: alias.clone(),
                device: configured.device.clone(),
                endpoint: runtime::resolve(&configured.device)?,
                output: corrected(request.rgb, gains, request.brightness),
                effect: request.effect,
            })
        })
        .collect()
}

/// Per-device results retain io::Error for application-specific handling.
/// Other targets continue after failure; no automatic rollback or retry.
pub struct Outcome {
    pub alias: String,
    pub result: io::Result<()>,
}

pub fn execute(plan: Vec<PlannedTarget>) -> Vec<Outcome> {
    plan.into_iter()
        .map(|target| Outcome {
            result: runtime::apply(
                &target.device,
                &target.endpoint,
                target.output,
                target.effect,
            ),
            alias: target.alias,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::save;
    use std::fs;
    #[test]
    fn concurrent_calibration_preserves_both_targets() {
        let directory =
            std::env::temp_dir().join(format!("asusio-concurrent-config-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("lighting.json");
        let mut configuration = config();
        configuration.host_fingerprint = Some(config::host().unwrap());
        configuration
            .targets
            .insert("ram".into(), configuration.targets["gpu"].clone());
        save(&path, &configuration, false).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let workers: Vec<_> = [("gpu", [48, 48, 255]), ("ram", [128, 192, 255])]
            .into_iter()
            .map(|(alias, gains)| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let store = Store::new(path);
                    barrier.wait();
                    for _ in 0..10 {
                        store.calibrate(alias, gains).unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let store = Store::new(&path);
        let saved = store.load().unwrap();
        assert_eq!(saved.targets["gpu"].white_balance, [48, 48, 255]);
        assert_eq!(saved.targets["ram"].white_balance, [128, 192, 255]);
        let original = fs::read(&path).unwrap();
        assert!(store.calibrate("missing", [0; 3]).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn library_rejects_invalid_requests_before_hardware_resolution() {
        let mut config = config();
        config.host_fingerprint = Some(config::host().unwrap());
        assert_eq!(
            plan(&config, &Request::new("gpu", [255; 3]).brightness(101))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(
            plan(&config, &Request::new("unknown", [255; 3]))
                .err()
                .unwrap()
                .to_string()
                .contains("unknown target")
        );
    }
    #[test]
    fn persistent_config_is_private_and_setup_never_overwrites() {
        use std::os::unix::fs::PermissionsExt;
        let directory =
            std::env::temp_dir().join(format!("asusio-config-test-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("lighting.json");
        let mut config = config();
        save(&path, &config, false).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(save(&path, &config, false).is_err());
        config.targets.get_mut("gpu").unwrap().white_balance = [48, 48, 255];
        save(&path, &config, true).unwrap();
        let saved: Config = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.targets["gpu"].white_balance, [48, 48, 255]);
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn compensation_preserves_neutral_targets_and_scales_brightness_without_clipping() {
        assert_eq!(corrected([255; 3], [48, 48, 255], 100), [48, 48, 255]);
        assert_eq!(
            corrected([128, 192, 255], [48, 48, 255], 100),
            [24, 36, 255]
        );
        assert_eq!(corrected([128, 192, 255], neutral(), 100), [128, 192, 255]);
        assert_eq!(corrected([255; 3], [48, 48, 255], 50), [24, 24, 128]);
        assert_eq!(corrected([255; 3], neutral(), 0), [0; 3]);
    }
    fn config() -> Config {
        serde_json::from_value(serde_json::json!({"schema_version":1,"host_fingerprint":"host-a","targets":{"gpu":{"device":{"backend":"ene","adapter_name":"adapter","adapter_parent":"/sys/devices/gpu","address":103,"firmware":"AUMA0-E6K5-0107","led_count":22}}}})).unwrap()
    }
    #[test]
    fn local_configuration_rejects_wrong_host_schema_and_reserved_alias() {
        let mut config = config();
        config.validate("host-a", false).unwrap();
        assert!(config.validate("host-b", false).is_err());
        config.schema_version = 2;
        assert!(config.validate("host-a", false).is_err());
        config.schema_version = 1;
        let target = config.targets.remove("gpu").unwrap();
        config.targets.insert(ALL.into(), target);
        assert!(config.validate("host-a", false).is_err());
    }
    #[test]
    fn calibration_is_per_target_and_survives_serialization() {
        let mut config = config();
        config
            .targets
            .insert("ram".into(), config.targets["gpu"].clone());
        config.targets.get_mut("gpu").unwrap().white_balance = [48, 48, 255];
        let config: Config = serde_json::from_slice(&serde_json::to_vec(&config).unwrap()).unwrap();
        assert_eq!(config.targets["ram"].white_balance, neutral());
        assert_eq!(config.targets["gpu"].white_balance, [48, 48, 255]);
    }
}
