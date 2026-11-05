//! CPU profile commands. Profiles are applied and recorded only by asusiod.
use crate::{
    cli::{ProfileAction, ProfileSetArgs},
    output::{CliResult, print_json},
};
use asusio_core::{
    cpu_profile::{Custom, FanPlan, Mode, Profile},
    service::ipc::{
        self, CpuSummary, GpuReading, ProfileReport, ProfileResult, Request, Telemetry,
    },
    telemetry::Sampler,
};
use std::{path::Path, time::Duration};

/// Without asusiod, load is measured over this short window.
const LOCAL_SAMPLE: Duration = Duration::from_millis(300);

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

fn optional<T: std::fmt::Display>(value: Option<T>, unit: &str) -> String {
    value.map_or_else(
        || "unavailable".to_owned(),
        |value| format!("{value}{unit}"),
    )
}

fn gib(bytes: u64) -> f64 {
    bytes as f64 / (1u64 << 30) as f64
}

fn print_gpu(gpu: &GpuReading) {
    println!(
        "GPU {}: {}",
        gpu.index,
        gpu.name.as_deref().unwrap_or("unknown")
    );
    println!(
        "  temperature {}  load {}  memory controller {}  P{}",
        optional(gpu.temperature_celsius, " °C"),
        optional(gpu.utilization_percent, "%"),
        optional(gpu.memory_utilization_percent, "%"),
        optional(gpu.pstate, "")
    );
    println!(
        "  power {} of {}",
        optional(gpu.power_watts.map(|w| format!("{w:.1}")), " W"),
        optional(gpu.power_limit_watts.map(|w| format!("{w:.0}")), " W")
    );
    println!(
        "  clocks graphics {} (max {})  memory {}",
        optional(gpu.graphics_clock_mhz, " MHz"),
        optional(gpu.max_graphics_clock_mhz, " MHz"),
        optional(gpu.memory_clock_mhz, " MHz")
    );
    if let (Some(used), Some(total)) = (gpu.memory_used_bytes, gpu.memory_total_bytes) {
        println!("  VRAM {:.1} / {:.1} GiB", gib(used), gib(total));
    }
    if let (Some(generation), Some(width)) = (gpu.pcie_generation, gpu.pcie_width) {
        println!("  PCIe Gen{generation} x{width}");
    }
    for fan in &gpu.fans {
        println!(
            "  fan {}: {} RPM, {} (target {})",
            fan.index,
            optional(fan.rpm, ""),
            optional(fan.percent, "%"),
            optional(fan.target_percent, "%")
        );
    }
    println!("  voltage: not provided by NVML");
}

fn status(socket: &Path, json: bool) -> CliResult {
    let (telemetry, local) = if ipc::available(socket) {
        (ipc::call::<Telemetry>(socket, &Request::Telemetry)?, false)
    } else {
        let mut sampler = Sampler::new();
        sampler.prime();
        std::thread::sleep(LOCAL_SAMPLE);
        (sampler.read(), true)
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
    let ccd: Vec<_> = telemetry
        .ccd_celsius
        .iter()
        .map(|ccd| format!("{} {:.1} °C", ccd.label, ccd.value))
        .collect();
    println!(
        "  Tctl {}  {}",
        optional(telemetry.tctl_celsius.map(|c| format!("{c:.1}")), " °C"),
        ccd.join("  ")
    );
    if let Some(usage) = &telemetry.cpu_usage {
        let cores: Vec<_> = usage
            .cores_percent
            .iter()
            .map(|core| format!("{core:.0}"))
            .collect();
        println!(
            "  load {:.1}%  per CPU [{}]",
            usage.total_percent,
            cores.join(" ")
        );
    } else {
        println!("  load unavailable");
    }
    if let Some(frequency) = &telemetry.cpu_frequency {
        println!(
            "  frequency average {} MHz, highest {} MHz",
            frequency.average_mhz, frequency.max_mhz
        );
    }
    println!(
        "  power package {}  cores {}",
        optional(telemetry.package_watts.map(|w| format!("{w:.1}")), " W"),
        optional(telemetry.core_watts.map(|w| format!("{w:.1}")), " W")
    );
    println!(
        "  scheduler {}",
        telemetry
            .scheduler
            .as_deref()
            .unwrap_or("kernel default (no sched_ext)")
    );
    if let Some(memory) = &telemetry.memory {
        println!(
            "Memory: {:.1} / {:.1} GiB",
            gib(memory.used_bytes),
            gib(memory.total_bytes)
        );
    }
    for gpu in &telemetry.gpus {
        print_gpu(gpu);
    }
    if let Some(error) = &telemetry.gpu_error {
        println!("GPU: {error}");
    }
    if !telemetry.voltages.is_empty() {
        let voltages: Vec<_> = telemetry
            .voltages
            .iter()
            .map(|v| format!("{} {:.3} V", v.label, v.value))
            .collect();
        println!(
            "Voltages ({}): {}",
            telemetry.board.as_deref().unwrap_or("board"),
            voltages.join(", ")
        );
    }
    println!("Fans and pumps:");
    for fan in &telemetry.fans {
        let kind = serde_json::to_value(fan.kind)?;
        let name = match (&fan.header, &fan.label) {
            (Some(header), Some(label)) => format!("{header} ({label})"),
            _ => format!("fan{}", fan.channel),
        };
        let control = match &fan.pwm {
            Some(pwm) => {
                let control = serde_json::to_value(pwm.control)?;
                format!(
                    "pwm{} {}% {}",
                    pwm.channel,
                    pwm.percent,
                    control.as_str().unwrap_or_default()
                )
            }
            None => "no PWM output".to_owned(),
        };
        println!(
            "  {name} [{}]: {} RPM, {control}",
            kind.as_str().unwrap_or_default(),
            fan.rpm
        );
    }
    if !telemetry.unused_headers.is_empty() {
        println!("  not connected: {}", telemetry.unused_headers.join(", "));
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
        let mut builtin = args(Mode::Performance);
        builtin.boost = Some(false);
        assert!(custom_from(&builtin, None).is_err());
    }
}
