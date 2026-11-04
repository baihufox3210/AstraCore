//! Command dispatch. Each module owns presentation for one area of the CLI.
mod daemon;
mod diagnostics;
mod fans;
mod lighting;
mod profile;
mod system;

use crate::{
    cli::{Commands, LightingAction},
    output::CliResult,
};

pub fn run(command: Commands) -> CliResult {
    match command {
        Commands::Info => system::info(),
        Commands::Sensors { json } => system::sensors(json),
        Commands::Devices { json, hid } => system::devices(json, hid),
        Commands::Capabilities { json } => system::capabilities(json),
        Commands::HidDescriptor { device_id, json } => system::hid_descriptor(&device_id, json),
        Commands::Fans { action } => fans::run(action),
        Commands::Daemon { socket, action } => daemon::run(action, &socket),
        Commands::Profile { socket, action } => profile::run(action, &socket),
        Commands::Lighting { action, config } => match action {
            LightingAction::Managed(action) => lighting::run(action, config),
            LightingAction::Diagnostic(action) => diagnostics::run(action),
        },
    }
}
