//! Fan presets, detection, roles and reset through astrad. Curves are validated by the
//! daemon; a rejected request changes nothing.
use crate::{
    cli::{PresetAction, RoleArg},
    output::{CliResult, print_json},
};
use astracore_core::{
    fan_curve::FanCurve,
    service::ipc::{
        self, DetectState, FanChannelInfo, FanDetectResult, FanPresetInfo, FanPresetResult,
        FanResetResult, FanRole, FanSetup, PresetSource, Request, RoleSource,
    },
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
    if let Some(problem) = &preset.problem {
        println!("      not applied on this machine: {problem}");
    }
    for curve in &preset.curves {
        println!("      {}", describe_curve(curve));
    }
}

fn role_name(role: FanRole) -> &'static str {
    match role {
        FanRole::Fan => "fan",
        FanRole::Pump => "pump",
    }
}

fn describe_channel(channel: &FanChannelInfo) {
    let state = if channel.connected {
        "connected"
    } else {
        "nothing connected"
    };
    let role = match (channel.role, channel.role_source) {
        (Some(role), Some(RoleSource::Board)) => format!("{} (board profile)", role_name(role)),
        (Some(role), _) => format!("{} (confirmed)", role_name(role)),
        (None, _) => match channel.suggested_role {
            Some(suggested) => format!("unconfirmed, detection suggests {}", role_name(suggested)),
            None => "unconfirmed".to_owned(),
        },
    };
    println!(
        "  pwm{}: {} ({state}) - {role}",
        channel.pwm,
        channel.headers.join(" + ")
    );
    if let Some(note) = channel.probe.as_ref().and_then(|probe| probe.note.as_ref()) {
        println!("         {note}");
    }
}

fn print_detection(setup: &FanSetup) {
    let detection = &setup.detection;
    match detection.state {
        DetectState::Idle => {}
        DetectState::Running => println!(
            "Detection running: {} of {} outputs done{}",
            detection.done,
            detection.total,
            detection
                .pwm
                .map_or_else(String::new, |pwm| format!(", testing pwm{pwm}"))
        ),
        DetectState::Finished | DetectState::Failed => {
            if let Some(message) = &detection.message {
                println!("Last detection: {message}");
            }
        }
    }
}

pub fn list(socket: &Path, json: bool) -> CliResult {
    let setup: FanSetup = ipc::call(socket, &Request::GetFans)?;
    if json {
        return print_json(serde_json::to_value(&setup)?);
    }
    println!(
        "Fan chip: {}{}",
        setup.controller.as_deref().unwrap_or("none found"),
        if setup.controller.is_some() && !setup.controller_verified {
            " (not yet verified on real hardware; test with care)"
        } else {
            ""
        }
    );
    println!(
        "Board: {}",
        setup.board.as_deref().unwrap_or("no board profile")
    );
    match &setup.reason {
        Some(reason) => println!("Curves cannot be edited yet: {reason}"),
        None => println!("Curves can be edited"),
    }
    if setup.needs_detection {
        println!("No board profile and no detection result: run `astra fans detect --execute`");
    }
    print_detection(&setup);
    if setup.thermal_hold {
        println!(
            "Thermal protection is active: curves are handed back to firmware until the CPU cools down"
        );
    }
    for channel in &setup.channels {
        describe_channel(channel);
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

pub fn detect(socket: &Path, execute: bool, json: bool) -> CliResult {
    let result: FanDetectResult = ipc::call(socket, &Request::DetectFans { execute })?;
    if json {
        return print_json(serde_json::to_value(&result)?);
    }
    for item in &result.plan {
        match (&item.reason, item.will_test) {
            (None, true) => println!(
                "  pwm{}: firmware duty {}%, will be raised",
                item.pwm, item.baseline_percent
            ),
            (Some(reason), _) => println!(
                "  pwm{}: firmware duty {}%, skipped: {reason}",
                item.pwm, item.baseline_percent
            ),
            (None, false) => {}
        }
    }
    if !result.executed {
        println!(
            "Preview only. Detection raises the speed of the listed outputs one at a time for about {} seconds in total, then restores them; add --execute to start it",
            result.estimated_seconds
        );
        return Ok(());
    }
    println!("Detection started in astrad; follow it with `astra fans presets`");
    Ok(())
}

pub fn role(socket: &Path, pwm: u8, role: RoleArg) -> CliResult {
    let role = match role {
        RoleArg::Fan => Some(FanRole::Fan),
        RoleArg::Pump => Some(FanRole::Pump),
        RoleArg::None => None,
    };
    let setup: FanSetup = ipc::call(socket, &Request::SetFanRole { pwm, role })?;
    match role {
        Some(role) => println!("pwm{pwm} confirmed as a {}", role_name(role)),
        None => println!("pwm{pwm}: confirmation removed"),
    }
    for channel in setup.channels.iter().filter(|c| c.pwm == pwm) {
        describe_channel(channel);
    }
    Ok(())
}

pub fn reset(socket: &Path, execute: bool, everything: bool) -> CliResult {
    let result: FanResetResult = ipc::call(
        socket,
        &Request::ResetFans {
            execute,
            everything,
        },
    )?;
    if result.removed_presets.is_empty() {
        println!("No saved presets");
    } else {
        println!("Saved presets: {}", result.removed_presets.join(", "));
    }
    if everything {
        println!("The detection result and the confirmed roles are forgotten as well");
    }
    if !result.executed {
        println!("Preview only; --execute deletes them and returns the fans to firmware control");
        return Ok(());
    }
    for error in &result.release_errors {
        eprintln!("warning: could not hand back {error}");
    }
    if !result.release_errors.is_empty() {
        println!("Saved presets were deleted, but not every fan could be handed back to firmware");
    } else if result.released.is_empty() {
        println!("Reset done; the fans are under firmware control");
    } else {
        let outputs: Vec<_> = result.released.iter().map(|p| format!("pwm{p}")).collect();
        println!(
            "Reset done; returned to firmware control: {}",
            outputs.join(", ")
        );
    }
    Ok(())
}
