//! Named CPU performance profiles: built-in modes plus one user-defined custom mode.
//!
//! A profile combines CPU settings for [`cpu_pstate`] with an optional fan-curve preset.
//! Presets are only defined in the root-owned daemon configuration; a profile can name
//! one, but never carries curve points itself. A missing preset leaves the default
//! curves (normally none, i.e. firmware control) in place.
use crate::fan_curve::FanCurve;
pub use astracore_backends::cpu_pstate::{self, Epp, Governor, Settings};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, io, str::FromStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Silent,
    Balanced,
    Performance,
    Custom,
}

impl Mode {
    pub const ALL: [Self; 4] = [
        Self::Silent,
        Self::Balanced,
        Self::Performance,
        Self::Custom,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Silent => "silent",
            Self::Balanced => "balanced",
            Self::Performance => "performance",
            Self::Custom => "custom",
        }
    }

    /// CPU settings and preferred fan preset of a built-in mode; `None` for custom.
    pub fn builtin(self) -> Option<(Settings, Option<&'static str>)> {
        let settings = |governor, epp, boost| Settings {
            governor,
            epp,
            boost,
        };
        Some(match self {
            Self::Silent => (
                settings(Governor::Powersave, Epp::Power, false),
                Some("silent"),
            ),
            Self::Balanced => (
                settings(Governor::Powersave, Epp::BalancePerformance, true),
                Some("balanced"),
            ),
            Self::Performance => (
                settings(Governor::Powersave, Epp::Performance, true),
                Some("performance"),
            ),
            Self::Custom => return None,
        })
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

impl FromStr for Mode {
    type Err = io::Error;
    fn from_str(value: &str) -> io::Result<Self> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.as_str() == value)
            .ok_or_else(|| {
                invalid(format!(
                    "unknown profile {value:?}; expected silent, balanced, performance or custom"
                ))
            })
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

/// The user's custom mode. `fan_preset: None` keeps the default curves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Custom {
    pub governor: Governor,
    pub epp: Epp,
    pub boost: bool,
    #[serde(default)]
    pub fan_preset: Option<String>,
}

impl Custom {
    pub fn settings(&self) -> Settings {
        Settings {
            governor: self.governor,
            epp: self.epp,
            boost: self.boost,
        }
    }
}

impl Default for Custom {
    /// Starts from the balanced mode's CPU settings.
    fn default() -> Self {
        let (settings, _) = Mode::Balanced.builtin().expect("balanced is built in");
        Self {
            governor: settings.governor,
            epp: settings.epp,
            boost: settings.boost,
            fan_preset: None,
        }
    }
}

/// Fan presets from the daemon configuration, keyed by name.
pub type FanPresets = BTreeMap<String, Vec<FanCurve>>;

/// Preset names are short identifiers so they can be shown and passed around safely.
pub fn validate_preset_name(name: &str) -> io::Result<()> {
    let valid = (1..=32).contains(&name.len())
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(&byte)
        });
    if valid {
        Ok(())
    } else {
        Err(invalid(format!(
            "fan preset name {name:?} must be 1-32 characters of a-z, 0-9, - or _"
        )))
    }
}

/// Most presets a user may define through the socket.
pub const MAX_USER_PRESETS: usize = 16;

/// Check a preset arriving over the socket: a valid name, at least one curve, every curve
/// passing [`FanCurve::validate_user`], and no channel used twice.
pub fn validate_user_preset(name: &str, curves: &[FanCurve], assignable: &[u8]) -> io::Result<()> {
    validate_preset_name(name)?;
    if curves.is_empty() {
        return Err(invalid(
            "a fan preset needs at least one curve; delete it instead",
        ));
    }
    let mut channels = Vec::new();
    for curve in curves {
        curve
            .validate_user(assignable)
            .map_err(|error| invalid(format!("pwm{}: {error}", curve.channel)))?;
        if channels.contains(&curve.channel) {
            return Err(invalid(format!(
                "pwm{} has more than one curve",
                curve.channel
            )));
        }
        channels.push(curve.channel);
    }
    Ok(())
}

/// Which fan curves a profile ends up running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanPlan {
    /// Preset the profile asked for, if any.
    pub requested: Option<String>,
    /// Preset actually used; `None` means the default `fan_curves`.
    pub preset: Option<String>,
    /// No curve at all: every channel stays under firmware control.
    pub firmware: bool,
    /// Why the requested preset is not used, e.g. it is not defined.
    pub note: Option<String>,
    /// Curves stay inside the daemon; clients only see the names above.
    #[serde(skip)]
    pub curves: Vec<FanCurve>,
}

impl FanPlan {
    pub fn resolve(requested: Option<&str>, presets: &FanPresets, default: &[FanCurve]) -> Self {
        let (preset, curves, note) = match requested.map(|name| (name, presets.get(name))) {
            Some((name, Some(curves))) => (Some(name.to_owned()), curves.clone(), None),
            Some((name, None)) => (
                None,
                default.to_vec(),
                Some(format!(
                    "fan preset {name:?} is not defined in the daemon configuration"
                )),
            ),
            None => (None, default.to_vec(), None),
        };
        Self {
            requested: requested.map(str::to_owned),
            preset,
            firmware: curves.is_empty(),
            note,
            curves,
        }
    }
}

/// A fully resolved profile ready to apply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub mode: Mode,
    #[serde(flatten)]
    pub cpu: Settings,
    pub fan: FanPlan,
}

