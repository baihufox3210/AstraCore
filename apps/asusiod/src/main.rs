//! asusiod: restores recorded lighting at boot, runs fan curves, and serves lighting
//! requests to authorized local users over a Unix socket.
mod daemon;
mod fans;
mod server;
mod signals;

use asusio_core::service::{DEFAULT_CONFIG, ServiceConfig};
use clap::Parser;
use std::{io, path::PathBuf, process::ExitCode, sync::Arc, thread};

#[derive(Parser)]
#[command(name = "asusiod", version, about = "asusio hardware daemon")]
struct Args {
    #[arg(long, default_value = DEFAULT_CONFIG)]
    config: PathBuf,
    /// Return every configured fan-curve channel to firmware control, then exit.
    /// Used by systemd's ExecStopPost so a crash never leaves fans in manual mode.
    #[arg(long)]
    release_fans: bool,
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("asusiod: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> io::Result<()> {
    let config = ServiceConfig::load(&args.config)?;
    if args.release_fans {
        return fans::release_all(&config.fan_curves);
    }
    // A panic in any thread must end the process so ExecStopPost releases the fans.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        std::process::abort();
    }));
    signals::install()?;

    let daemon = Arc::new(daemon::Daemon::new(config)?);
    eprintln!("asusiod {} started", env!("CARGO_PKG_VERSION"));
    let restore = daemon.config.restore_lighting.then(|| {
        let daemon = daemon.clone();
        thread::spawn(move || daemon.restore_at_startup())
    });
    let fans = (!daemon.config.fan_curves.is_empty()).then(|| {
        let daemon = daemon.clone();
        thread::spawn(move || fans::run(&daemon))
    });
    let served = server::serve(&daemon);
    signals::request_stop();
    for handle in [fans, restore].into_iter().flatten() {
        let _ = handle.join();
    }
    eprintln!("asusiod stopped");
    served
}
