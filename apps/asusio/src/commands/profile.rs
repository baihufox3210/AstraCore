//! CPU profile commands. Profiles are applied and recorded only by asusiod.
use crate::{
    cli::{ProfileAction, ProfileSetArgs},
    output::{CliResult, print_json},
};
use asusio_core::{
    cpu_profile::{Custom, FanPlan, Mode, Profile},
    service::ipc::{self, CpuSummary, ProfileReport, ProfileResult, Request, Telemetry},
    telemetry,
};
use std::path::Path;

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn describe_fan(plan: &FanPlan) -> String {
    let base = match (&plan.preset, plan.firmware) {
        (Some(name), _) => format!("preset {name}"),
        (None, true) => "firmware control".to_owned(),
        (None, false) => "default curves".to_owned(),
    };
    match &plan.note {
        Some(note) => format!("{base} ({note})"),
        None => base,
    }
}

fn describe_profile(profile: &Profile) -> String {
    format!(
        "governor={} epp={} boost={}; fans: {}",
        profile.cpu.governor,
        profile.cpu.epp,
        on_off(profile.cpu.boost),
        describe_fan(&profile.fan)
    )
}

fn describe_cpu(cpu: &CpuSummary) -> String {
    let value = |value: &Option<String>| value.clone().unwrap_or_else(|| "mixed".into());
    let mut text = format!(
        "governor={} epp={} boost={}",
        value(&cpu.governor),
        value(&cpu.epp),
        cpu.boost.map_or("unknown", on_off)
    );
    if let Some(reason) = &cpu.reason {
        text.push_str(&format!(" (unsupported: {reason})"));
    }
    text
}

fn sync_label(in_sync: Option<bool>) -> &'static str {
    if in_sync == Some(false) {
        " (CPU state changed outside asusio)"
    } else {
        ""
    }
}

/// Merge `--governor/--epp/--boost/--fan-preset` over the stored custom profile.
fn custom_from(args: &ProfileSetArgs, stored: Option<Custom>) -> Result<Option<Custom>, String> {
    let given = args.governor.is_some()
        || args.epp.is_some()
        || args.boost.is_some()
        || args.fan_preset.is_some();
    if !given {
        return Ok(None);
    }
    if args.mode != Mode::Custom {
        return Err("--governor, --epp, --boost and --fan-preset apply to `custom` only".into());
    }
    let mut custom = stored.unwrap_or_default();
    if let Some(governor) = args.governor {
        custom.governor = governor;
    }
    if let Some(epp) = args.epp {
        custom.epp = epp;
    }
    if let Some(boost) = args.boost {
        custom.boost = boost;
    }
    if let Some(preset) = &args.fan_preset {
        custom.fan_preset = (preset != "default").then(|| preset.clone());
    }
    Ok(Some(custom))
}

fn list(socket: &Path, json: bool) -> CliResult {
    let report: ProfileReport = ipc::call(socket, &Request::GetProfiles)?;
    if json {
        return print_json(serde_json::to_value(&report)?);
    }
    for profile in &report.profiles {
        let marker = if report.active == Some(profile.mode) {
            "*"
        } else {
            " "
        };
        println!("{marker} {:<12}{}", profile.mode, describe_profile(profile));
    }
    if report.custom.is_none() {
        println!(
            "  {:<12}not defined; use `profile set custom --epp ...`",
            Mode::Custom
        );
    }
    match report.active {
        Some(mode) => println!("Recorded: {mode}{}", sync_label(report.in_sync)),
        None => println!("Recorded: none (CPU settings are left unchanged at boot)"),
    }
    println!("CPU: {}", describe_cpu(&report.cpu));
    if report.fan_presets.is_empty() {
        println!("Fan presets: none defined; fans stay under firmware control");
    } else {
        println!("Fan presets: {}", report.fan_presets.join(", "));
    }
    if let Some(error) = &report.last_error {
        println!("Last error: {error}");
    }
    Ok(())
}