/// Resolve `mode` (with the stored or requested custom settings) against the presets.
/// Fails for custom without settings, invalid CPU combinations, or a custom fan preset
/// that does not exist; built-in modes fall back to the default curves instead.
pub fn resolve(
    mode: Mode,
    custom: Option<&Custom>,
    presets: &FanPresets,
    default: &[FanCurve],
) -> io::Result<Profile> {
    let (cpu, preset) = match mode.builtin() {
        Some((cpu, preset)) => (cpu, preset.map(str::to_owned)),
        None => {
            let custom = custom.ok_or_else(|| invalid("no custom profile has been defined"))?;
            if let Some(name) = &custom.fan_preset {
                validate_preset_name(name)?;
                if !presets.contains_key(name) {
                    return Err(invalid(format!(
                        "fan preset {name:?} is not defined in the daemon configuration"
                    )));
                }
            }
            (custom.settings(), custom.fan_preset.clone())
        }
    };
    cpu.validate()?;
    let mut fan = FanPlan::resolve(preset.as_deref(), presets, default);
    // A built-in mode without a saved preset is the normal case (its fans stay under
    // firmware control), not something to warn about.
    if mode.builtin().is_some() && fan.preset.is_none() {
        fan.note = None;
    }
    Ok(Profile { mode, cpu, fan })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve(channel: u8) -> FanCurve {
        serde_json::from_value(serde_json::json!({
            "channel": channel, "sensor": {"chip": "k10temp", "label": "Tctl"},
            "points": [{"celsius": 40.0, "percent": 30}, {"celsius": 80.0, "percent": 100}]
        }))
        .unwrap()
    }

    #[test]
    fn builtin_modes_match_the_specification() {
        let cpu = |mode: Mode| mode.builtin().unwrap().0;
        let settings = |governor, epp, boost| Settings {
            governor,
            epp,
            boost,
        };
        assert_eq!(
            cpu(Mode::Silent),
            settings(Governor::Powersave, Epp::Power, false)
        );
        assert_eq!(
            cpu(Mode::Balanced),
            settings(Governor::Powersave, Epp::BalancePerformance, true)
        );
        assert_eq!(
            cpu(Mode::Performance),
            settings(Governor::Powersave, Epp::Performance, true)
        );
        assert!(Mode::Custom.builtin().is_none());
        for mode in Mode::ALL {
            assert_eq!(mode.as_str().parse::<Mode>().unwrap(), mode);
            if let Some((cpu, _)) = mode.builtin() {
                cpu.validate().unwrap();
            }
        }
        assert_eq!(
            serde_json::to_value(Mode::Performance).unwrap(),
            "performance"
        );
        assert!("turbo".parse::<Mode>().is_err(), "merged into performance");
        assert!("overclock".parse::<Mode>().is_err());
        assert_eq!(format!("{:<8}|", Mode::Silent), "silent  |", "honors width");
    }

    #[test]
    fn socket_presets_are_validated_as_a_whole() {
        let mut good = curve(2);
        good.points[1].celsius = 80.0;
        validate_user_preset("quiet", &[good.clone(), curve(4)], &[2, 4]).unwrap();
        assert!(
            validate_user_preset("Quiet!", &[good.clone()], &[2, 4]).is_err(),
            "name"
        );
        assert!(
            validate_user_preset("quiet", &[], &[2, 4]).is_err(),
            "empty"
        );
        assert!(
            validate_user_preset("quiet", &[good.clone(), good.clone()], &[2, 4]).is_err(),
            "duplicate channel"
        );
        assert!(
            validate_user_preset("quiet", &[good], &[4]).is_err(),
            "channel not assignable"
        );
    }

    #[test]
    fn missing_presets_fall_back_to_firmware() {
        let presets = FanPresets::new();
        for mode in [Mode::Silent, Mode::Balanced, Mode::Performance] {
            let profile = resolve(mode, None, &presets, &[]).unwrap();
            assert!(
                profile.fan.firmware && profile.fan.preset.is_none(),
                "{mode}"
            );
            assert!(
                profile.fan.note.is_none(),
                "{mode}: no preset is the normal case"
            );
            assert_eq!(profile.fan.requested.as_deref(), mode.builtin().unwrap().1);
        }
        let balanced = FanPresets::from([("balanced".to_owned(), vec![curve(2)])]);
        let profile = resolve(Mode::Balanced, None, &balanced, &[]).unwrap();
        assert_eq!(profile.fan.preset.as_deref(), Some("balanced"));
        let with_default = resolve(Mode::Balanced, None, &presets, &[curve(2)]).unwrap();
        assert!(!with_default.fan.firmware);
    }

    #[test]
    fn presets_are_selected_by_name_only() {
        let presets = FanPresets::from([("performance".to_owned(), vec![curve(2), curve(3)])]);
        let performance = resolve(Mode::Performance, None, &presets, &[]).unwrap();
        assert_eq!(performance.fan.preset.as_deref(), Some("performance"));
        assert_eq!(performance.fan.curves.len(), 2);
        let wire = serde_json::to_value(&performance).unwrap();
        assert!(
            wire["fan"].get("curves").is_none(),
            "curves never leave the daemon"
        );
        assert_eq!(wire["epp"], "performance");

        let mut custom = Custom {
            fan_preset: Some("nope".into()),
            ..Custom::default()
        };
        assert!(resolve(Mode::Custom, Some(&custom), &presets, &[]).is_err());
        custom.fan_preset = Some("performance".into());
        let resolved = resolve(Mode::Custom, Some(&custom), &presets, &[]).unwrap();
        assert_eq!(resolved.fan.curves.len(), 2);
        assert!(resolve(Mode::Custom, None, &presets, &[]).is_err());
        custom.governor = Governor::Performance;
        assert!(
            resolve(Mode::Custom, Some(&custom), &presets, &[]).is_err(),
            "EPP must be performance"
        );
        assert!(validate_preset_name("Silent!").is_err());
        let with_points = r#"{"governor":"powersave","epp":"power","boost":true,"points":[]}"#;
        assert!(serde_json::from_str::<Custom>(with_points).is_err());
    }
}
