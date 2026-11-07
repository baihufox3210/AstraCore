//! Client commands for the astrad system service.
use super::lighting::{print_planned, report_outcomes};
use crate::{
    cli::{DaemonAction, LightArgs, SetArgs, SyncArgs},
    output::{CliResult, print_json},
};
use astracore_core::{
    lighting::{Availability, TargetKind, hex},
    service::{
        LightingMode, LightingSource, LightingState,
        ipc::{
            self, FanState, LightingReport, LightingResult, Request, Status, TargetResult,
            TargetStatus,
        },
    },
};
use std::path::{Path, PathBuf};

/// The daemon reads the configuration on the caller's behalf, so it needs a real path.
fn config_path(lighting_config: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(lighting_config)
}

fn mode_name(mode: LightingMode) -> &'static str {
    match mode {
        LightingMode::Individual => "individual",
        LightingMode::Sync => "sync",
    }
}

/// Print planned outputs, then (when executed) per-target outcomes and recording.
fn print_result(result: &LightingResult, light: Option<&LightArgs>) -> CliResult {
    for target in &result.targets {
        match light {
            Some(light) => print_planned(&target.alias, light, target.output),
            None => println!("{}: output {}", target.alias, hex(target.output)),
        }
        if !result.executed
            && let Some(error) = &target.error
        {
            println!("  not ready: {error}");
        }
    }
    if !result.executed {
        println!(
            "Preview only (via astrad, mode {}); add --execute to apply and keep across reboots",
            mode_name(result.mode)
        );
        return Ok(());
    }
    if let Some(error) = &result.persist_error {
        eprintln!("warning: applied, but not recorded for boot restore: {error}");
    }
    report(&result.targets)?;
    println!(
        "Recorded by astrad (mode {}); restored at boot",
        mode_name(result.mode)
    );
    Ok(())
}

/// `lighting set` through the daemon: no sudo needed, and executed requests are
/// recorded for boot restore. Executing switches to individual mode.
pub fn set(socket: &Path, lighting_config: &Path, args: &SetArgs) -> CliResult {
    let light = args.light.state();
    let request = Request::SetLighting {
        lighting_config: config_path(lighting_config)?,
        target: args.target.clone(),
        rgb: light.rgb,
        brightness: light.brightness,
        calibrated: light.calibrated,
        effect: light.effect,
        execute: args.execute,
    };
    let result: LightingResult = ipc::call(socket, &request)?;
    print_result(&result, Some(&args.light))
}

/// `lighting sync`: one setting for every target; executing switches to Sync mode.
pub fn sync(socket: &Path, lighting_config: &Path, args: &SyncArgs) -> CliResult {
    let LightingState {
        rgb,
        brightness,
        calibrated,
        effect,
    } = args.light.state();
    let request = Request::SetSync {
        lighting_config: config_path(lighting_config)?,
        rgb,
        brightness,
        calibrated,
        effect,
        execute: args.execute,
    };
    let result: LightingResult = ipc::call(socket, &request)?;
    print_result(&result, Some(&args.light))
}

pub fn mode(socket: &Path, lighting_config: &Path, mode: LightingMode, execute: bool) -> CliResult {
    let request = Request::SetLightingMode {
        lighting_config: config_path(lighting_config)?,
        mode,
        execute,
    };
    let result: LightingResult = ipc::call(socket, &request)?;
    if result.targets.is_empty() {
        println!("No settings recorded for {} mode", mode_name(mode));
    }
    print_result(&result, None)
}

pub fn source(
    socket: &Path,
    lighting_config: &Path,
    target: &str,
    source: LightingSource,
    execute: bool,
) -> CliResult {
    let request = Request::SetLightingSource {
        lighting_config: config_path(lighting_config)?,
        target: target.to_owned(),
        source,
        execute,
    };
    let result: LightingResult = ipc::call(socket, &request)?;
    print_result(&result, None)
}

fn describe(state: &LightingState) -> String {
    let calibration = if state.calibrated {
        "calibrated"
    } else {
        "uncalibrated"
    };
    format!(
        "{} {} {}% {calibration}",
        state.effect,
        hex(state.rgb),
        state.brightness
    )
}

