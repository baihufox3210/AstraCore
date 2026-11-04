//! Shared daemon state and lighting operations.
use crate::signals;
use asusio_core::{
    lighting::{self, Request, Store},
    service::{
        LightingState, ServiceConfig, State,
        ipc::{FanStatus, LightingResult, RestoreEntry, Status, TargetResult},
    },
};
use asusio_policy::Policy;
use std::{
    fs, io,
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
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Panics abort the process, so a poisoned lock cannot be observed in practice.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Resolve a group name from /etc/group.
fn group_id(name: &str) -> io::Result<u32> {
    fs::read_to_string("/etc/group")?
        .lines()
        .find_map(|line| {
            let mut fields = line.split(':');
            (fields.next() == Some(name))
                .then(|| fields.nth(1).and_then(|gid| gid.parse().ok()))
                .flatten()
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("group {name} not found; create it with: groupadd -r {name}"),
            )
        })
}

#[allow(unsafe_code)]
fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

impl Daemon {
    pub fn new(config: ServiceConfig) -> io::Result<Self> {
        let client_gid = config.client_group.as_deref().map(group_id).transpose()?;
        let state = State::load(&config.state_file)?;
        Ok(Self {
            policy: Policy::new(effective_uid(), client_gid),
            client_gid,
            hardware: Mutex::new(()),
            state: Mutex::new(state),
            restore: Mutex::new(Vec::new()),
            fans: Mutex::new(Vec::new()),
            config,
        })
    }

    fn store(&self) -> Store {
        Store::new(&self.config.lighting_config)
    }

    pub fn status(&self) -> Status {
        Status {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            lighting_config: self.config.lighting_config.clone(),
            state_file: self.config.state_file.clone(),
            restore: lock(&self.restore).clone(),
            fans: lock(&self.fans).clone(),
        }
    }

    /// Plan, and with `execute` apply and record, one lighting request.
    pub fn set_lighting(&self, request: &Request, execute: bool) -> io::Result<LightingResult> {
        let _hardware = lock(&self.hardware);
        let plan = self.store().plan(request)?;
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
        let desired = LightingState::from(request);
        for (target, outcome) in targets.iter_mut().zip(lighting::execute(plan)) {
            target.error = outcome.result.err().map(|error| error.to_string());
        }
        // Record the desired state even for failed targets so boot restore retries them.
        let persist_error = {
            let mut state = lock(&self.state);
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
        let desired = lock(&self.state).lighting.get(alias).copied();
        let Some(desired) = desired else {
            return Ok(());
        };
        let _hardware = lock(&self.hardware);
        let plan = self.store().plan(&desired.request(alias))?;
        for outcome in lighting::execute(plan) {
            outcome.result?;
        }
        Ok(())
    }

    /// Reapply every recorded alias once, now.
    pub fn restore_now(&self) -> Vec<TargetResult> {
        let aliases: Vec<_> = lock(&self.state).lighting.keys().cloned().collect();
        aliases
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
        let aliases: Vec<_> = lock(&self.state).lighting.keys().cloned().collect();
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
