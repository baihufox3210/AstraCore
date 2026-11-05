//! Fan presets saved through the socket: validated, stored in the daemon's state and, when
//! the active profile uses them, applied at once. No authentication step beyond the normal
//! per-request policy: the limits in `asusio_core::fan_curve` are what keeps a bad curve
//! from harming the machine.
//!
//! Nothing here assumes a particular board. The fan chip's outputs are read from hwmon; what
//! is connected to them comes from the board profile (when one exists), from what the user
//! confirmed, and from the last detection.
use crate::daemon::{Daemon, lock};
use asusio_backends::fan_hwmon;
use asusio_core::{
    cpu_profile::{self, FanPlan, FanPresets, MAX_USER_PRESETS, Mode},
    fan_curve::{
        self, FanCurve, HwmonTemperature, MAX_FULL_SPEED_CELSIUS, MAX_POINTS, MIN_PERCENT,
        SensorRef, TemperatureSensor,
    },
    fan_map::{self, FanRole, RoleSource},
    service::ipc::{
        FanChannelInfo, FanLimits, FanPresetInfo, FanPresetResult, FanSensorInfo, FanSetup,
        PresetSource,
    },
};
use asusio_discovery::sysfs;
use std::{collections::BTreeMap, io, path::Path, sync::atomic::Ordering};

/// Lowest temperature a curve point may use, and the largest hysteresis (see
/// `FanCurve::validate`).
const MIN_CELSIUS: f64 = -20.0;
const MAX_HYSTERESIS_CELSIUS: f64 = 10.0;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

/// The machine's fan chip as the kernel exposes it.
#[derive(Debug, Clone)]
pub(crate) struct FanHardware {
    pub controller: String,
    pub pwms: Vec<u8>,
    pub tachs: Vec<u8>,
}

/// Read the single supported fan chip (Nuvoton `nct6*` or ITE `it8*`) from hwmon.
pub(crate) fn fan_hardware() -> io::Result<FanHardware> {
    let report = fan_hwmon::discover_fan_controllers(Path::new(sysfs::HWMON))?;
    match report.devices.as_slice() {
        [controller] => Ok(FanHardware {
            controller: controller.name.clone(),
            pwms: controller.pwm_channels.iter().map(|c| c.channel).collect(),
            tachs: controller.fan_speeds.iter().map(|f| f.channel).collect(),
        }),
        [] => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no supported fan controller (Nuvoton nct6* or ITE it8*) is exposed by the kernel",
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "more than one fan controller was found; this is not supported",
        )),
    }
}

/// Profiles that select preset `name`: built-in modes name theirs, custom uses its own.
fn used_by(name: &str, custom: Option<&str>) -> Vec<Mode> {
    Mode::ALL
        .into_iter()
        .filter(|mode| match mode.builtin() {
            Some((_, preset)) => preset == Some(name),
            None => custom == Some(name),
        })
        .collect()
}

impl Daemon {
    /// Roles of the outputs that have one: the board profile first, then what the user
    /// confirmed.
    pub(crate) fn roles(&self) -> BTreeMap<u8, (FanRole, RoleSource)> {
        let profile = lock(&self.sampler).profile().cloned();
        let confirmed = lock(&self.state).fan_roles.clone();
        fan_map::effective_roles(profile.as_ref(), &confirmed)
    }

    /// PWM outputs that are pumps; curves must never use them.
    pub(crate) fn pump_pwms(&self) -> Vec<u8> {
        self.roles()
            .into_iter()
            .filter(|(_, (role, _))| *role == FanRole::Pump)
            .map(|(pwm, _)| pwm)
            .collect()
    }

    /// PWM outputs a socket-defined curve may drive: confirmed fans that the kernel exposes.
    /// An output nobody has identified is excluded, because it could be a pump.
    pub(crate) fn assignable_pwms(&self) -> io::Result<Vec<u8>> {
        let hardware = fan_hardware()?;
        let roles = self.roles();
        Ok(hardware
            .pwms
            .into_iter()
            .filter(|pwm| {
                roles
                    .get(pwm)
                    .is_some_and(|(role, _)| *role == FanRole::Fan)
            })
            .collect())
    }

    /// Why a saved preset cannot run here, if it cannot.
    fn preset_problem(
        &self,
        name: &str,
        curves: &[FanCurve],
        assignable: &io::Result<Vec<u8>>,
    ) -> Option<String> {
        match assignable {
            Err(error) => Some(error.to_string()),
            Ok(pwms) => cpu_profile::validate_user_preset(name, curves, pwms)
                .err()
                .map(|error| error.to_string()),
        }
    }

    /// The presets that may drive the fans now: the configuration's (the administrator's own
    /// word) and the saved ones that are valid on this machine. A preset saved on other
    /// hardware stays saved, and simply does not run until its outputs are confirmed here.
    pub(crate) fn presets(&self) -> FanPresets {
        let assignable = self.assignable_pwms();
        let mut presets = self.config.fan_presets.clone();
        let user = lock(&self.state).user_fan_presets.clone();
        for (name, curves) in user {
            if self.preset_problem(&name, &curves, &assignable).is_none() {
                presets.insert(name, curves);
            }
        }
        presets
    }