fn readiness(availability: &Availability) -> String {
    match availability {
        Availability::Available => "available".to_owned(),
        Availability::NotReady { reason } => format!("not ready: {reason}"),
        Availability::Unavailable { reason } => format!("unavailable: {reason}"),
    }
}

/// Display order of device groups.
const KINDS: [TargetKind; 7] = [
    TargetKind::Board,
    TargetKind::Argb,
    TargetKind::Keyboard,
    TargetKind::Graphics,
    TargetKind::Memory,
    TargetKind::Screen,
    TargetKind::Other,
];

fn print_target(target: &TargetStatus) {
    let applied = target
        .applied
        .as_ref()
        .map_or_else(|| "nothing recorded".to_owned(), describe);
    let kind = serde_json::to_value(target.kind).unwrap_or_default();
    let source = match target.source {
        LightingSource::Sync => "sync",
        LightingSource::Custom => "custom",
    };
    println!(
        "  {:<10}{:<10}{:<8}{applied}",
        target.alias,
        kind.as_str().unwrap_or_default(),
        source
    );
    println!("            {}", readiness(&target.availability));
    if let Some(error) = &target.last_error {
        println!("            last write failed: {error}");
    }
}

pub fn lighting_status(socket: &Path, lighting_config: &Path, json: bool) -> CliResult {
    let request = Request::GetLighting {
        lighting_config: config_path(lighting_config)?,
    };
    let report: LightingReport = ipc::call(socket, &request)?;
    if json {
        return print_json(serde_json::to_value(&report)?);
    }
    println!("Mode: {}", mode_name(report.mode));
    match &report.sync {
        Some(sync) => println!("Sync: {}", describe(sync)),
        None => println!("Sync: not set"),
    }
    if !report.recorded {
        println!(
            "Note: {} is not the configuration astrad restores",
            report.lighting_config.display()
        );
    }
    for kind in KINDS {
        for target in report.targets.iter().filter(|target| target.kind == kind) {
            print_target(target);
        }
    }
    Ok(())
}

pub fn run(action: DaemonAction, socket: &Path) -> CliResult {
    match action {
        DaemonAction::Status { json } => {
            let status: Status = ipc::call(socket, &Request::Status)?;
            if json {
                return print_json(serde_json::to_value(&status)?);
            }
            println!("astrad {} ({})", status.version, socket.display());
            match &status.lighting_config {
                Some(path) => println!("Lighting config: {}", path.display()),
                None => println!("Lighting config: none recorded yet"),
            }
            println!("State file: {}", status.state_file.display());
            if status.restore.is_empty() {
                println!("Boot restore: nothing recorded");
            }
            for entry in &status.restore {
                let state = if entry.applied { "applied" } else { "pending" };
                print!(
                    "  restore {}: {state} after {} attempt(s)",
                    entry.alias, entry.attempts
                );
                match &entry.last_error {
                    Some(error) => println!(" — {error}"),
                    None => println!(),
                }
            }
            if status.fans.is_empty() {
                println!("Fan curves: none configured");
            }
            for fan in &status.fans {
                let state = match &fan.state {
                    FanState::Starting => "starting".to_owned(),
                    FanState::Active { celsius, percent } => {
                        format!("{celsius:.1} °C -> {percent}%")
                    }
                    FanState::Released { reason } => format!("firmware control ({reason})"),
                };
                println!("  pwm{} [{}]: {state}", fan.channel, fan.sensor);
            }
        }
        DaemonAction::Restore => {
            let results: Vec<TargetResult> = ipc::call(socket, &Request::RestoreLighting)?;
            if results.is_empty() {
                println!("Nothing recorded; run `astra lighting set ... --execute` first");
            }
            for target in &results {
                println!("{}: recorded {}", target.alias, hex(target.output));
            }
            report(&results)?;
        }
    }
    Ok(())
}

fn report(targets: &[TargetResult]) -> CliResult {
    report_outcomes(
        targets
            .iter()
            .map(|target| (target.alias.as_str(), target.error.clone())),
    )
}
