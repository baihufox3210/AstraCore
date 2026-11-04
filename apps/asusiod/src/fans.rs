//! Fan-curve loop. Any error hands the channel back to firmware control.
use crate::{daemon::Daemon, signals};
use asusio_core::{
    fan_curve::{CurveController, FanCurve, HwmonTemperature, Nct6799Pwm, PwmOutput},
    service::ipc::{FanState, FanStatus},
};
use std::{io, time::Duration};

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

/// Build controllers for one curve set and publish their initial status.
fn start(daemon: &Daemon, curves: &[FanCurve]) -> Vec<Option<Controller>> {
    let mut controllers = Vec::new();
    let mut statuses = Vec::new();
    for curve in curves {
        let sensor = format!("{}/{}", curve.sensor.chip, curve.sensor.label);
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
            sensor,
            state,
        });
    }
    *daemon.fans.lock().unwrap_or_else(|p| p.into_inner()) = statuses;
    controllers
}

/// Hand every running controller back to firmware.
fn stop(controllers: &mut [Option<Controller>], reason: &str) {
    for controller in controllers.iter_mut().flatten() {
        release(controller, reason.to_owned());
    }
}

/// Drive the selected curve set. When a profile selects another set, every running
/// channel is first returned to firmware, then the new set starts. A channel released
/// after an error stays released until the selection changes or the daemon restarts.
pub fn run(daemon: &Daemon) {
    let (mut generation, plan) = daemon.fan_plan();
    let mut controllers = start(daemon, &plan.curves);
    let interval = Duration::from_millis(daemon.config.fan_interval_ms);

    loop {
        let (current, plan) = daemon.fan_plan();
        if current != generation {
            stop(&mut controllers, "fan preset changed");
            generation = current;
            controllers = start(daemon, &plan.curves);
        }
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

/// Release every channel any default or preset curve may drive; used after the daemon
/// exits for any reason.
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
