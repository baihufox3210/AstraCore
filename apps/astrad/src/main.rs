//! astrad: restores recorded lighting and CPU profile at boot, runs fan curves, and
//! serves lighting and profile requests to authorized local users over a Unix socket.
mod daemon;
mod fan_detect;
mod fan_presets;
mod fans;
mod lighting;
mod profile;
mod server;
mod signals;

use astracore_core::{
    service::{DEFAULT_CONFIG, DEFAULT_STATE, LEGACY_STATE, ServiceConfig, State, carry_over},
    telemetry::Sampler,
};
use clap::Parser;
use std::{
    io,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    thread,
};

#[derive(Parser)]
#[command(name = "astrad", version, about = "AstraCore hardware daemon")]
struct Args {
    #[arg(long, default_value = DEFAULT_CONFIG)]
    config: PathBuf,
    /// Return every configured fan-curve channel to firmware control, then exit.
    /// Used by systemd's ExecStopPost so a crash never leaves fans in manual mode.
    #[arg(long)]
    release_fans: bool,
    /// Diagnostic: ignore the built-in board profile, as on a board AstraCore does not know.
    /// Roles must then be confirmed and detection run before curves can be saved.
    #[arg(long)]
    no_board_profile: bool,
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("astrad: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> io::Result<()> {
    let config = ServiceConfig::load(&args.config)?;
    // First start after the rename from asusio: carry the recorded state over.
    if config.state_file == Path::new(DEFAULT_STATE)
        && carry_over(Path::new(LEGACY_STATE), &config.state_file)?
    {
        eprintln!("state: carried over from {LEGACY_STATE}");
    }
    if args.release_fans {
        // Every output astrad may have been driving: the configuration's, the saved presets'
        // and every one it ever took over (its firmware mode was recorded first), each
        // restored to the mode recorded for it, so a crash never leaves a fan at a manual
        // duty. Outputs that were never touched are left alone.
        let mut channels = config.fan_channels();
        let mut recorded = std::collections::BTreeMap::new();
        if let Ok(state) = State::load(&config.state_file) {
            channels.extend(state.user_fan_presets.values().flatten().map(|c| c.channel));
            channels.extend(state.firmware_enable.keys());
            recorded = state.firmware_enable;
        }
        channels.sort_unstable();
        channels.dedup();
        return fans::release_all(&channels, &recorded);
    }
    // A panic in any thread must end the process so ExecStopPost releases the fans.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        std::process::abort();
    }));
    signals::install()?;

    let sampler = if args.no_board_profile {
        eprintln!("board profile ignored (--no-board-profile)");
        Sampler::without_board_profile()
    } else {
        Sampler::new()
    };
    let daemon = Arc::new(daemon::Daemon::new(config, sampler)?);
    // Bind the fan knowledge in the state file to this machine, then refuse a configuration
    // whose curves would drive a pump.
    daemon.sync_fan_hardware();
    daemon.check_config_pumps()?;
    eprintln!("astrad {} started", env!("CARGO_PKG_VERSION"));
    // Before the fan loop starts, so it begins with the recorded profile's curves.
    daemon.restore_profile_at_startup();
    let restore = daemon.config.restore_lighting.then(|| {
        let daemon = daemon.clone();
        thread::spawn(move || daemon.restore_at_startup())
    });
    // Always running: curves can be saved through the socket at any time, and fan detection
    // relies on the loop to hand outputs back first. With nothing to drive it only sleeps.
    let fans = Some({
        let daemon = daemon.clone();
        thread::spawn(move || fans::run(&daemon))
    });
    let served = server::serve(&daemon);
    signals::request_stop();
    for handle in [fans, restore].into_iter().flatten() {
        let _ = handle.join();
    }
    eprintln!("astrad stopped");
    served
}
