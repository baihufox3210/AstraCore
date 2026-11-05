//! Fan presets saved through the socket: validated, stored in the daemon's state and, when
//! the active profile uses them, applied at once. No authentication step beyond the normal
//! per-request policy: the limits in `asusio_core::fan_curve` are what keeps a bad curve
//! from harming the machine.
use crate::daemon::{Daemon, lock};
use asusio_core::{
    cpu_profile::{self, FanPlan, FanPresets, MAX_USER_PRESETS, Mode},
    fan_curve::{
        self, FanCurve, HwmonTemperature, MAX_FULL_SPEED_CELSIUS, MAX_POINTS, MIN_PERCENT,
        Nct6799Pwm, SensorRef, TemperatureSensor,
    },
    service::ipc::{
        FanChannelInfo, FanLimits, FanPresetInfo, FanPresetResult, FanSensorInfo, FanSetup,
        PresetSource,
    },
};
use std::{io, sync::atomic::Ordering};

/// Lowest temperature a curve point may use, and the largest hysteresis (see
/// `FanCurve::validate`).
const MIN_CELSIUS: f64 = -20.0;
const MAX_HYSTERESIS_CELSIUS: f64 = 10.0;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
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
    /// Configuration presets with the user's saved presets on top.
    pub(crate) fn presets(&self) -> FanPresets {
        let mut presets = self.config.fan_presets.clone();
        presets.extend(lock(&self.state).user_fan_presets.clone());
        presets
    }

    /// PWM channels a socket-defined curve may drive: fan headers of the board profile
    /// that the kernel exposes. Without a profile nothing is allowed, because pumps could
    /// not be told apart from fans.
    fn assignable_pwms(&self) -> io::Result<Vec<u8>> {
        let sampler = lock(&self.sampler);
        let profile = sampler.profile().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "fan curves can only be edited on boards with a hardware profile; this board has none",
            )
        })?;
        Ok(profile
            .fan_pwms()
            .into_iter()
            .filter(|pwm| Nct6799Pwm::new(*pwm).is_ok())
            .collect())
    }

    fn custom_preset(&self) -> Option<String> {
        self.recorded_profile()
            .and_then(|profile| profile.custom)
            .and_then(|custom| custom.fan_preset)
    }

    fn preset_infos(&self) -> Vec<FanPresetInfo> {
        let user = lock(&self.state).user_fan_presets.clone();
        let custom = self.custom_preset();
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
                let used_by = used_by(&name, custom.as_deref());
                FanPresetInfo {
                    source: if from_user.is_some() {
                        PresetSource::User
                    } else {
                        PresetSource::Config
                    },
                    overrides_config: from_user.is_some()
                        && self.config.fan_presets.contains_key(&name),
                    curves: from_user
                        .or_else(|| self.config.fan_presets.get(&name))
                        .cloned()
                        .unwrap_or_default(),
                    used_by,
                    name,
                }
            })
            .collect()
    }

    /// Recompute the fan plan of the recorded profile; true when the running curves change.
    /// The fan loop notices the new generation within one interval and restarts.
    fn refresh_fan_plan(&self) -> bool {
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
        let (profile, connected) = {
            let mut sampler = lock(&self.sampler);
            let (connected, _) = sampler.headers();
            (sampler.profile().cloned(), connected)
        };
        let connected: Vec<_> = connected.into_iter().filter_map(|h| h.header).collect();
        let channels: Vec<_> = profile
            .iter()
            .flat_map(|profile| {
                profile.fan_pwms().into_iter().map(|pwm| {
                    let headers: Vec<String> = profile
                        .fan_headers
                        .iter()
                        .filter(|header| header.pwm == Some(pwm))
                        .map(|header| header.header.clone())
                        .collect();
                    FanChannelInfo {
                        pwm,
                        connected: headers.iter().any(|h| connected.contains(h)),
                        headers,
                        available: Nct6799Pwm::new(pwm).is_ok(),
                    }
                })
            })
            .collect();
        let reason = match &profile {
            None => {
                Some("this board has no hardware profile, so pumps cannot be told apart".to_owned())
            }
            Some(_) if !channels.iter().any(|c| c.available) => {
                Some("the kernel exposes no PWM output".to_owned())
            }
            Some(_) => None,
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
        FanSetup {
            board: profile.as_ref().and_then(|p| p.boards.first().cloned()),
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
    /// uses it. A rejected preset changes nothing.
    pub fn set_fan_preset(
        &self,
        name: String,
        curves: Vec<FanCurve>,
        execute: bool,
    ) -> io::Result<FanPresetResult> {
        let assignable = self.assignable_pwms()?;
        cpu_profile::validate_user_preset(&name, &curves, &assignable)?;
        let preview = FanPresetInfo {
            name: name.clone(),
            source: PresetSource::User,
            overrides_config: self.config.fan_presets.contains_key(&name),
            curves: curves.clone(),
            used_by: used_by(&name, self.custom_preset().as_deref()),
        };
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
