//! Persistent host-local configuration; no hardware writes.
use asusio_backends::{aura_hid, ene_smbus};
use asusio_types::Rgb;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
/// Unity channel gains: no white-balance compensation.
pub fn neutral() -> Rgb {
    [255; 3]
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    #[serde(default)]
    pub host_fingerprint: Option<String>,
    pub targets: BTreeMap<String, Target>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    #[serde(default = "neutral")]
    pub white_balance: Rgb,
    pub device: Device,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case", deny_unknown_fields)]
pub enum Device {
    AuraArgb {
        physical_path: String,
        headers: Vec<u8>,
    },
    AuraBoard {
        physical_path: String,
        locations: Vec<u8>,
    },
    ScopeKeyboard {
        physical_path: String,
    },
    Ene {
        adapter_name: String,
        adapter_parent: PathBuf,
        address: u8,
        firmware: String,
        led_count: u8,
    },
    Screen {
        usb_id: String,
        daemon_socket: PathBuf,
    },
}

pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
fn fingerprint(value: &str) -> String {
    // Stable host binding, not an authentication mechanism. Do not persist the machine-id itself.
    let hash = value
        .trim()
        .bytes()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    format!("{hash:016x}")
}
pub fn host() -> io::Result<String> {
    let value = fs::read_to_string("/etc/machine-id")?;
    if value.trim().len() != 32 || !value.trim().bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid(
            "invalid machine-id; cannot bind lighting configuration",
        ));
    }
    Ok(fingerprint(&value))
}
pub fn config_path(explicit: Option<PathBuf>) -> io::Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    if std::env::var_os("SUDO_UID").is_some() {
        return Err(invalid(
            "under sudo, specify lighting --config /absolute/path/lighting.json to select your user configuration",
        ));
    }
    if let Some(root) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        let root = PathBuf::from(root);
        if !root.is_absolute() {
            return Err(invalid("XDG_CONFIG_HOME must be absolute"));
        }
        return Ok(root.join("asusio/lighting.json"));
    }
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("HOME is not configured"))?;
    Ok(PathBuf::from(home).join(".config/asusio/lighting.json"))
}
impl Config {
    /// Parse an installed configuration and validate it against this host.
    pub fn from_json(bytes: &[u8]) -> io::Result<Self> {
        let config: Self =
            serde_json::from_slice(bytes).map_err(|error| invalid(error.to_string()))?;
        config.validate(&host()?, false)?;
        Ok(config)
    }

    pub fn validate(&self, expected_host: &str, allow_unbound: bool) -> io::Result<()> {
        if self.schema_version != 1 {
            return Err(invalid("unsupported lighting configuration schema"));
        }
        match &self.host_fingerprint {
            Some(value) if value == expected_host => {}
            None if allow_unbound => {}
            _ => return Err(invalid("lighting configuration belongs to another host")),
        }
        if self.targets.is_empty() {
            return Err(invalid("no lighting targets configured"));
        }
        for (alias, target) in &self.targets {
            if alias == super::ALL
                || alias.is_empty()
                || !alias
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
            {
                return Err(invalid(format!("invalid target alias: {alias}")));
            }
            match &target.device {
                Device::AuraArgb {
                    physical_path,
                    headers,
                } if !physical_path.is_empty()
                    && !headers.is_empty()
                    && headers
                        .iter()
                        .all(|index| *index < aura_hid::MAX_ARGB_HEADERS) => {}
                Device::AuraBoard {
                    physical_path,
                    locations,
                } if !physical_path.is_empty() && !locations.is_empty() => {}
                Device::ScopeKeyboard { physical_path } if !physical_path.is_empty() => {}
                Device::Ene {
                    adapter_name,
                    adapter_parent,
                    address,
                    firmware,
                    led_count,
                } if !adapter_name.is_empty()
                    && adapter_parent.is_absolute()
                    && ene_smbus::is_rgb_address(*address)
                    && !firmware.is_empty()
                    && (1..=ene_smbus::MAX_LED_COUNT).contains(led_count) => {}
                Device::Screen {
                    usb_id,
                    daemon_socket,
                } if !usb_id.is_empty() && daemon_socket.is_absolute() => {}
                _ => return Err(invalid(format!("invalid device configuration for {alias}"))),
            }
        }
        Ok(())
    }
}
pub(crate) fn load(path: &Path) -> io::Result<Config> {
    Config::from_json(&fs::read(path)?)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
}
pub(crate) fn save(path: &Path, config: &Config, replace: bool) -> io::Result<()> {
    crate::persist::write_json(path, config, replace)
}

/// Host-local configuration transactions shared by CLI and library clients.
/// Concurrent writers serialize on a stable sidecar lock, then reload before updating.
pub struct Store {
    path: PathBuf,
}
impl Store {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    /// Use `explicit`, or the default per-user location (see [`config_path`]).
    pub fn open(explicit: Option<PathBuf>) -> io::Result<Self> {
        Ok(Self::new(config_path(explicit)?))
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Load and validate against this host.
    pub fn load(&self) -> io::Result<Config> {
        load(&self.path)
    }
    /// Load and plan in one step; see [`super::plan`].
    pub fn plan(&self, request: &super::Request) -> io::Result<Vec<super::PlannedTarget>> {
        super::plan(&self.load()?, request)
    }
    pub fn install(&self, mut config: Config) -> io::Result<()> {
        let identity = host()?;
        config.validate(&identity, true)?;
        config.host_fingerprint = Some(identity);
        for target in config.targets.values() {
            super::runtime::resolve(&target.device)?;
        }
        let _lock = self.lock()?;
        save(&self.path, &config, false)
    }
    /// Change only this target's gains; reload under lock to preserve concurrent edits.
    pub fn calibrate(&self, alias: &str, gains: Rgb) -> io::Result<()> {
        let _lock = self.lock()?;
        let mut config = self.load()?;
        config
            .targets
            .get_mut(alias)
            .ok_or_else(|| invalid(format!("unknown target: {alias}")))?
            .white_balance = gains;
        config.validate(&host()?, false)?;
        save(&self.path, &config, true)
    }
    #[allow(unsafe_code)]
    fn lock(&self) -> io::Result<fs::File> {
        use std::os::fd::AsRawFd;
        if std::env::var_os("SUDO_UID").is_some() {
            return Err(invalid(
                "edit lighting configuration as your normal user, without sudo",
            ));
        }
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let mut name = self
            .path
            .file_name()
            .ok_or_else(|| invalid("configuration path needs a filename"))?
            .to_os_string();
        name.push(".lock");
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(parent.join(name))?;
        // SAFETY: flock takes a live descriptor and scalar flags. Closing the returned
        // File releases this lock, including on every early error path.
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        Ok(file)
    }
}
