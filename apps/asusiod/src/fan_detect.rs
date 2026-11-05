//! Learning the fans of the machine asusiod runs on: fingerprinting the hardware, recording
//! the firmware's own fan modes, detecting which fan inputs follow which output, confirming
//! roles, and resetting to the system defaults.
use crate::{
    daemon::{Daemon, lock},
    fan_presets::fan_hardware,
    signals,
};
use asusio_backends::fan_hwmon;
use asusio_core::{
    fan_curve::{HwmonPwm, PwmOutput},
    fan_map::{self, FULL_SPEED_PERCENT, FanMap, FanRole},
    fan_probe::{self, ProbeConfig, ProbeIo},
    service::ipc::{
        DetectPlanItem, DetectState, FanDetectResult, FanDetection, FanResetResult, FanSetup,
    },
    telemetry,
};
use asusio_discovery::sysfs;
use std::{
    collections::BTreeMap,
    io,
    path::Path,
    sync::{Arc, atomic::Ordering},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// How long detection waits for the fan loop to hand its curves back before it starts.
const PAUSE_WAIT: Duration = Duration::from_secs(6);

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn hwmon_root() -> &'static Path {
    Path::new(sysfs::HWMON)
}

fn controller() -> io::Result<fan_hwmon::FanController> {
    fan_hwmon::discover_fan_controllers(hwmon_root())?
        .devices
        .into_iter()
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no fan controller"))
}

fn percent(raw: u8) -> u8 {
    ((u16::from(raw) * 100 + 127) / 255) as u8
}

/// The probe's view of the chip: sysfs, with the daemon recording firmware modes.
struct SysfsIo<'a> {
    daemon: &'a Daemon,
}

impl ProbeIo for SysfsIo<'_> {
    fn duty_percent(&mut self, pwm: u8) -> io::Result<u8> {
        controller()?
            .pwm_channels
            .iter()
            .find(|channel| channel.channel == pwm)
            .map(|channel| percent(channel.duty))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "PWM output not available"))
    }

    fn enable(&mut self, pwm: u8) -> io::Result<u8> {
        fan_hwmon::current_enable(hwmon_root(), pwm)
    }

    fn rpms(&mut self) -> io::Result<BTreeMap<u8, u64>> {
        Ok(controller()?
            .fan_speeds
            .iter()
            .map(|fan| (fan.channel, fan.rpm))
            .collect())
    }

    fn before_takeover(&mut self, pwm: u8) -> io::Result<()> {
        self.daemon.record_firmware_modes(&[pwm])
    }

    fn set_manual(&mut self, pwm: u8, percent: u8) -> io::Result<()> {
        fan_hwmon::set_manual(hwmon_root(), pwm, percent)
    }

    fn restore(&mut self, pwm: u8) -> io::Result<()> {
        self.daemon.release_channel(pwm)
    }

    fn cpu_celsius(&mut self) -> Option<f64> {
        telemetry::tctl().ok()
    }

    fn sleep(&mut self, duration: Duration) -> bool {
        signals::sleep(duration)
    }
}

impl Daemon {
    /// Tie the fan knowledge in the state file to this machine. When the file has moved to
    /// other hardware, roles, detection results and recorded firmware modes are dropped
    /// (they could be wrong here); saved presets are kept.
    pub(crate) fn sync_fan_hardware(&self) {
        let Ok(hardware) = fan_hardware() else {
            return;
        };
        let board = asusio_discovery::discover_board().ok();
        let fingerprint = fan_map::fingerprint(board.as_ref(), &hardware.controller);
        let mut state = lock(&self.state);
        let before = state.fan_fingerprint.clone();
        if state.bind_fan_hardware(&fingerprint) {
            eprintln!(
                "fans: this state file belongs to other hardware; roles and detection results were discarded, saved presets kept"
            );
        }
        if state.fan_fingerprint != before
            && let Some(error) = self.save(&state)
        {
            eprintln!("fans: cannot save the hardware fingerprint: {error}");
        }
    }

    /// Refuse to start when the configuration's own curves would drive a pump.
    pub(crate) fn check_config_pumps(&self) -> io::Result<()> {
        self.config.check_pumps(&self.pump_pwms())
    }

