//! CPU profiles: apply, record, restore at boot, and live telemetry.
use crate::daemon::{Daemon, lock};
use asusio_core::{
    cpu_profile::{self, Custom, Mode, Profile, cpu_pstate},
    service::{
        ProfileState,
        ipc::{CpuSummary, FanState, ProfileReport, ProfileResult, PwmControl, Telemetry},
    },
};
use asusio_discovery::sysfs;
use std::{io, path::Path};

impl Daemon {
    pub(crate) fn resolve(&self, mode: Mode, custom: Option<&Custom>) -> io::Result<Profile> {
        cpu_profile::resolve(mode, custom, &self.presets(), &self.config.fan_curves)
    }

    pub(crate) fn recorded_profile(&self) -> Option<ProfileState> {
        lock(&self.state).profile.clone()
    }

    fn in_sync(&self, cpu: &CpuSummary) -> Option<bool> {
        let recorded = self.recorded_profile()?;
        let profile = self.resolve(recorded.mode, recorded.custom.as_ref()).ok()?;
        Some(cpu.matches(&profile))
    }

    pub fn profiles(&self) -> ProfileReport {
        let recorded = self.recorded_profile();
        let custom = recorded.as_ref().and_then(|p| p.custom.clone());
        let profiles = Mode::ALL
            .into_iter()
            .filter_map(|mode| self.resolve(mode, custom.as_ref()).ok())
            .collect();
        let cpu = CpuSummary::from(&cpu_pstate::inspect(Path::new(sysfs::CPU)));
        ProfileReport {
            active: recorded.map(|p| p.mode),
            in_sync: self.in_sync(&cpu),
            custom,
            profiles,
            fan_presets: self.presets().keys().cloned().collect(),
            cpu,
            last_error: lock(&self.profile_error).clone(),
        }
    }

    /// Apply the CPU settings, then switch fan curves and record the selection. A CPU
    /// failure (already rolled back by the backend) changes nothing else.
    fn apply_profile(
        &self,
        profile: &Profile,
        custom: Option<Custom>,
    ) -> io::Result<ProfileResult> {
        let _cpu = lock(&self.cpu);
        let applied = match cpu_pstate::apply(Path::new(sysfs::CPU), &profile.cpu) {
            Ok(state) => state,
            Err(error) => {
                let message = format!("{}: {error}", profile.mode);
                *lock(&self.profile_error) = Some(message.clone());
                return Err(io::Error::new(error.kind(), message));
            }
        };
        *lock(&self.profile_error) = None;
        {
            let mut plan = lock(&self.fan_plan);
            if plan.1 != profile.fan {
                *plan = (plan.0 + 1, profile.fan.clone());
            }
        }
        let persist_error = {
            let mut state = lock(&self.state);
            let custom = custom.or_else(|| state.profile.as_ref().and_then(|p| p.custom.clone()));
            state.profile = Some(ProfileState {
                mode: profile.mode,
                custom,
            });
            state.save(&self.config.state_file).err()
        }
        .map(|error| error.to_string());
        Ok(ProfileResult {
            executed: true,
            profile: profile.clone(),
            cpu: Some(CpuSummary::from(&Ok(applied))),
            persist_error,
        })
    }

    /// Preview, or with `execute` apply and record, a CPU profile.
    pub fn set_profile(
        &self,
        mode: Mode,
        custom: Option<Custom>,
        execute: bool,
    ) -> io::Result<ProfileResult> {
        if custom.is_some() && mode != Mode::Custom {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "custom settings can only be given with the custom profile",
            ));
        }
        let stored = self.recorded_profile().and_then(|p| p.custom);
        let profile = self.resolve(mode, custom.as_ref().or(stored.as_ref()))?;
        if !execute {
            return Ok(ProfileResult {
                executed: false,
                profile,
                cpu: None,
                persist_error: None,
            });
        }
        self.apply_profile(&profile, custom)
    }

    /// Boot restore of the recorded profile. Without one, the CPU is left untouched.
    pub fn restore_profile_at_startup(&self) {
        let Some(recorded) = self.recorded_profile() else {
            eprintln!("profile: none recorded; CPU settings left unchanged");
            return;
        };
        let result = self
            .resolve(recorded.mode, recorded.custom.as_ref())
            .and_then(|profile| self.apply_profile(&profile, None));
        match result {
            Ok(result) => eprintln!("profile: {} restored", result.profile.mode),
            Err(error) => {
                eprintln!("profile: restore failed: {error}");
                *lock(&self.profile_error) = Some(error.to_string());
            }
        }
    }

    pub fn telemetry(&self) -> Telemetry {
        let (map, confirmed) = {
            let state = lock(&self.state);
            (state.fan_map.clone(), state.fan_roles.clone())
        };
        // Which output each fan input follows, as detection saw it.
        let wiring: std::collections::BTreeMap<u8, u8> = map
            .iter()
            .flat_map(|map| &map.channels)
            .flat_map(|channel| channel.tachs.iter().map(|t| (t.tach, channel.pwm)))
            .collect();
        let (mut telemetry, profile) = {
            let mut sampler = lock(&self.sampler);
            sampler.set_wiring(wiring);
            (sampler.read(), sampler.profile().cloned())
        };
        let roles = asusio_core::fan_map::effective_roles(profile.as_ref(), &confirmed);
        let names: Vec<(u8, String)> = telemetry
            .fans
            .iter()
            .map(|h| {
                (
                    h.channel,
                    h.header
                        .clone()
                        .unwrap_or_else(|| format!("fan{}", h.channel)),
                )
            })
            .collect();
        for header in &mut telemetry.fans {
            let pwm = header.pwm.as_ref().map(|p| p.channel);
            if header.kind == asusio_core::service::ipc::FanKind::Unknown
                && let Some((role, _)) = pwm.and_then(|p| roles.get(&p))
            {
                header.kind = match role {
                    asusio_core::fan_map::FanRole::Fan => asusio_core::service::ipc::FanKind::Fan,
                    asusio_core::fan_map::FanRole::Pump => asusio_core::service::ipc::FanKind::Pump,
                };
            }
            // Another fan input on the same output, when detection saw one.
            if header.shares_pwm_with.is_none()
                && let Some(channel) = pwm.and_then(|p| map.as_ref().and_then(|m| m.channel(p)))
            {
                header.shares_pwm_with = channel
                    .tachs
                    .iter()
                    .filter(|t| t.tach != header.channel)
                    .find_map(|t| names.iter().find(|(tach, _)| *tach == t.tach))
                    .map(|(_, name)| name.clone());
            }
        }
        telemetry.mode = self.recorded_profile().map(|p| p.mode);
        telemetry.in_sync = self.in_sync(&telemetry.cpu);
        telemetry.fan = Some(lock(&self.fan_plan).1.clone());
        let fans = lock(&self.fans);
        for pwm in telemetry
            .fans
            .iter_mut()
            .filter_map(|header| header.pwm.as_mut())
        {
            let driven = fans.iter().any(|fan| {
                fan.channel == pwm.channel && matches!(fan.state, FanState::Active { .. })
            });
            if driven {
                pwm.control = PwmControl::Curve;
            }
        }
        telemetry
    }
}
