//! Lighting requests: individual and Sync modes, per-target status and boot restore.
use crate::{
    daemon::{Daemon, lock, read_config},
    signals,
};
use asusio_core::{
    lighting::{self, ALL, Availability, Config, Request, TargetKind, info},
    service::{
        LightingMode, LightingSource, LightingState, State,
        ipc::{LightingReport, LightingResult, RestoreEntry, TargetResult, TargetStatus},
    },
};
use std::{
    io,
    path::PathBuf,
    time::{Duration, Instant},
};

/// How long boot restore keeps retrying targets that are not ready yet (e.g. the
/// screen, whose lianli-daemon socket only appears after login).
const RESTORE_WINDOW: Duration = Duration::from_secs(600);
const RESTORE_RETRY: Duration = Duration::from_secs(10);
/// Readiness is re-checked at most this often while a front end polls.
const AVAILABILITY_TTL: Duration = Duration::from_secs(10);

/// One alias and the settings it should show.
type Job = (String, LightingState);

/// The configuration owner the daemon must check (`None` for root).
fn owner(caller: u32) -> Option<u32> {
    (caller != 0).then_some(caller)
}

fn check_brightness(desired: &LightingState) -> io::Result<()> {
    if desired.brightness > 100 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "brightness must be 0..100",
        ));
    }
    Ok(())
}

fn aliases(config: &Config) -> Vec<&str> {
    config.targets.keys().map(String::as_str).collect()
}

impl Daemon {
    /// Plan every job separately, so one unresolvable target does not block the others,
    /// then (with `execute`) write the planned ones. All planning happens before any write.
    fn run_jobs(&self, config: &Config, jobs: &[Job], execute: bool) -> Vec<TargetResult> {
        let _hardware = lock(&self.hardware);
        let plans: Vec<_> = jobs
            .iter()
            .map(|(alias, desired)| lighting::plan(config, &desired.request(alias)))
            .collect();
        let results: Vec<_> = jobs
            .iter()
            .zip(plans)
            .map(|((alias, desired), plan)| {
                let (output, error) = match plan {
                    Err(error) => (desired.rgb, Some(error.to_string())),
                    Ok(plan) => {
                        let output = plan.first().map_or(desired.rgb, |target| target.output());
                        let error = execute
                            .then(|| {
                                lighting::execute(plan)
                                    .into_iter()
                                    .find_map(|outcome| outcome.result.err())
                            })
                            .flatten()
                            .map(|error| error.to_string());
                        (output, error)
                    }
                };
                TargetResult {
                    alias: alias.clone(),
                    output,
                    error,
                }
            })
            .collect();
        if execute {
            self.note_results(&results);
        }
        results
    }

    /// Remember per-target write errors, and refresh readiness after a write.
    fn note_results(&self, results: &[TargetResult]) {
        let mut errors = lock(&self.lighting_errors);
        let mut availability = lock(&self.availability);
        for result in results {
            match &result.error {
                Some(error) => {
                    errors.insert(result.alias.clone(), error.clone());
                    availability.remove(&result.alias);
                }
                None => {
                    errors.remove(&result.alias);
                    availability.insert(
                        result.alias.clone(),
                        (Instant::now(), Availability::Available),
                    );
                }
            }
        }
    }

    fn availability(&self, alias: &str, device: &lighting::Device) -> Availability {
        if let Some((at, cached)) = lock(&self.availability).get(alias)
            && at.elapsed() < AVAILABILITY_TTL
        {
            return cached.clone();
        }
        let fresh = info::availability(device);
        lock(&self.availability).insert(alias.to_owned(), (Instant::now(), fresh.clone()));
        fresh
    }

    pub(crate) fn save(&self, state: &State) -> Option<String> {
        state
            .save(&self.config.state_file)
            .err()
            .map(|error| error.to_string())
    }

