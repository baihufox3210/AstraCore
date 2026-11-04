mod cli;
mod commands;
mod output;

use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    match commands::run(cli::Cli::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::FAILURE
        }
    }
}
