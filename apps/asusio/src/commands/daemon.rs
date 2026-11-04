//! Client commands for the asusiod system service.
use super::lighting::{print_planned, report_outcomes};
use crate::{
    cli::DaemonAction,
    output::{CliResult, print_json},
};
use asusio_core::{
    lighting::hex,
    service::ipc::{self, FanState, LightingResult, Request, Status, TargetResult},
};
use std::path::Path;

pub fn run(action: DaemonAction, socket: &Path) -> CliResult {
    match action {
        DaemonAction::Status { json } => {
            let status: Status = ipc::call(socket, &Request::Status)?;
            if json {
                return print_json(serde_json::to_value(&status)?);
            }
            println!("asusiod {} ({})", status.version, socket.display());
            println!("Lighting config: {}", status.lighting_config.display());
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
        DaemonAction::Set(args) => {
            let request = Request::SetLighting {
                target: args.target.clone(),
                rgb: args.color,
                brightness: args.brightness,
                calibrated: !args.uncalibrated,
                execute: args.execute,
            };
            let result: LightingResult = ipc::call(socket, &request)?;
            for target in &result.targets {
                print_planned(&target.alias, &args, target.output);
            }
            if !result.executed {
                println!("Preview only; add --execute to apply and record for boot restore");
                return Ok(());
            }
            if let Some(error) = &result.persist_error {
                eprintln!("warning: applied, but not recorded for boot restore: {error}");
            }
            report(&result.targets)?;
        }
        DaemonAction::Restore => {
            let results: Vec<TargetResult> = ipc::call(socket, &Request::RestoreLighting)?;
            if results.is_empty() {
                println!("Nothing recorded; use `asusio daemon set ... --execute` first");
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
