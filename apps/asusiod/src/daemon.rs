//! Shared daemon state. Operations live in `lighting` and `profile`.
use asusio_core::{
    cpu_profile::FanPlan,
    lighting::{Availability, Config},
    service::{
        ServiceConfig, State,
        ipc::{FanStatus, RestoreEntry, Status},
    },
    telemetry::Sampler,
};
use asusio_policy::Policy;
use std::{
    collections::BTreeMap,
    fs, io,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    sync::{Mutex, MutexGuard},
    time::Instant,
};

pub struct Daemon {
    pub config: ServiceConfig,
    pub policy: Policy,
    pub client_gid: Option<u32>,
    /// Serializes all lighting hardware access.
    pub(crate) hardware: Mutex<()>,
    pub(crate) state: Mutex<State>,
    pub(crate) restore: Mutex<Vec<RestoreEntry>>,
    pub fans: Mutex<Vec<FanStatus>>,
    /// Error of the last write per lighting alias.
    pub(crate) lighting_errors: Mutex<BTreeMap<String, String>>,
    /// Cached readiness per lighting alias, so polling does not rescan devices.
    pub(crate) availability: Mutex<BTreeMap<String, (Instant, Availability)>>,
    /// Selected fan curves with a generation counter the fan loop watches.
    pub(crate) fan_plan: Mutex<(u64, FanPlan)>,
    /// Serializes CPU profile writes.
    pub(crate) cpu: Mutex<()>,
    pub(crate) profile_error: Mutex<Option<String>>,
    /// Telemetry counters, the NVML session and headers seen spinning.
    pub(crate) sampler: Mutex<Sampler>,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
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
pub(crate) fn read_config(path: &Path, owner: Option<u32>) -> io::Result<Config> {
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
        let sampler = Sampler::new();
        // The board profile names the pump headers; curves must never drive them.
        let pumps = sampler.profile().map(|p| p.pump_pwms()).unwrap_or_default();
        config.check_pumps(&pumps)?;
        let state = State::load(&config.state_file)?;
        let fan_plan = FanPlan::resolve(None, &config.fan_presets, &config.fan_curves);
        Ok(Self {
            fan_plan: Mutex::new((0, fan_plan)),
            cpu: Mutex::new(()),
            profile_error: Mutex::new(None),
            sampler: Mutex::new(sampler),
            policy: Policy::new(effective_uid(), client_gid, config.allow_active_sessions),
            client_gid,
            hardware: Mutex::new(()),
            state: Mutex::new(state),
            restore: Mutex::new(Vec::new()),
            fans: Mutex::new(Vec::new()),
            lighting_errors: Mutex::new(BTreeMap::new()),
            availability: Mutex::new(BTreeMap::new()),
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
}