    fn custom_preset(&self) -> Option<String> {
        self.recorded_profile()
            .and_then(|profile| profile.custom)
            .and_then(|custom| custom.fan_preset)
    }

    fn preset_infos(&self) -> Vec<FanPresetInfo> {
        let user = lock(&self.state).user_fan_presets.clone();
        let custom = self.custom_preset();
        let assignable = self.assignable_pwms();
        let mut names: Vec<_> = self
            .config
            .fan_presets
            .keys()
            .chain(user.keys())
            .cloned()
            .collect();
        names.sort();
        names.dedup();
        names
            .into_iter()
            .map(|name| {
                let from_user = user.get(&name);
                let curves = from_user
                    .or_else(|| self.config.fan_presets.get(&name))
                    .cloned()
                    .unwrap_or_default();
                let problem = from_user.and_then(|c| self.preset_problem(&name, c, &assignable));
                FanPresetInfo {
                    source: if from_user.is_some() {
                        PresetSource::User
                    } else {
                        PresetSource::Config
                    },
                    overrides_config: from_user.is_some()
                        && self.config.fan_presets.contains_key(&name),
                    used_by: used_by(&name, custom.as_deref()),
                    usable: problem.is_none(),
                    problem,
                    curves,
                    name,
                }
            })
            .collect()
    }

    /// Recompute the fan plan of the recorded profile; true when the running curves change.
    /// The fan loop notices the new generation within one interval and restarts.
    pub(crate) fn refresh_fan_plan(&self) -> bool {
        let plan = match self.recorded_profile() {
            Some(recorded) => match self.resolve(recorded.mode, recorded.custom.as_ref()) {
                Ok(profile) => profile.fan,
                Err(error) => {
                    eprintln!("fans: cannot resolve the active profile: {error}");
                    return false;
                }
            },
            None => FanPlan::resolve(None, &self.presets(), &self.config.fan_curves),
        };
        let mut current = lock(&self.fan_plan);
        if current.1 == plan {
            return false;
        }
        *current = (current.0 + 1, plan);
        true
    }

    pub fn fan_setup(&self) -> FanSetup {
        self.sync_fan_hardware();
        let (profile, connected) = {
            let mut sampler = lock(&self.sampler);
            let (connected, _) = sampler.headers();
            (sampler.profile().cloned(), connected)
        };
        let connected_tachs: Vec<u8> = connected.iter().map(|header| header.channel).collect();
        let hardware = fan_hardware();
        let (map, detected_at) = {
            let state = lock(&self.state);
            (
                state.fan_map.clone(),
                state.fan_map.as_ref().map(|m| m.taken_unix),
            )
        };
        let roles = self.roles();

        let channels: Vec<FanChannelInfo> = match &hardware {
            Err(_) => Vec::new(),
            Ok(hardware) => hardware
                .pwms
                .iter()
                .map(|&pwm| {
                    let board_headers: Vec<_> = profile
                        .iter()
                        .flat_map(|p| &p.fan_headers)
                        .filter(|header| header.pwm == Some(pwm))
                        .collect();
                    let probe = map.as_ref().and_then(|m| m.channel(pwm)).cloned();
                    let tachs: Vec<u8> = if !board_headers.is_empty() {
                        board_headers.iter().map(|header| header.tach).collect()
                    } else if let Some(probe) = &probe {
                        probe.tachs.iter().map(|t| t.tach).collect()
                    } else if hardware.tachs.contains(&pwm) {
                        vec![pwm]
                    } else {
                        Vec::new()
                    };
                    let headers = if !board_headers.is_empty() {
                        board_headers.iter().map(|h| h.header.clone()).collect()
                    } else if tachs.is_empty() {
                        vec![format!("pwm{pwm}")]
                    } else {
                        tachs.iter().map(|t| format!("fan{t}")).collect()
                    };
                    let role = roles.get(&pwm).copied();
                    FanChannelInfo {
                        pwm,
                        connected: tachs.iter().any(|t| connected_tachs.contains(t)),
                        headers,
                        tachs,
                        available: true,
                        role: role.map(|(role, _)| role),
                        role_source: role.map(|(_, source)| source),
                        suggested_role: probe.as_ref().and_then(|p| p.suggested_role()),
                        assignable: role.is_some_and(|(role, _)| role == FanRole::Fan),
                        probe,
                    }
                })
                .collect(),
        };
        let reason = match &hardware {
            Err(error) => Some(error.to_string()),
            Ok(_) if channels.iter().any(|c| c.assignable) => None,
            Ok(_) => Some(
                "confirm what is connected to each PWM output before editing curves".to_owned(),
            ),
        };
        let sensors = fan_curve::USER_SENSORS
            .iter()
            .map(|(chip, label)| FanSensorInfo {
                chip: (*chip).to_owned(),
                label: (*label).to_owned(),
                celsius: HwmonTemperature::new(SensorRef {
                    chip: (*chip).to_owned(),
                    label: (*label).to_owned(),
                })
                .read_celsius()
                .ok(),
            })
            .collect();
        let controller = hardware.as_ref().ok().map(|h| h.controller.clone());
        FanSetup {
            board: profile.as_ref().and_then(|p| p.boards.first().cloned()),
            controller_verified: controller
                .as_deref()
                .is_some_and(fan_hwmon::is_verified_controller),
            needs_detection: hardware.is_ok() && profile.is_none() && map.is_none(),
            detection: lock(&self.detect).clone(),
            detected_at,
            controller,
            editable: reason.is_none(),
            reason,
            channels,
            presets: self.preset_infos(),
            active_preset: self.fan_plan().1.preset,
            thermal_hold: self.thermal_hold.load(Ordering::Relaxed),
            limits: FanLimits {
                min_percent: MIN_PERCENT,
                max_points: MAX_POINTS,
                max_full_speed_celsius: MAX_FULL_SPEED_CELSIUS,
                min_celsius: MIN_CELSIUS,
                max_hysteresis_celsius: MAX_HYSTERESIS_CELSIUS,
                max_user_presets: MAX_USER_PRESETS,
                thermal_release_celsius: fan_curve::THERMAL_RELEASE_CELSIUS,
                thermal_resume_celsius: fan_curve::THERMAL_RESUME_CELSIUS,
                sensors,
            },
        }
    }

