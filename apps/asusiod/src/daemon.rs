//! Shared daemon state and lighting operations.
use crate::signals;
use asusio_core::{
    cpu_profile::{self, Custom, FanPlan, Mode, Profile, cpu_pstate},
    lighting::{self, Config, Request},
    service::{
        LightingState, ProfileState, ServiceConfig, State,
        ipc::{
            CpuSummary, FanState, FanStatus, LightingResult, ProfileReport, ProfileResult,
            PwmControl, RestoreEntry, Status, TargetResult, Telemetry,
        },
    },
    telemetry::{self, PowerSampler},
};
use asusio_discovery::sysfs;
use asusio_policy::Policy;
use std::{
    fs, io,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

/// How long boot restore keeps retrying targets that are not ready yet (e.g. the
/// screen, whose lianli-daemon socket only appears after login).
const RESTORE_WINDOW: Duration = Duration::from_secs(600);
const RESTORE_RETRY: Duration = Duration::from_secs(10);

pub struct Daemon {
    pub config: ServiceConfig,
    pub policy: Policy,
    pub client_gid: Option<u32>,
    /// Serializes all lighting hardware access.
    hardware: Mutex<()>,
    state: Mutex<State>,
    restore: Mutex<Vec<RestoreEntry>>,
    pub fans: Mutex<Vec<FanStatus>>,
    /// Selected fan curves with a generation counter the fan loop watches.
    fan_plan: Mutex<(u64, FanPlan)>,
    /// Serializes CPU profile writes.
    cpu: Mutex<()>,
    profile_error: Mutex<Option<String>>,
    power: Mutex<PowerSampler>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Panics abort the process, so a poisoned lock cannot be observed in practice.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Resolve a group name from /etc/group.
fn group_id(name: &str) -> Option<u32> {
    fs::read_to_string("/etc/group")
        .ok()?
        .lines()
        .find_map(|line| {
            let mut fields = line.split(':');
            (fields.next() == Some(name))
                .then(|| fields.nth(1).and_then(|gid| gid.parse().ok()))
                .flatten()
        })
}

#[allow(unsafe_code)]
fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Read a lighting configuration on behalf of `owner` (`None` = root, any owner).
/// The file must be a regular, non-symlink file owned by that user, so a client
/// cannot make the root daemon read files it does not own.
fn read_config(path: &Path, owner: Option<u32>) -> io::Result<Config> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "lighting configuration path must be absolute",
        ));
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || owner.is_some_and(|uid| metadata.uid() != uid) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "lighting configuration must be a regular file owned by the requesting user",
        ));
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024).read_to_end(&mut bytes)?;
    Config::from_json(&bytes)
}

impl Daemon {
    pub fn new(config: ServiceConfig) -> io::Result<Self> {
        let client_gid = config.client_group.as_deref().and_then(|name| {
            let gid = group_id(name);
            if gid.is_none() {
                eprintln!("group {name} not found; group-based access disabled");
            }
            gid
        });
        let state = State::load(&config.state_file)?;
        let fan_plan = FanPlan::resolve(None, &config.fan_presets, &config.fan_curves);
        Ok(Self {
            fan_plan: Mutex::new((0, fan_plan)),
            cpu: Mutex::new(()),
            profile_error: Mutex::new(None),
            power: Mutex::new(PowerSampler::new(Path::new(sysfs::POWERCAP))),
            policy: Policy::new(effective_uid(), client_gid, config.allow_active_sessions),
            client_gid,
            hardware: Mutex::new(()),
            state: Mutex::new(state),
            restore: Mutex::new(Vec::new()),
            fans: Mutex::new(Vec::new()),
            config,
        })
    }

    pub fn status(&self) -> Status {
        // Guards in a struct literal live until the end of the statement, so the state
        // is read once here instead of locking it twice inside the literal.
        let (lighting_config, profile) = {
            let state = lock(&self.state);
            (
                state.lighting_config.clone(),
                state.profile.as_ref().map(|p| p.mode),
            )
        };
        Status {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            lighting_config,
            state_file: self.config.state_file.clone(),
            restore: lock(&self.restore).clone(),
            fans: lock(&self.fans).clone(),
            profile,
            fan: Some(self.fan_plan().1),
            profile_error: lock(&self.profile_error).clone(),
        }
    }

    /// The fan loop polls this; a new generation means the selection changed.
    pub fn fan_plan(&self) -> (u64, FanPlan) {
        lock(&self.fan_plan).clone()
    }

    fn resolve(&self, mode: Mode, custom: Option<&Custom>) -> io::Result<Profile> {
        cpu_profile::resolve(
            mode,
            custom,
            &self.config.fan_presets,
            &self.config.fan_curves,
        )
    }

