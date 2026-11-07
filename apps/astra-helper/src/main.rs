//! Installs or removes astrad without a package manager. Previews by default;
//! `--execute` (as root) installs files, loads modules and enables the service.
use astracore_core::service::DEFAULT_CONFIG;
use clap::{Parser, Subcommand};
use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{self, ExitCode},
};

const UNIT_TEMPLATE: &str = include_str!("../../../packaging/systemd/astrad.service.in");
const UNIT_PATH: &str = "/etc/systemd/system/astrad.service";
const MODULES_PATH: &str = "/etc/modules-load.d/astracore.conf";
/// i2c-dev exposes ENE controllers; nct6775 exposes fan PWM channels.
const MODULES: &str = include_str!("../../../packaging/systemd/astracore-modules.conf");

#[derive(Parser)]
#[command(
    name = "astra-helper",
    version,
    about = "Install astrad without a package"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Install binaries, the unit and module loading, then enable the service
    Install {
        /// Where astrad and astra are installed
        #[arg(long, default_value = "/usr/local/bin")]
        bin_dir: PathBuf,
        #[arg(long)]
        execute: bool,
    },
    /// Disable the service and remove the unit and module file (keeps state)
    Uninstall {
        #[arg(long)]
        execute: bool,
    },
}

enum Action {
    Write {
        path: PathBuf,
        content: String,
    },
    Copy {
        from: PathBuf,
        to: PathBuf,
    },
    Remove {
        path: PathBuf,
    },
    Run {
        program: &'static str,
        args: &'static [&'static str],
    },
}

impl Action {
    fn describe(&self) -> String {
        match self {
            Self::Write { path, .. } => format!("write   {}", path.display()),
            Self::Copy { from, to } => format!("install {} -> {}", from.display(), to.display()),
            Self::Remove { path } => format!("remove  {}", path.display()),
            Self::Run { program, args } => format!("run     {program} {}", args.join(" ")),
        }
    }

    fn apply(&self) -> io::Result<()> {
        match self {
            Self::Write { path, content } => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(path, content)?;
                fs::set_permissions(path, fs::Permissions::from_mode(0o644))
            }
            Self::Copy { from, to } => {
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent)?;
                }
                // Copy to a temporary name, then rename, so a running binary is not truncated.
                let temp = to.with_extension("astracore-new");
                fs::copy(from, &temp)?;
                fs::set_permissions(&temp, fs::Permissions::from_mode(0o755))?;
                fs::rename(&temp, to)
            }
            Self::Remove { path } => match fs::remove_file(path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                result => result,
            },
            Self::Run { program, args } => {
                let status = process::Command::new(program).args(*args).status()?;
                if status.success() {
                    Ok(())
                } else {
                    Err(io::Error::other(format!("{program} failed: {status}")))
                }
            }
        }
    }
}

fn render_unit(bin_dir: &Path) -> String {
    UNIT_TEMPLATE
        .replace("@BINDIR@", &bin_dir.display().to_string())
        .replace("@CONFIG@", DEFAULT_CONFIG)
}

fn install_actions(bin_dir: &Path) -> io::Result<Vec<Action>> {
    if !bin_dir.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--bin-dir must be an absolute path",
        ));
    }
    let source_dir = std::env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut actions = Vec::new();
    for binary in ["astrad", "astra"] {
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
        Action::Write {
            path: UNIT_PATH.into(),
            content: render_unit(bin_dir),
        },
        Action::Write {
            path: MODULES_PATH.into(),
            content: MODULES.to_owned(),
        },
        Action::Run {
            program: "modprobe",
            args: &["-a", "i2c-dev", "nct6775"],
        },
        Action::Run {
            program: "systemctl",
            args: &["daemon-reload"],
        },
        Action::Run {
            program: "systemctl",
            args: &["enable", "--now", "astrad.service"],
        },
    ]);
    Ok(actions)
}

fn uninstall_actions() -> Vec<Action> {
    vec![
        Action::Run {
            program: "systemctl",
            args: &["disable", "--now", "astrad.service"],
        },
        Action::Remove {
            path: UNIT_PATH.into(),
        },
        Action::Remove {
            path: MODULES_PATH.into(),
        },
        Action::Run {
            program: "systemctl",
            args: &["daemon-reload"],
        },
    ]
}

fn run(cli: Cli) -> io::Result<()> {
    let (actions, execute) = match cli.command {
        Command::Install { bin_dir, execute } => (install_actions(&bin_dir)?, execute),
        Command::Uninstall { execute } => (uninstall_actions(), execute),
    };
    for action in &actions {
        println!("{}", action.describe());
        if execute {
            action.apply()?;
        }
    }
    if execute {
        println!("Done. Set lighting once with: astra lighting set all <RRGGBB> --execute");
    } else {
        println!("Preview only; add --execute (with sudo) to apply");
    }
    Ok(())
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("astra-helper: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_template_is_fully_rendered() {
        let unit = render_unit(Path::new("/usr/local/bin"));
        assert!(!unit.contains('@'));
        assert!(
            unit.contains("ExecStart=/usr/local/bin/astrad --config /etc/astracore/astrad.json")
        );
        assert!(unit.contains("--release-fans"));
        assert_eq!(MODULES, "i2c-dev\nnct6775\n");
    }
}
