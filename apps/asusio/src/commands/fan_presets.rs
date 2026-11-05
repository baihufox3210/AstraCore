//! Fan presets through asusiod: list, save (applied at once when a profile uses them) and
//! delete. Curves are validated by the daemon; a rejected preset changes nothing.
use crate::{
    cli::PresetAction,
    output::{CliResult, print_json},
};
use asusio_core::{
    fan_curve::FanCurve,
    service::ipc::{self, FanPresetInfo, FanPresetResult, FanSetup, PresetSource, Request},
};
use std::path::Path;

fn describe_curve(curve: &FanCurve) -> String {
    let points: Vec<_> = curve
        .points
        .iter()
        .map(|point| format!("{:.0}:{}", point.celsius, point.percent))
        .collect();
    format!(
        "pwm{} [{}/{}] {}",
        curve.channel,
        curve.sensor.chip,
        curve.sensor.label,
        points.join(" ")
    )
}

fn describe_preset(preset: &FanPresetInfo, active: Option<&str>) {
    let source = match preset.source {
        PresetSource::Config => "config (read-only)",
        PresetSource::User if preset.overrides_config => "saved, replaces config",
        PresetSource::User => "saved",
    };
    let modes: Vec<_> = preset.used_by.iter().map(ToString::to_string).collect();
    let marker = if active == Some(preset.name.as_str()) {
        "*"
    } else {
        " "
    };
    println!(
        "{marker} {}  {source}  used by: {}",
        preset.name,
        if modes.is_empty() {
            "no profile".to_owned()
        } else {
            modes.join(", ")
        }
    );
    for curve in &preset.curves {
        println!("      {}", describe_curve(curve));
    }
}

pub fn list(socket: &Path, json: bool) -> CliResult {
    let setup: FanSetup = ipc::call(socket, &Request::GetFans)?;
    if json {
        return print_json(serde_json::to_value(&setup)?);
    }
    println!("Board: {}", setup.board.as_deref().unwrap_or("unknown"));
    match &setup.reason {
        Some(reason) => println!("Curves cannot be edited: {reason}"),
        None => println!("Curves can be edited"),
    }
    if setup.thermal_hold {
        println!(
            "Thermal protection is active: curves are handed back to firmware until the CPU cools down"
        );
    }
    for channel in &setup.channels {
        let state = match (channel.available, channel.connected) {
            (false, _) => "not exposed by the kernel",
            (true, true) => "connected",
            (true, false) => "nothing connected",
        };
        println!(
            "  pwm{}: {} ({state})",
            channel.pwm,
            channel.headers.join(" + ")
        );
    }
    if setup.presets.is_empty() {
        println!("No fan presets; fans stay under firmware control");
    }
    for preset in &setup.presets {
        describe_preset(preset, setup.active_preset.as_deref());
    }
    let limits = &setup.limits;
    println!(
        "Limits: duty {}-100%, at most {} points, 100% by {:.0} °C, hysteresis up to {:.0} °C, {} saved presets",
        limits.min_percent,
        limits.max_points,
        limits.max_full_speed_celsius,
        limits.max_hysteresis_celsius,
        limits.max_user_presets
    );
    for sensor in &limits.sensors {
        println!(
            "Sensor: {}/{} ({})",
            sensor.chip,
            sensor.label,
            sensor
                .celsius
                .map_or_else(|| "unreadable".to_owned(), |c| format!("{c:.1} °C"))
        );
    }
    println!(
        "Over-temperature guard: firmware control above {:.0} °C, curves resume at {:.0} °C",
        limits.thermal_release_celsius, limits.thermal_resume_celsius
    );
    Ok(())
}

fn print_result(result: &FanPresetResult, json: bool, saved: bool) -> CliResult {
    if json {
        return print_json(serde_json::to_value(result)?);
    }
    if let Some(preset) = &result.preset {
        describe_preset(preset, None);
    }
    if !result.executed {
        println!("Preview only (valid); add --execute to save and apply");
        return Ok(());
    }
    if let Some(error) = &result.persist_error {
        eprintln!("warning: applied, but not saved for the next boot: {error}");
    }
    let action = if saved { "Saved" } else { "Deleted" };
    if result.applied {
        println!("{action}; the running fan curves changed and take effect within a few seconds");
    } else {
        println!("{action}; no running profile uses it, so nothing changed yet");
    }
    Ok(())
}

pub fn run(socket: &Path, action: PresetAction) -> CliResult {
    match action {
        PresetAction::Set {
            name,
            curves,
            execute,
            json,
        } => {
            let result: FanPresetResult = ipc::call(
                socket,
                &Request::SetFanPreset {
                    name,
                    curves,
                    execute,
                },
            )?;
            print_result(&result, json, true)
        }
        PresetAction::Delete { name, execute } => {
            let result: FanPresetResult =
                ipc::call(socket, &Request::DeleteFanPreset { name, execute })?;
            print_result(&result, false, false)
        }
    }
}
