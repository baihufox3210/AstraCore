//! Named lighting control through the reusable core API.
use crate::{
    cli::{ManagedAction, SetArgs},
    output::CliResult,
};
use asusio_core::lighting::{self, Config, Request, Store, hex};
use asusio_types::Rgb;

impl SetArgs {
    pub fn request(&self) -> Request<'_> {
        Request::new(&self.target, self.color)
            .brightness(self.brightness)
            .calibrated(!self.uncalibrated)
    }
}

pub fn print_planned(alias: &str, args: &SetArgs, output: Rgb) {
    println!(
        "{alias}: requested {} -> output {} (brightness {}%)",
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
use std::{fs, io, path::PathBuf};

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
        ManagedAction::Set(args) => {
            let plan = store.plan(&args.request())?;
            for item in &plan {
                print_planned(item.alias(), &args, item.output());
            }
            if !args.execute {
                println!("Preview only; add --execute to apply. Config: {path}");
                return Ok(());
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
