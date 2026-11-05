//! Fan-curve loop. Any error hands the channel back to firmware control, and an
//! independent over-temperature guard does the same whatever the curves say.
use crate::{daemon::Daemon, signals};
use asusio_core::{
    fan_curve::{
        CurveController, FanCurve, GuardAction, HwmonTemperature, Nct6799Pwm, PwmOutput,
        THERMAL_RELEASE_CELSIUS, ThermalGuard,
    },
    service::ipc::{FanState, FanStatus},
    telemetry,
};
use std::{io, sync::atomic::Ordering, time::Duration};

type Controller = CurveController<HwmonTemperature, Nct6799Pwm>;

fn controller(curve: &FanCurve) -> io::Result<Controller> {
    CurveController::new(
        curve.clone(),
        HwmonTemperature::new(curve.sensor.clone()),
        Nct6799Pwm::new(curve.channel)?,
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
        let (controller, state) = match controller(curve) {
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

    loop {
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

/// Release every channel any curve may have driven; used after the daemon exits for any
/// reason.
pub fn release_all(channels: &[u8]) -> io::Result<()> {
    let mut failures = Vec::new();
    for &channel in channels {
        match Nct6799Pwm::new(channel).and_then(|mut pwm| pwm.release()) {
            Ok(()) => eprintln!("fans: pwm{channel} returned to firmware control"),
            Err(error) => failures.push(format!("pwm{channel}: {error}")),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(failures.join("; ")))
    }
}