    fn recorded_profile(&self) -> Option<ProfileState> {
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
            fan_presets: self.config.fan_presets.keys().cloned().collect(),
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
        let mut telemetry = telemetry::read(Some(&mut lock(&self.power)));
        telemetry.mode = self.recorded_profile().map(|p| p.mode);
        telemetry.in_sync = self.in_sync(&telemetry.cpu);
        telemetry.fan = Some(lock(&self.fan_plan).1.clone());
        let fans = lock(&self.fans);
        for pwm in &mut telemetry.pwm {
            let driven = fans.iter().any(|fan| {
                fan.channel == pwm.channel && matches!(fan.state, FanState::Active { .. })
            });
            if driven {
                pwm.control = PwmControl::Curve;
            }
        }
        telemetry
    }

    /// Plan, and with `execute` apply and record, one lighting request from `caller`.
    pub fn set_lighting(
        &self,
        lighting_config: PathBuf,
        caller: u32,
        request: &Request,
        execute: bool,
    ) -> io::Result<LightingResult> {
        let owner = (caller != 0).then_some(caller);
        let config = read_config(&lighting_config, owner)?;
        let _hardware = lock(&self.hardware);
        let plan = lighting::plan(&config, request)?;
        let mut targets: Vec<_> = plan
            .iter()
            .map(|target| TargetResult {
                alias: target.alias().to_owned(),
                output: target.output(),
                error: None,
            })
            .collect();
        if !execute {
            return Ok(LightingResult {
                executed: false,
                targets,
                persist_error: None,
            });
        }
        for (target, outcome) in targets.iter_mut().zip(lighting::execute(plan)) {
            target.error = outcome.result.err().map(|error| error.to_string());
        }
        // Record the desired state even for failed targets so boot restore retries them.
        let persist_error = {
            let mut state = lock(&self.state);
            if state.lighting_config.as_ref() != Some(&lighting_config) {
                state.lighting.clear();
            }
            state.lighting_config = Some(lighting_config);
            state.config_owner = owner;
            let desired = LightingState::from(request);
            for target in &targets {
                state.lighting.insert(target.alias.clone(), desired);
            }
            state.save(&self.config.state_file).err()
        }
        .map(|error| error.to_string());
        Ok(LightingResult {
            executed: true,
            targets,
            persist_error,
        })
    }

    /// Reapply one alias from the latest recorded state.
    fn restore_one(&self, alias: &str) -> io::Result<()> {
        let (path, owner, desired) = {
            let state = lock(&self.state);
            (
                state.lighting_config.clone(),
                state.config_owner,
                state.lighting.get(alias).copied(),
            )
        };
        let (Some(path), Some(desired)) = (path, desired) else {
            return Ok(());
        };
        let config = read_config(&path, owner)?;
        let _hardware = lock(&self.hardware);
        let plan = lighting::plan(&config, &desired.request(alias))?;
        for outcome in lighting::execute(plan) {
            outcome.result?;
        }
        Ok(())
    }

    fn recorded_aliases(&self) -> Vec<String> {
        lock(&self.state).lighting.keys().cloned().collect()
    }

    /// Reapply every recorded alias once, now.
    pub fn restore_now(&self) -> Vec<TargetResult> {
        self.recorded_aliases()
            .into_iter()
            .map(|alias| {
                let error = self
                    .restore_one(&alias)
                    .err()
                    .map(|error| error.to_string());
                let output = lock(&self.state).lighting[&alias].rgb;
                TargetResult {
                    alias,
                    output,
                    error,
                }
            })
            .collect()
    }

    /// Boot restore: retry targets that are not ready yet, within a bounded window.
    pub fn restore_at_startup(&self) {
        let aliases = self.recorded_aliases();
        if aliases.is_empty() {
            eprintln!("restore: no recorded lighting state");
            return;
        }
        *lock(&self.restore) = aliases
            .iter()
            .map(|alias| RestoreEntry {
                alias: alias.clone(),
                applied: false,
                attempts: 0,
                last_error: None,
            })
            .collect();
        let deadline = Instant::now() + RESTORE_WINDOW;
        loop {
            let pending: Vec<_> = lock(&self.restore)
                .iter()
                .filter(|entry| !entry.applied)
                .map(|entry| entry.alias.clone())
                .collect();
            if pending.is_empty() {
                eprintln!("restore: all recorded lighting applied");
                return;
            }
            for alias in pending {
                let result = self.restore_one(&alias);
                let mut entries = lock(&self.restore);
                if let Some(entry) = entries.iter_mut().find(|entry| entry.alias == alias) {
                    entry.attempts += 1;
                    match result {
                        Ok(()) => {
                            entry.applied = true;
                            entry.last_error = None;
                            eprintln!("restore: {alias} applied");
                        }
                        Err(error) => entry.last_error = Some(error.to_string()),
                    }
                }
            }
            if Instant::now() >= deadline {
                for entry in lock(&self.restore).iter().filter(|entry| !entry.applied) {
                    eprintln!(
                        "restore: giving up on {}: {}",
                        entry.alias,
                        entry.last_error.as_deref().unwrap_or("unknown error")
                    );
                }
                return;
            }
            if !signals::sleep(RESTORE_RETRY) {
                return;
            }
        }
    }
}
