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

pub fn run(daemon: &Daemon) {
    let curves = &daemon.config.fan_curves;
    let mut controllers: Vec<Option<Controller>> = Vec::new();
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
    let interval = Duration::from_millis(daemon.config.fan_interval_ms);

    loop {
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
    for controller in controllers.iter_mut().flatten() {
        release(controller, "daemon stopping".to_owned());
    }
}

/// Release every configured channel; used after the daemon exits for any reason.
pub fn release_all(curves: &[FanCurve]) -> io::Result<()> {
    let mut failures = Vec::new();
    for curve in curves {
        match Nct6799Pwm::new(curve.channel).and_then(|mut pwm| pwm.release()) {
            Ok(()) => eprintln!("fans: pwm{} returned to firmware control", curve.channel),
            Err(error) => failures.push(format!("pwm{}: {error}", curve.channel)),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(failures.join("; ")))
    }
}