    /// Remember the firmware's mode of each output before it is first taken over, so it can
    /// be restored exactly. An output that is already under manual control has no mode to
    /// remember and is left unrecorded.
    pub(crate) fn record_firmware_modes(&self, pwms: &[u8]) -> io::Result<()> {
        let mut state = lock(&self.state);
        let mut changed = false;
        for &pwm in pwms {
            if state.firmware_enable.contains_key(&pwm) {
                continue;
            }
            let mode = fan_hwmon::current_enable(hwmon_root(), pwm)?;
            if mode != 1 {
                state.firmware_enable.insert(pwm, mode);
                changed = true;
            }
        }
        if changed && let Some(error) = self.save(&state) {
            return Err(io::Error::other(format!(
                "cannot save the firmware fan modes: {error}"
            )));
        }
        Ok(())
    }

    pub(crate) fn firmware_enable(&self, pwm: u8) -> Option<u8> {
        lock(&self.state).firmware_enable.get(&pwm).copied()
    }

    /// Hand one output back to the firmware mode that was recorded for it.
    pub(crate) fn release_channel(&self, pwm: u8) -> io::Result<()> {
        HwmonPwm::with_firmware_enable(pwm, self.firmware_enable(pwm))?.release()
    }

    /// Confirm (or, with `None`, unconfirm) what is connected to an output. The board
    /// profile's own answer cannot be overridden.
    pub fn set_fan_role(&self, pwm: u8, role: Option<FanRole>) -> io::Result<FanSetup> {
        self.sync_fan_hardware();
        let hardware = fan_hardware()?;
        if !hardware.pwms.contains(&pwm) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("pwm{pwm} is not exposed by the kernel"),
            ));
        }
        if self
            .roles()
            .get(&pwm)
            .is_some_and(|(_, source)| *source == fan_map::RoleSource::Board)
        {
            return Err(invalid(format!(
                "pwm{pwm} is described by the board profile and cannot be changed"
            )));
        }
        {
            let _serialize = lock(&self.cpu);
            let mut state = lock(&self.state);
            match role {
                Some(role) => state.fan_roles.insert(pwm, role),
                None => state.fan_roles.remove(&pwm),
            };
            if let Some(error) = self.save(&state) {
                return Err(io::Error::other(format!("cannot save the role: {error}")));
            }
            drop(state);
            // A curve on an output that is no longer a fan stops within one interval.
            self.refresh_fan_plan();
        }
        Ok(self.fan_setup())
    }

    /// Which outputs detection would raise, and why the others are skipped. With `execute`
    /// it starts the detection in the background and returns at once.
    pub fn detect_fans(self: &Arc<Self>, execute: bool) -> io::Result<FanDetectResult> {
        self.sync_fan_hardware();
        let hardware = fan_hardware()?;
        let roles = self.roles();
        let cfg = ProbeConfig::default();
        let mut io = SysfsIo { daemon: self };
        let (mut plan, mut run) = (Vec::new(), Vec::new());
        for pwm in hardware.pwms {
            let baseline = io.duty_percent(pwm).unwrap_or(0);
            if roles
                .get(&pwm)
                .is_some_and(|(role, _)| *role == FanRole::Pump)
            {
                plan.push(DetectPlanItem {
                    pwm,
                    baseline_percent: baseline,
                    will_test: false,
                    reason: Some("a pump; it is never raised".to_owned()),
                });
                continue;
            }
            run.push(pwm);
            let reason = if io.enable(pwm).is_ok_and(|mode| mode == 1) {
                Some("another program controls it manually".to_owned())
            } else if baseline >= FULL_SPEED_PERCENT {
                Some(format!(
                    "the firmware already runs it at {baseline}%; it cannot be tested by raising it"
                ))
            } else {
                None
            };
            plan.push(DetectPlanItem {
                pwm,
                baseline_percent: baseline,
                will_test: reason.is_none(),
                reason,
            });
        }
        let tested = plan.iter().filter(|item| item.will_test).count() as u64;
        let per_output = cfg.samples.last().copied().unwrap_or(0) + cfg.recover.as_secs();
        let mut result = FanDetectResult {
            executed: false,
            plan,
            estimated_seconds: tested * per_output,
            detection: lock(&self.detect).clone(),
        };
        if !execute {
            return Ok(result);
        }
        {
            let mut detection = lock(&self.detect);
            if detection.state == DetectState::Running {
                return Err(invalid("fan detection is already running"));
            }
            *detection = FanDetection {
                state: DetectState::Running,
                pwm: None,
                done: 0,
                total: run.len(),
                message: None,
            };
            result.detection = detection.clone();
        }
        let daemon = self.clone();
        thread::spawn(move || run_detection(&daemon, &run));
        result.executed = true;
        Ok(result)
    }

    /// Reset the fans to the system defaults: every saved preset is deleted and the fans go
    /// back to firmware control. With `everything`, the detection result and the confirmed
    /// roles are forgotten as well. The board profile and the configuration file stay.
    pub fn reset_fans(&self, execute: bool, everything: bool) -> io::Result<FanResetResult> {
        let _serialize = lock(&self.cpu);
        if self.probing.load(Ordering::SeqCst) {
            return Err(invalid("fan detection is running; wait for it to finish"));
        }
        let removed: Vec<String> = lock(&self.state).user_fan_presets.keys().cloned().collect();
        let mut result = FanResetResult {
            executed: execute,
            removed_presets: removed,
            cleared_knowledge: everything,
            released: Vec::new(),
            release_errors: Vec::new(),
        };
        if !execute {
            return Ok(result);
        }
        {
            let mut state = lock(&self.state);
            state.user_fan_presets.clear();
            if everything {
                state.fan_map = None;
                state.fan_roles.clear();
            }
            // A custom profile must not point at a preset that no longer exists.
            if let Some(custom) = state
                .profile
                .as_mut()
                .and_then(|profile| profile.custom.as_mut())
                && custom
                    .fan_preset
                    .as_ref()
                    .is_some_and(|name| !self.config.fan_presets.contains_key(name))
            {
                custom.fan_preset = None;
            }
            if let Some(error) = self.save(&state) {
                return Err(io::Error::other(format!("cannot save the reset: {error}")));
            }
        }
        self.refresh_fan_plan();
        // Outputs the daemon has taken over and no curve drives any more go back to firmware
        // at once; the fan loop releases the rest within one interval.
        let driven: Vec<u8> = self
            .fan_plan()
            .1
            .curves
            .iter()
            .map(|curve| curve.channel)
            .collect();
        let recorded: Vec<u8> = lock(&self.state).firmware_enable.keys().copied().collect();
        for pwm in recorded.into_iter().filter(|pwm| !driven.contains(pwm)) {
            match self.release_channel(pwm) {
                Ok(()) => result.released.push(pwm),
                Err(error) => {
                    eprintln!("fans: pwm{pwm} could not be released: {error}");
                    result.release_errors.push(format!("pwm{pwm}: {error}"));
                }
            }
        }
        Ok(result)
    }
}