fn status(socket: &Path, json: bool) -> CliResult {
    let (telemetry, local) = if ipc::available(socket) {
        (ipc::call::<Telemetry>(socket, &Request::Telemetry)?, false)
    } else {
        (telemetry::read(None), true)
    };
    if json {
        let mut value = serde_json::to_value(&telemetry)?;
        value["source"] = if local { "local" } else { "asusiod" }.into();
        return print_json(value);
    }
    if local {
        println!("asusiod is not running; showing local readings without profile or power");
    }
    match telemetry.mode {
        Some(mode) => println!("Profile: {mode}{}", sync_label(telemetry.in_sync)),
        None => println!("Profile: none recorded"),
    }
    println!("CPU: {}", describe_cpu(&telemetry.cpu));
    println!(
        "Scheduler: {}",
        telemetry
            .scheduler
            .as_deref()
            .unwrap_or("kernel default (no sched_ext)")
    );
    match telemetry.tctl_celsius {
        Some(celsius) => println!("Tctl: {celsius:.1} °C"),
        None => println!("Tctl: unavailable"),
    }
    match telemetry.package_watts {
        Some(watts) => println!("Package power: {watts:.1} W"),
        None => println!("Package power: unavailable"),
    }
    for fan in &telemetry.fans {
        println!("  fan{}: {} RPM", fan.channel, fan.rpm);
    }
    for pwm in &telemetry.pwm {
        let control = serde_json::to_value(pwm.control)?;
        println!(
            "  pwm{}: {}% {}",
            pwm.channel,
            pwm.percent,
            control.as_str().unwrap_or_default()
        );
    }
    if let Some(plan) = &telemetry.fan {
        println!("Fan curves: {}", describe_fan(plan));
    }
    for error in &telemetry.errors {
        eprintln!("warning: {error}");
    }
    Ok(())
}

fn set(socket: &Path, args: &ProfileSetArgs) -> CliResult {
    let stored = if args.mode == Mode::Custom {
        ipc::call::<ProfileReport>(socket, &Request::GetProfiles)?.custom
    } else {
        None
    };
    let request = Request::SetProfile {
        mode: args.mode,
        custom: custom_from(args, stored)?,
        execute: args.execute,
    };
    let result: ProfileResult = ipc::call(socket, &request)?;
    if args.json {
        return print_json(serde_json::to_value(&result)?);
    }
    println!(
        "{}: {}",
        result.profile.mode,
        describe_profile(&result.profile)
    );
    if !result.executed {
        println!("Preview only (via asusiod); add --execute to apply and keep across reboots");
        return Ok(());
    }
    if let Some(cpu) = &result.cpu {
        println!("Applied: {}", describe_cpu(cpu));
    }
    match &result.persist_error {
        Some(error) => eprintln!("warning: applied, but not recorded for boot restore: {error}"),
        None => println!("Recorded by asusiod; restored at boot"),
    }
    Ok(())
}

pub fn run(action: ProfileAction, socket: &Path) -> CliResult {
    match action {
        ProfileAction::List { json } => list(socket, json),
        ProfileAction::Status { json } => status(socket, json),
        ProfileAction::Set(args) => set(socket, &args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asusio_core::cpu_profile::{Epp, Governor};

    fn args(mode: Mode) -> ProfileSetArgs {
        ProfileSetArgs {
            mode,
            governor: None,
            epp: None,
            boost: None,
            fan_preset: None,
            execute: false,
            json: false,
        }
    }

    #[test]
    fn custom_flags_merge_over_the_stored_profile() {
        assert_eq!(
            custom_from(&args(Mode::Custom), None),
            Ok(None),
            "use stored"
        );
        let mut flags = args(Mode::Custom);
        flags.epp = Some(Epp::Power);
        flags.fan_preset = Some("default".into());
        let stored = Custom {
            governor: Governor::Powersave,
            epp: Epp::Performance,
            boost: false,
            fan_preset: Some("silent".into()),
        };
        let merged = custom_from(&flags, Some(stored)).unwrap().unwrap();
        assert_eq!(
            (merged.epp, merged.boost, merged.fan_preset),
            (Epp::Power, false, None)
        );
        let mut builtin = args(Mode::Turbo);
        builtin.boost = Some(false);
        assert!(custom_from(&builtin, None).is_err());
    }
}
