//! Descriptive target information for front ends: device kind, supported effects and
//! readiness. Only sysfs metadata and socket existence are checked; nothing is opened.
use super::{config::Device, runtime};
use asusio_backends::ene_smbus;
use serde::{Deserialize, Serialize};
use std::{fmt, io, os::unix::fs::FileTypeExt, path::Path, str::FromStr};

/// A lighting effect. Only controller built-in hardware modes are planned; each new
/// effect stays experimental until its color change is confirmed on the hardware.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// One solid color; verified on every configured backend.
    #[default]
    Static,
}

impl Effect {
    pub const ALL: [Self; 1] = [Self::Static];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
        }
    }

    /// Whether the effect still awaits physical confirmation.
    pub fn experimental(self) -> bool {
        match self {
            Self::Static => false,
        }
    }
}

impl fmt::Display for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

impl FromStr for Effect {
    type Err = io::Error;
    fn from_str(value: &str) -> io::Result<Self> {
        Self::ALL
            .into_iter()
            .find(|effect| effect.as_str() == value)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown effect {value:?}; supported: static"),
                )
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectInfo {
    pub effect: Effect,
    pub experimental: bool,
}

/// Device category, for grouping in a user interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    /// Onboard mainboard LEDs.
    Board,
    /// Addressable headers (e.g. fan LEDs).
    Argb,
    Keyboard,
    Graphics,
    Memory,
    Screen,
    /// ENE firmware without a known class.
    Other,
}

impl TargetKind {
    pub fn of(device: &Device) -> Self {
        match device {
            Device::AuraBoard { .. } => Self::Board,
            Device::AuraArgb { .. } => Self::Argb,
            Device::ScopeKeyboard { .. } => Self::Keyboard,
            Device::Ene { firmware, .. } => match ene_smbus::class(firmware) {
                Some(ene_smbus::Class::Graphics) => Self::Graphics,
                Some(ene_smbus::Class::Memory) => Self::Memory,
                None => Self::Other,
            },
            Device::Screen { .. } => Self::Screen,
        }
    }
}

/// Configuration variant name, as serialized in `lighting.json`.
pub fn backend(device: &Device) -> &'static str {
    match device {
        Device::AuraArgb { .. } => "aura_argb",
        Device::AuraBoard { .. } => "aura_board",
        Device::ScopeKeyboard { .. } => "scope_keyboard",
        Device::Ene { .. } => "ene",
        Device::Screen { .. } => "screen",
    }
}

/// Effects the device's backend implements.
pub fn effects(_device: &Device) -> Vec<EffectInfo> {
    Effect::ALL
        .into_iter()
        .map(|effect| EffectInfo {
            effect,
            experimental: effect.experimental(),
        })
        .collect()
}

pub fn supports(device: &Device, effect: Effect) -> bool {
    effects(device).iter().any(|info| info.effect == effect)
}

/// Whether a target can be written now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Availability {
    Available,
    /// Expected to become available, e.g. the screen before lianli-daemon starts.
    NotReady {
        reason: String,
    },
    Unavailable {
        reason: String,
    },
}

fn is_socket(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
}

/// Resolve the device binding (sysfs only) and, for the screen, check that the
/// lianli-daemon socket exists.
pub fn availability(device: &Device) -> Availability {
    if let Device::Screen { daemon_socket, .. } = device
        && !is_socket(daemon_socket)
    {
        return Availability::NotReady {
            reason: format!(
                "waiting for lianli-daemon ({} is not present; it starts after login)",
                daemon_socket.display()
            ),
        };
    }
    match runtime::resolve(device) {
        Ok(_) => Availability::Available,
        Err(error) => Availability::Unavailable {
            reason: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ene(firmware: &str) -> Device {
        Device::Ene {
            adapter_name: "adapter".into(),
            adapter_parent: "/sys/devices/x".into(),
            address: 0x67,
            firmware: firmware.into(),
            led_count: 8,
        }
    }

    #[test]
    fn kinds_follow_backend_and_ene_firmware() {
        assert_eq!(
            TargetKind::of(&ene("AUMA0-E6K5-0107")),
            TargetKind::Graphics
        );
        assert_eq!(TargetKind::of(&ene("AUDA0-E6K5-0101")), TargetKind::Memory);
        assert_eq!(TargetKind::of(&ene("NEW-FIRMWARE")), TargetKind::Other);
        let keyboard = Device::ScopeKeyboard {
            physical_path: "usb-1".into(),
        };
        assert_eq!(TargetKind::of(&keyboard), TargetKind::Keyboard);
        assert_eq!(backend(&keyboard), "scope_keyboard");
        assert_eq!(serde_json::to_value(TargetKind::Argb).unwrap(), "argb");
    }

    #[test]
    fn static_is_the_only_and_verified_effect() {
        let infos = effects(&ene("AUMA0-E6K5-0107"));
        assert_eq!(
            infos,
            [EffectInfo {
                effect: Effect::Static,
                experimental: false
            }]
        );
        assert_eq!("static".parse::<Effect>().unwrap(), Effect::Static);
        assert!("rainbow".parse::<Effect>().is_err());
        assert_eq!(Effect::default(), Effect::Static);
    }

    #[test]
    fn screen_without_daemon_socket_is_not_ready() {
        let screen = Device::Screen {
            usb_id: "6-9.3".into(),
            daemon_socket: "/nonexistent/lianli-daemon.sock".into(),
        };
        let availability = availability(&screen);
        assert!(
            matches!(&availability, Availability::NotReady { reason } if reason.contains("lianli-daemon")),
            "{availability:?}"
        );
        let wire = serde_json::to_value(&availability).unwrap();
        assert_eq!(wire["state"], "not_ready");
    }
}