/// Wait for the fan loop to hand its curves back; false if it did not.
fn wait_for_pause(daemon: &Daemon) -> bool {
    let steps = (PAUSE_WAIT.as_millis() / 100) as usize;
    for _ in 0..steps {
        if daemon.loop_paused.load(Ordering::SeqCst) {
            return true;
        }
        if !signals::sleep(Duration::from_millis(100)) {
            return false;
        }
    }
    daemon.loop_paused.load(Ordering::SeqCst)
}

fn run_detection(daemon: &Arc<Daemon>, pwms: &[u8]) {
    // The fan loop releases its curves while this flag is set and resumes when it clears.
    daemon.probing.store(true, Ordering::SeqCst);
    let detection = if wait_for_pause(daemon) {
        let mut io = SysfsIo { daemon };
        let channels = fan_probe::probe_all(
            &mut io,
            &ProbeConfig::default(),
            pwms,
            &mut |done, total, pwm| {
                *lock(&daemon.detect) = FanDetection {
                    state: DetectState::Running,
                    pwm: Some(pwm),
                    done,
                    total,
                    message: None,
                };
            },
        );
        let followed = channels.iter().filter(|c| !c.tachs.is_empty()).count();
        let warnings: Vec<String> = channels
            .iter()
            .filter_map(|c| c.note.as_ref().filter(|n| n.starts_with("WARNING")))
            .cloned()
            .collect();
        let tested = channels.iter().filter(|c| c.tested).count();
        let taken_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let saved = {
            let mut state = lock(&daemon.state);
            state.fan_map = Some(FanMap {
                taken_unix,
                channels,
            });
            daemon.save(&state)
        };
        let mut message = format!(
            "{tested} of {} outputs tested, {followed} drive a fan input",
            pwms.len()
        );
        if let Some(error) = saved {
            message.push_str(&format!("; the result could not be saved: {error}"));
        }
        for warning in warnings {
            message.push_str(&format!("; {warning}"));
        }
        FanDetection {
            state: DetectState::Finished,
            pwm: None,
            done: pwms.len(),
            total: pwms.len(),
            message: Some(message),
        }
    } else {
        FanDetection {
            state: DetectState::Failed,
            pwm: None,
            done: 0,
            total: pwms.len(),
            message: Some("the fan loop did not pause; nothing was changed".to_owned()),
        }
    };
    eprintln!("fans: detection finished: {:?}", detection.message);
    *lock(&daemon.detect) = detection;
    daemon.probing.store(false, Ordering::SeqCst);
}
