//! Named lighting control through the reusable core API.
use crate::{cli::ManagedAction, output::CliResult};
use asusio_core::lighting::{self, Config, Request, Store, hex};
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
        ManagedAction::Set {
            target,
            color,
            brightness,
            uncalibrated,
            execute,
        } => {
            let request = Request::new(&target, color)
                .brightness(brightness)
                .calibrated(!uncalibrated);
            let plan = store.plan(&request)?;
            for item in &plan {
                println!(
                    "{}: requested {} -> output {} (brightness {brightness}%)",
                    item.alias(),
                    hex(color),
                    hex(item.output())
                );
            }
            if !execute {
                println!("Preview only; add --execute to apply. Config: {path}");
                return Ok(());
            }
            let mut failed = 0;
            for outcome in lighting::execute(plan) {
                match outcome.result {
                    Ok(()) => println!(
                        "{}: command completed (physical rendering is not measured)",
                        outcome.alias
                    ),
                    Err(error) => {
                        failed += 1;
                        eprintln!("{}: {error}", outcome.alias);
                    }
                }
            }
            if failed > 0 {
                return Err(io::Error::other(format!(
                    "{failed} target(s) failed; other targets may already be updated"
                ))
                .into());
            }
        }
    }
    Ok(())
}
