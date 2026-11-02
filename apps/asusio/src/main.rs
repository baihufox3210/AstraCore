use asusio_discovery::discover_board;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "asusio",
    version,
    about = "ASUS Hardware I/O Framework for Linux"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Display motherboard information
    Info,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Info => {
            let board = discover_board()?;

            println!("ASUSIO Hardware Information");
            println!("Vendor : {}", board.vendor);
            println!("Board  : {}", board.model);
            println!(
                "BIOS   : {}",
                board.bios_version.as_deref().unwrap_or("Unknown")
            );
        }
    }

    Ok(())
}
