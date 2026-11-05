//! Fan-curve loop. Any error hands the channel back to firmware control, and an
//! independent over-temperature guard does the same whatever the curves say.
use crate::{daemon::Daemon, signals};
use asusio_backends::fan_hwmon;
use asusio_core::{
    fan_curve::{
        CurveController, FanCurve, GuardAction, HwmonPwm, HwmonTemperature, PwmOutput,
        THERMAL_RELEASE_CELSIUS, ThermalGuard,
    },
    service::ipc::{FanState, FanStatus},
    telemetry,
};
use std::{io, sync::atomic::Ordering, time::Duration};

type Controller = CurveController<HwmonTemperature, HwmonPwm>;

/// Take over one output: remember the firmware's mode first, and refuse when it could not
/// be handed back (an unverified chip whose mode was not seen before).
fn controller(daemon: &Daemon, curve: &FanCurve) -> io::Result<Controller> {
    daemon.record_firmware_modes(&[curve.channel])?;
    let recorded = daemon.firmware_enable(curve.channel);
    if recorded.is_none() {
        let name =
            fan_hwmon::controller_name(std::path::Path::new(asusio_discovery::sysfs::HWMON))?;
        if fan_hwmon::default_firmware_enable(&name).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "pwm{} is under manual control and {name} has no known firmware mode to return to",
                    curve.channel
                ),
            ));
        }
    }
    CurveController::new(
        curve.clone(),
        HwmonTemperature::new(curve.sensor.clone()),
        HwmonPwm::with_firmware_enable(curve.channel, recorded)?,
    )
}

fn release(controller: &mut Controller, reason: String) -> FanState {
    let channel = controller.curve().channel;
    let reason = match controller.release() {
        Ok(()) => reason,
        Err(error) => format!("{reason}; firmware restore also failed: {error}"),
    };
    eprintln!("fans: pwm{channel} released to firmware: {reason}");
    FanState::Released { reason }
}

fn sensor_name(curve: &FanCurve) -> String {
    format!("{}/{}", curve.sensor.chip, curve.sensor.label)
}

fn publish(daemon: &Daemon, statuses: Vec<FanStatus>) {
    *daemon.fans.lock().unwrap_or_else(|p| p.into_inner()) = statuses;
}

/// Build controllers for one curve set and publish their initial status.
fn start(daemon: &Daemon, curves: &[FanCurve]) -> Vec<Option<Controller>> {
    let mut controllers = Vec::new();
    let mut statuses = Vec::new();
    for curve in curves {
        let (controller, state) = match controller(daemon, curve) {
            Ok(controller) => (Some(controller), FanState::Starting),
            Err(error) => (
                None,
                FanState::Released {
                    reason: error.to_string(),
                },
            ),
        };
        controllers.push(controller);
        statuses.push(FanStatus {
            channel: curve.channel,
            sensor: sensor_name(curve),
            state,
        });
    }
    publish(daemon, statuses);
    controllers
}

/// Publish every curve channel as handed back to firmware, with `reason`.
fn publish_released(daemon: &Daemon, curves: &[FanCurve], reason: &str) {
    publish(
        daemon,
        curves
            .iter()
            .map(|curve| FanStatus {
                channel: curve.channel,
                sensor: sensor_name(curve),
                state: FanState::Released {
                    reason: reason.to_owned(),
                },
            })
            .collect(),
    );
}

/// Hand every running controller back to firmware.
fn stop(controllers: &mut [Option<Controller>], reason: &str) {
    for controller in controllers.iter_mut().flatten() {
        release(controller, reason.to_owned());
    }
}

/// Drive the selected curve set. When a profile or a saved preset selects another set,
/// every running channel is first returned to firmware, then the new set starts. A
/// channel released after an error stays released until the selection changes or the
/// daemon restarts. The thermal guard is the exception: it resumes the curves itself once
/// the CPU has cooled down.
pub fn run(daemon: &Daemon) {
    let (mut generation, plan) = daemon.fan_plan();
    let mut curves = plan.curves;
    let mut guard = ThermalGuard::default();
    let mut controllers = start(daemon, &curves);
    let interval = Duration::from_millis(daemon.config.fan_interval_ms);
    let thermal_reason = format!("CPU above {THERMAL_RELEASE_CELSIUS} °C: firmware control");
    let mut paused = false;

    loop {
        // Fan detection owns the outputs while it runs: hand every curve back to firmware,
        // say so, and wait. Curves restart from scratch afterwards.
        if daemon.probing.load(Ordering::SeqCst) {
            if !paused {
                stop(&mut controllers, "fan detection");
                controllers = Vec::new();
                publish_released(daemon, &curves, "fan detection is running");
                paused = true;
            }
            daemon.loop_paused.store(true, Ordering::SeqCst);
            if !signals::sleep(interval) {
                break;
            }
            continue;
        }
        if paused {
            paused = false;
            daemon.loop_paused.store(false, Ordering::SeqCst);
            controllers = if guard.held() {
                Vec::new()
            } else {
                start(daemon, &curves)
            };
        }
        let (current, plan) = daemon.fan_plan();
        if current != generation {
            stop(&mut controllers, "fan preset changed");
            generation = current;
            curves = plan.curves;
            if guard.held() {
                controllers = Vec::new();
                publish_released(daemon, &curves, &thermal_reason);
            } else {
                controllers = start(daemon, &curves);
            }
        }
        if !curves.is_empty() {
            match guard.update(telemetry::tctl().ok()) {
                GuardAction::Release => {
                    eprintln!("fans: thermal protection, curves released to firmware");
                    stop(&mut controllers, "thermal protection");
                    controllers = Vec::new();
                    publish_released(daemon, &curves, &thermal_reason);
                }
                GuardAction::Resume => {
                    eprintln!("fans: CPU cooled down, curves resumed");
                    controllers = start(daemon, &curves);
                }
                GuardAction::None => {}
            }
        }
        daemon.thermal_hold.store(guard.held(), Ordering::Relaxed);
        for (index, slot) in controllers.iter_mut().enumerate() {
            let Some(controller) = slot else { continue };
            let state = match controller.tick() {
                Ok(tick) => FanState::Active {
                    celsius: tick.celsius,
                    percent: tick.percent,
                },
                Err(error) => {
                    let state = release(controller, error.to_string());
                    *slot = None;
                    state
                }
            };
            daemon.fans.lock().unwrap_or_else(|p| p.into_inner())[index].state = state;
        }
        if !signals::sleep(interval) {
            break;
        }
    }
    stop(&mut controllers, "daemon stopping");
}

/// Release every channel any curve may have driven, each to the firmware mode recorded
/// for it; used after the daemon exits for any reason. A channel the machine no longer has
/// (the state file moved to other hardware) is skipped, not an error.
pub fn release_all(
    channels: &[u8],
    recorded: &std::collections::BTreeMap<u8, u8>,
) -> io::Result<()> {
    let mut failures = Vec::new();
    for &channel in channels {
        let result = HwmonPwm::with_firmware_enable(channel, recorded.get(&channel).copied())
            .and_then(|mut pwm| pwm.release());
        match result {
            Ok(()) => eprintln!("fans: pwm{channel} returned to firmware control"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                eprintln!("fans: pwm{channel} is not present; skipped");
            }
            Err(error) => failures.push(format!("pwm{channel}: {error}")),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(failures.join("; ")))
    }
}
