mod cli;
mod commands;
mod output;

use clap::Parser;

fn main() -> output::CliResult {
    commands::run(cli::Cli::parse().command)
}