    /// Individual request for one alias or `all`; switches to individual mode when executed.
    pub fn set_lighting(
        &self,
        lighting_config: PathBuf,
        caller: u32,
        request: &Request,
        execute: bool,
    ) -> io::Result<LightingResult> {
        let owner = owner(caller);
        let config = read_config(&lighting_config, owner)?;
        let desired = LightingState::from(request);
        check_brightness(&desired)?;
        let selected: Vec<&str> = match request.target() {
            ALL => aliases(&config),
            alias if config.targets.contains_key(alias) => vec![alias],
            alias => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown target: {alias}"),
                ));
            }
        };
        let jobs: Vec<Job> = selected
            .iter()
            .map(|alias| ((*alias).to_owned(), desired))
            .collect();
        let targets = self.run_jobs(&config, &jobs, execute);
        let mut state = lock(&self.state);
        if !execute {
            return Ok(LightingResult {
                executed: false,
                targets,
                persist_error: None,
                mode: state.lighting_mode,
            });
        }
        // Record the desired state even for failed targets so boot restore retries them.
        state.bind_lighting_config(lighting_config, owner);
        // In Sync mode the target keeps its own settings from now on (it turns custom);
        // the others go on following Sync. Otherwise this is individual mode as before.
        let stay_in_sync = state.lighting_mode == LightingMode::Sync && state.sync.is_some();
        if !stay_in_sync {
            state.switch_lighting_mode(LightingMode::Individual, aliases(&config))?;
        }
        for target in &targets {
            state.lighting.insert(target.alias.clone(), desired);
            if stay_in_sync {
                state.lighting_custom.insert(target.alias.clone());
            }
        }
        Ok(LightingResult {
            executed: true,
            targets,
            persist_error: self.save(&state),
            mode: state.lighting_mode,
        })
    }

    /// One setting for every target; switches to Sync mode when executed.
    pub fn set_sync(
        &self,
        lighting_config: PathBuf,
        caller: u32,
        desired: LightingState,
        execute: bool,
    ) -> io::Result<LightingResult> {
        let owner = owner(caller);
        let config = read_config(&lighting_config, owner)?;
        check_brightness(&desired)?;
        // Targets with settings of their own (custom) keep them; see `State::record_sync`.
        let followers = lock(&self.state).sync_followers(aliases(&config));
        let jobs: Vec<Job> = followers
            .into_iter()
            .map(|alias| (alias, desired))
            .collect();
        let targets = self.run_jobs(&config, &jobs, execute);
        let mut state = lock(&self.state);
        if !execute {
            return Ok(LightingResult {
                executed: false,
                targets,
                persist_error: None,
                mode: state.lighting_mode,
            });
        }
        state.bind_lighting_config(lighting_config, owner);
        state.record_sync(desired, aliases(&config));
        Ok(LightingResult {
            executed: true,
            targets,
            persist_error: self.save(&state),
            mode: state.lighting_mode,
        })
    }

    /// Switch between Sync and individual mode, showing the settings recorded for it.
    pub fn set_lighting_mode(
        &self,
        lighting_config: PathBuf,
        caller: u32,
        mode: LightingMode,
        execute: bool,
    ) -> io::Result<LightingResult> {
        let owner = owner(caller);
        let config = read_config(&lighting_config, owner)?;
        let transition = |state: &mut State| {
            state.bind_lighting_config(lighting_config.clone(), owner);
            state.switch_lighting_mode(mode, aliases(&config))
        };
        let mut next = lock(&self.state).clone();
        transition(&mut next)?;
        let jobs: Vec<Job> = aliases(&config)
            .into_iter()
            .filter_map(|alias| {
                next.desired(alias)
                    .map(|desired| (alias.to_owned(), desired))
            })
            .collect();
        let targets = self.run_jobs(&config, &jobs, execute);
        if !execute {
            return Ok(LightingResult {
                executed: false,
                targets,
                persist_error: None,
                mode,
            });
        }
        // Apply the same transition to the current state rather than overwriting it with
        // the earlier snapshot, so concurrent changes are kept.
        let mut state = lock(&self.state);
        transition(&mut state)?;
        Ok(LightingResult {
            executed: true,
            targets,
            persist_error: self.save(&state),
            mode: state.lighting_mode,
        })
    }

    /// Make one target follow Sync or keep its own settings. Every target whose shown
    /// setting changes is written (switching from individual mode can move several).
    pub fn set_lighting_source(
        &self,
        lighting_config: PathBuf,
        caller: u32,
        target: &str,
        source: LightingSource,
        execute: bool,
    ) -> io::Result<LightingResult> {
        let owner = owner(caller);
        let config = read_config(&lighting_config, owner)?;
        if !config.targets.contains_key(target) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown target: {target}"),
            ));
        }
        let transition = |state: &mut State| {
            state.bind_lighting_config(lighting_config.clone(), owner);
            state.set_lighting_source(target, source, aliases(&config))
        };
        let before = lock(&self.state).clone();
        let mut next = before.clone();
        transition(&mut next)?;
        let jobs: Vec<Job> = aliases(&config)
            .into_iter()
            .filter_map(|alias| {
                let desired = next.desired(alias)?;
                (alias == target || before.desired(alias) != Some(desired))
                    .then(|| (alias.to_owned(), desired))
            })
            .collect();
        let targets = self.run_jobs(&config, &jobs, execute);
        if !execute {
            return Ok(LightingResult {
                executed: false,
                targets,
                persist_error: None,
                mode: next.lighting_mode,
            });
        }
        let mut state = lock(&self.state);
        transition(&mut state)?;
        Ok(LightingResult {
            executed: true,
            targets,
            persist_error: self.save(&state),
            mode: state.lighting_mode,
        })
    }

    /// Every configured target with its recorded settings, effects and readiness.
    pub fn lighting_report(
        &self,
        lighting_config: PathBuf,
        caller: u32,
    ) -> io::Result<LightingReport> {
        let config = read_config(&lighting_config, owner(caller))?;
        let state = lock(&self.state).clone();
        let recorded = state.lighting_config.as_ref() == Some(&lighting_config);
        let errors = lock(&self.lighting_errors).clone();
        let targets = config
            .targets
            .iter()
            .map(|(alias, target)| {
                let individual = recorded
                    .then(|| state.lighting.get(alias).copied())
                    .flatten();
                let source = state.source(alias);
                let applied = match source {
                    LightingSource::Custom => individual,
                    LightingSource::Sync => state.sync,
                };
                TargetStatus {
                    alias: alias.clone(),
                    kind: TargetKind::of(&target.device),
                    backend: info::backend(&target.device).to_owned(),
                    white_balance: target.white_balance,
                    effects: info::effects(&target.device),
                    source,
                    individual,
                    applied,
                    availability: self.availability(alias, &target.device),
                    last_error: errors.get(alias).cloned(),
                }
            })
            .collect();
        Ok(LightingReport {
            mode: state.lighting_mode,
            sync: state.sync,
            lighting_config,
            recorded,
            targets,
        })
    }

    /// Aliases the current mode restores; Sync mode needs the recorded configuration.
    fn restore_aliases(&self) -> Vec<String> {
        let state = lock(&self.state).clone();
        let configured = match (&state.lighting_config, state.lighting_mode) {
            (Some(path), LightingMode::Sync) => match read_config(path, state.config_owner) {
                Ok(config) => config.targets.into_keys().collect(),
                Err(error) => {
                    eprintln!("restore: cannot read {}: {error}", path.display());
                    Vec::new()
                }
            },
            _ => Vec::new(),
        };
        state.restore_aliases(configured.iter().map(String::as_str))
    }

    /// Reapply one alias from the latest recorded state of the current mode.
    fn restore_one(&self, alias: &str) -> io::Result<()> {
        let (path, owner, desired) = {
            let state = lock(&self.state);
            (
                state.lighting_config.clone(),
                state.config_owner,
                state.desired(alias),
            )
        };
        let (Some(path), Some(desired)) = (path, desired) else {
            return Ok(());
        };
        let config = read_config(&path, owner)?;
        let result = self.run_jobs(&config, &[(alias.to_owned(), desired)], true);
        match result.into_iter().next().and_then(|target| target.error) {
            Some(error) => Err(io::Error::other(error)),
            None => Ok(()),
        }
    }

    /// Reapply every alias of the current mode once, now.
    pub fn restore_now(&self) -> Vec<TargetResult> {
        self.restore_aliases()
            .into_iter()
            .map(|alias| {
                let error = self
                    .restore_one(&alias)
                    .err()
                    .map(|error| error.to_string());
                let output = lock(&self.state)
                    .desired(&alias)
                    .map_or([0; 3], |desired| desired.rgb);
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
        let aliases = self.restore_aliases();
        if aliases.is_empty() {
            eprintln!("restore: no recorded lighting state");
            return;
        }
        eprintln!("restore: {:?} mode", lock(&self.state).lighting_mode);
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
