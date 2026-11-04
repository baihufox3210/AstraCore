//! Installs or removes the asusiod system integration. Previews by default;
//! `--execute` writes files (run with sudo). It never runs systemctl or edits groups.
use asusio_core::{
    lighting::Store,
    service::{DEFAULT_CONFIG, DEFAULT_GROUP, ServiceConfig},
};
use clap::{Parser, Subcommand};
use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::ExitCode,
};

const UNIT_TEMPLATE: &str = include_str!("../../../packaging/systemd/asusiod.service.in");
const UNIT_PATH: &str = "/etc/systemd/system/asusiod.service";
const MODULES_PATH: &str = "/etc/modules-load.d/asusio.conf";
/// i2c-dev exposes ENE controllers; nct6775 exposes fan PWM channels.
const MODULES: &str = "i2c-dev\nnct6775\n";

#[derive(Parser)]
#[command(
    name = "asusio-helper",
    version,
    about = "Install the asusiod system service"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Install the unit, daemon configuration, module loading and binaries
    Install {
        /// The user's lighting configuration, e.g. /home/me/.config/asusio/lighting.json
        #[arg(long)]
        lighting_config: PathBuf,
        /// Group allowed to control lighting through the daemon socket
        #[arg(long, default_value = DEFAULT_GROUP)]
        group: String,
        /// Where asusiod and asusio are installed
        #[arg(long, default_value = "/usr/local/bin")]
        bin_dir: PathBuf,
        #[arg(long)]
        execute: bool,
    },
    /// Remove the unit and module-loading file (keeps configuration and state)
    Uninstall {
        #[arg(long)]
        execute: bool,
    },
}

enum Action {
    /// Create or replace a file this tool owns.
    Write {
        path: PathBuf,
        content: String,
        mode: u32,
    },
    /// Create only when absent; never overwrite user edits.
    Create {
        path: PathBuf,
        content: String,
        mode: u32,
    },
    Copy {
        from: PathBuf,
        to: PathBuf,
    },
    Remove {
        path: PathBuf,
    },
}

impl Action {
    fn describe(&self) -> String {
        match self {
            Self::Write { path, .. } => format!("write   {}", path.display()),
            Self::Create { path, .. } if path.exists() => {
                format!("keep    {} (already exists)", path.display())
            }
            Self::Create { path, .. } => format!("create  {}", path.display()),
            Self::Copy { from, to } => format!("install {} -> {}", from.display(), to.display()),
            Self::Remove { path } => format!("remove  {}", path.display()),
        }
    }

    fn apply(&self) -> io::Result<()> {
        let write = |path: &Path, content: &str, mode: u32| -> io::Result<()> {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, content)?;
            fs::set_permissions(path, fs::Permissions::from_mode(mode))
        };
        match self {
            Self::Write {
                path,
                content,
                mode,
            } => write(path, content, *mode),
            Self::Create { path, .. } if path.exists() => Ok(()),
            Self::Create {
                path,
                content,
                mode,
            } => write(path, content, *mode),
            Self::Copy { from, to } => {
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent)?;
                }
                // Copy to a temporary name, then rename, so a running binary is not truncated.
                let temp = to.with_extension("asusio-new");
                fs::copy(from, &temp)?;
                fs::set_permissions(&temp, fs::Permissions::from_mode(0o755))?;
                fs::rename(&temp, to)
            }
            Self::Remove { path } => match fs::remove_file(path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                result => result,
            },
        }
    }
}

fn render_unit(bin_dir: &Path, config: &Path) -> String {
    UNIT_TEMPLATE
        .replace("@BINDIR@", &bin_dir.display().to_string())
        .replace("@CONFIG@", &config.display().to_string())
}

fn install_actions(lighting_config: &Path, group: &str, bin_dir: &Path) -> io::Result<Vec<Action>> {
    if !lighting_config.is_absolute() || !bin_dir.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--lighting-config and --bin-dir must be absolute paths",
        ));
    }
    // Validates schema and host binding before anything is installed.
    Store::new(lighting_config).load()?;
    let mut config = ServiceConfig::new(lighting_config);
    config.client_group = Some(group.to_owned());
    config.validate()?;
    let config_json = serde_json::to_string_pretty(&config).map_err(io::Error::other)? + "\n";

    let source_dir = std::env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut actions = Vec::new();
    for binary in ["asusiod", "asusio"] {
        let from = source_dir.join(binary);
        if !from.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} not found; build the workspace first", from.display()),
            ));
        }
        actions.push(Action::Copy {
            from,
            to: bin_dir.join(binary),
        });
    }
    actions.extend([
        Action::Create {
            path: DEFAULT_CONFIG.into(),
            content: config_json,
            mode: 0o644,
        },
        Action::Write {
            path: UNIT_PATH.into(),
            content: render_unit(bin_dir, Path::new(DEFAULT_CONFIG)),
            mode: 0o644,
        },
        Action::Create {
            path: MODULES_PATH.into(),
            content: MODULES.to_owned(),
            mode: 0o644,
        },
    ]);
    Ok(actions)
}

fn run(cli: Cli) -> io::Result<()> {
    let (actions, execute, next_steps) = match cli.command {
        Command::Install {
            lighting_config,
            group,
            bin_dir,
            execute,
        } => {
            let user = std::env::var("SUDO_USER").unwrap_or_else(|_| "$USER".to_owned());
            (
                install_actions(&lighting_config, &group, &bin_dir)?,
                execute,
                vec![
                    format!("sudo groupadd -r {group}   # if the group does not exist"),
                    format!("sudo usermod -aG {group} {user}   # then log out and back in"),
                    "sudo systemctl daemon-reload".to_owned(),
                    "sudo systemctl enable --now asusiod".to_owned(),
                    "asusio daemon set all <RRGGBB> --execute   # recorded for boot restore"
                        .to_owned(),
                ],
            )
        }
        Command::Uninstall { execute } => (
            vec![
                Action::Remove {
                    path: UNIT_PATH.into(),
                },
                Action::Remove {
                    path: MODULES_PATH.into(),
                },
            ],
            execute,
            vec![
                "sudo systemctl disable --now asusiod   # before removing the unit".to_owned(),
                "sudo systemctl daemon-reload".to_owned(),
                format!("kept: {DEFAULT_CONFIG}, /var/lib/asusio (remove manually if unwanted)"),
            ],
        ),
    };
    for action in &actions {
        println!("{}", action.describe());
        if execute {
            action.apply()?;
        }
    }
    if !execute {
        println!("Preview only; add --execute (with sudo) to apply");
    }
    println!("\nNext steps:");
    for step in next_steps {
        println!("  {step}");
    }
    Ok(())
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("asusio-helper: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_template_is_fully_rendered() {
        let unit = render_unit(Path::new("/usr/local/bin"), Path::new(DEFAULT_CONFIG));
        assert!(!unit.contains('@'));
        assert!(
            unit.contains("ExecStart=/usr/local/bin/asusiod --config /etc/asusio/asusiod.json")
        );
        assert!(unit.contains("--release-fans"));
    }
}