    /// Validate a preset and, with `execute`, save it and apply it when the active profile
    /// uses it. A rejected preset changes nothing. Saved presets are permanent: they are
    /// never expired, never rewritten by detection, and survive a move to other hardware.
    pub fn set_fan_preset(
        &self,
        name: String,
        curves: Vec<FanCurve>,
        execute: bool,
    ) -> io::Result<FanPresetResult> {
        let assignable = self.assignable_pwms()?;
        if assignable.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no PWM output has been confirmed as a fan yet; run fan detection and confirm the outputs first",
            ));
        }
        cpu_profile::validate_user_preset(&name, &curves, &assignable)?;
        let preview = fan_preset_info(self, &name, &curves);
        if !execute {
            return Ok(FanPresetResult {
                executed: false,
                preset: Some(preview),
                applied: false,
                persist_error: None,
            });
        }
        // Serialized with profile changes, which also replace the fan plan.
        let _serialize = lock(&self.cpu);
        let persist_error = {
            let mut state = lock(&self.state);
            if !state.user_fan_presets.contains_key(&name)
                && state.user_fan_presets.len() >= MAX_USER_PRESETS
            {
                return Err(invalid(format!(
                    "at most {MAX_USER_PRESETS} presets can be saved; delete one first"
                )));
            }
            state.user_fan_presets.insert(name.clone(), curves);
            self.save(&state)
        };
        let applied = self.refresh_fan_plan();
        let preset = self
            .preset_infos()
            .into_iter()
            .find(|info| info.name == name);
        Ok(FanPresetResult {
            executed: true,
            preset,
            applied,
            persist_error,
        })
    }

    /// Delete a preset saved through the socket. A configuration preset of the same name
    /// becomes visible again; a preset the custom profile selects cannot be deleted unless
    /// the configuration still provides it.
    pub fn delete_fan_preset(&self, name: String, execute: bool) -> io::Result<FanPresetResult> {
        let _serialize = lock(&self.cpu);
        if !lock(&self.state).user_fan_presets.contains_key(&name) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no saved fan preset named {name:?}"),
            ));
        }
        if self.custom_preset().as_deref() == Some(name.as_str())
            && !self.config.fan_presets.contains_key(&name)
        {
            return Err(invalid(format!(
                "the custom profile uses fan preset {name:?}; select another preset first"
            )));
        }
        if !execute {
            return Ok(FanPresetResult {
                executed: false,
                preset: None,
                applied: false,
                persist_error: None,
            });
        }
        let persist_error = {
            let mut state = lock(&self.state);
            state.user_fan_presets.remove(&name);
            self.save(&state)
        };
        let applied = self.refresh_fan_plan();
        Ok(FanPresetResult {
            executed: true,
            preset: self
                .preset_infos()
                .into_iter()
                .find(|info| info.name == name),
            applied,
            persist_error,
        })
    }
}

/// What a preset would look like once saved.
fn fan_preset_info(daemon: &Daemon, name: &str, curves: &[FanCurve]) -> FanPresetInfo {
    FanPresetInfo {
        name: name.to_owned(),
        source: PresetSource::User,
        overrides_config: daemon.config.fan_presets.contains_key(name),
        curves: curves.to_vec(),
        used_by: used_by(name, daemon.custom_preset().as_deref()),
        usable: true,
        problem: None,
    }
}
