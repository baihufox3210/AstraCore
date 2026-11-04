//! Named lighting control through the reusable core API.
use crate::{
    cli::{LightArgs, ManagedAction},
    output::CliResult,
};
use asusio_core::{
    lighting::{self, Config, Request, Store, hex},
    service::{DEFAULT_SOCKET, LightingState, ipc},
};
use asusio_types::Rgb;

impl LightArgs {
    pub fn state(&self) -> LightingState {
        LightingState {
            rgb: self.color,
            brightness: self.brightness,
            calibrated: !self.uncalibrated,
            effect: self.effect,
        }
    }

    pub fn request<'a>(&self, target: &'a str) -> Request<'a> {
        self.state().request(target)
    }
}

pub fn print_planned(alias: &str, args: &LightArgs, output: Rgb) {
    println!(
        "{alias}: requested {} {} -> output {} (brightness {}%)",
        args.effect,
        hex(args.color),
        hex(output),
        args.brightness
    );
}

/// Report per-target results; any failure makes the command fail.
pub fn report_outcomes<'a>(
    outcomes: impl IntoIterator<Item = (&'a str, Option<String>)>,
) -> CliResult {
    let mut failed = 0;
    for (alias, error) in outcomes {
        match error {
            None => println!("{alias}: command completed (physical rendering is not measured)"),
            Some(error) => {
                failed += 1;
                eprintln!("{alias}: {error}");
            }
        }
    }
    if failed > 0 {
        return Err(io::Error::other(format!(
            "{failed} target(s) failed; other targets may already be updated"
        ))
        .into());
    }
    Ok(())
}
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub fn run(action: ManagedAction, config: Option<PathBuf>) -> CliResult {
    let store = Store::open(config)?;
    let path = store.path().display();
    match action {
        ManagedAction::Setup { from } => {
            let config: Config = serde_json::from_slice(&fs::read(from)?)?;
            store.install(config)?;
            println!("Local configuration saved: {path}");
        }
        ManagedAction::Targets { json } => {
            let config = store.load()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&config)?);
            } else {
                println!("Config: {path}");
                for (alias, target) in config.targets {
                    println!(
                        "{alias}: white balance {} ({:?})",
                        hex(target.white_balance),
                        target.device
                    );
                }
            }
        }
        ManagedAction::Calibrate { target, gains } => {
            store.calibrate(&target, gains)?;
            println!(
                "{target}: white balance {} saved to {path} (hardware unchanged)",
                hex(gains)
            );
        }
        ManagedAction::Sync(args) => {
            return super::daemon::sync(Path::new(DEFAULT_SOCKET), store.path(), &args);
        }
        ManagedAction::Mode { mode, execute } => {
            return super::daemon::mode(Path::new(DEFAULT_SOCKET), store.path(), mode, execute);
        }
        ManagedAction::Status { json } => {
            return super::daemon::lighting_status(Path::new(DEFAULT_SOCKET), store.path(), json);
        }
        ManagedAction::Set(args) => {
            let socket = Path::new(DEFAULT_SOCKET);
            if !args.direct && ipc::available(socket) {
                return super::daemon::set(socket, store.path(), &args);
            }
            let plan = store.plan(&args.light.request(&args.target))?;
            for item in &plan {
                print_planned(item.alias(), &args.light, item.output());
            }
            if !args.execute {
                println!("Preview only; add --execute to apply. Config: {path}");
                return Ok(());
            }
            if ipc::available(socket) {
                println!("Writing directly; not recorded for boot restore");
            } else {
                println!("asusiod is not running; not recorded for boot restore");
            }
            let outcomes = lighting::execute(plan);
            report_outcomes(outcomes.iter().map(|outcome| {
                (
                    outcome.alias.as_str(),
                    outcome.result.as_ref().err().map(ToString::to_string),
                )
            }))?;
        }
    }
    Ok(())
}
