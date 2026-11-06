//! Descriptive target information for front ends: device kind, supported effects and
//! readiness. Only sysfs metadata and socket existence are checked; nothing is opened.
use super::{config::Device, runtime};
use asusio_backends::ene_smbus;
use serde::{Deserialize, Serialize};
use std::{fmt, io, os::unix::fs::FileTypeExt, path::Path, str::FromStr};

/// A lighting effect: a controller built-in hardware mode. The names and Aura IDs come from
/// the effect table of ASUS's own HAL (`AacHal_x64_MbMgr.dll`, see Research.md); each
/// effect stays experimental on a backend until it is confirmed on the hardware.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// One solid color; verified on every configured backend.
    #[default]
    Static,
    Breathing,
    Strobing,
    ColorCycle,
    Rainbow,
    BreathRainbow,
    RainbowComet,
    Comet,
    MovingFlash,
    FlashAndDash,
    Wave,
    GlowingYoyo,
    StarryNight,
}

impl Effect {
    pub const ALL: [Self; 13] = [
        Self::Static,
        Self::Breathing,
        Self::Strobing,
        Self::ColorCycle,
        Self::Rainbow,
        Self::BreathRainbow,
        Self::RainbowComet,
        Self::Comet,
        Self::MovingFlash,
        Self::FlashAndDash,
        Self::Wave,
        Self::GlowingYoyo,
        Self::StarryNight,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Breathing => "breathing",
            Self::Strobing => "strobing",
            Self::ColorCycle => "color_cycle",
            Self::Rainbow => "rainbow",
            Self::BreathRainbow => "breath_rainbow",
            Self::RainbowComet => "rainbow_comet",
            Self::Comet => "comet",
            Self::MovingFlash => "moving_flash",
            Self::FlashAndDash => "flash_and_dash",
            Self::Wave => "wave",
            Self::GlowingYoyo => "glowing_yoyo",
            Self::StarryNight => "starry_night",
        }
    }

    /// The ENE mode (register `0x8021`) for this effect, if ENE controllers have it.
    pub fn ene_mode(self) -> Option<u8> {
        match self {
            Self::Static => Some(1),
            Self::Breathing => Some(2),
            Self::Strobing => Some(3),
            Self::ColorCycle => Some(4),
            Self::Rainbow => Some(5),
            Self::BreathRainbow => Some(6),
            Self::StarryNight => Some(13),
            _ => None,
        }
    }

    /// The lianli-daemon mode that renders this effect on the screen, if any.
    pub fn screen_mode(self) -> Option<&'static str> {
        match self {
            Self::Static => Some("Static"),
            Self::Breathing => Some("Breathing"),
            Self::ColorCycle => Some("RainbowMorph"),
            Self::Rainbow => Some("Rainbow"),
            Self::StarryNight => Some("Twinkle"),
            _ => None,
        }
    }

    /// The effect ID of the Aura HID `41 05` report (byte 8), from the HAL's effect table.
    pub fn aura_id(self) -> u8 {
        match self {
            Self::Static => 1,
            Self::Breathing => 2,
            Self::Strobing => 3,
            Self::ColorCycle => 4,
            Self::Rainbow => 5,
            Self::BreathRainbow => 6,
            Self::RainbowComet => 7,
            Self::Comet => 8,
            Self::MovingFlash => 9,
            Self::FlashAndDash => 10,
            Self::Wave => 11,
            Self::GlowingYoyo => 12,
            Self::StarryNight => 13,
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
                    format!(
                        "unknown effect {value:?}; known: {}",
                        Self::ALL.map(Self::as_str).join(", ")
                    ),
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

/// Effects the device's backend implements, and whether each still awaits confirmation on
/// the hardware.
///
/// Aura controllers take the HAL's effects through the same `41 05` report as Static; on the
/// B850-A these seven were confirmed (2026-11-06). Rainbow_Comet, Comet, Moving_Flash,
/// Flash_and_Dash, Wave and Glowing_Yoyo showed no animation with that report alone (they
/// likely need the `41 23` / `41 28` reports, not implemented), so they are not offered.
/// ENE GPU/DRAM controllers take the same seven as modes of register `0x8021`; the screen
/// gets five from lianli-daemon's software modes. The keyboard only has Static.
pub fn effects(device: &Device) -> Vec<EffectInfo> {
    const SEVEN: [Effect; 7] = [
        Effect::Static,
        Effect::Breathing,
        Effect::Strobing,
        Effect::ColorCycle,
        Effect::Rainbow,
        Effect::BreathRainbow,
        Effect::StarryNight,
    ];
    let (list, confirmed): (Vec<Effect>, &[Effect]) = match device {
        Device::AuraArgb { .. } | Device::AuraBoard { .. } => (SEVEN.to_vec(), &SEVEN),
        Device::Ene { .. } => (SEVEN.to_vec(), &[Effect::Static]),
        Device::Screen { .. } => (
            Effect::ALL
                .into_iter()
                .filter(|effect| effect.screen_mode().is_some())
                .collect(),
            &[Effect::Static],
        ),
        Device::ScopeKeyboard { .. } => (vec![Effect::Static], &[Effect::Static]),
    };
    list.into_iter()
        .map(|effect| EffectInfo {
            effect,
            experimental: !confirmed.contains(&effect),
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

    #[test]
    fn effects_follow_the_hal_table() {
        for (index, effect) in Effect::ALL.into_iter().enumerate() {
            assert_eq!(
                usize::from(effect.aura_id()),
                index + 1,
                "IDs 1-13 in order"
            );
            assert_eq!(effect.as_str().parse::<Effect>().unwrap(), effect);
            let json = serde_json::to_string(&effect).unwrap();
            assert_eq!(json, format!("\"{}\"", effect.as_str()));
        }
        assert!("sparkle".parse::<Effect>().is_err());

        let argb = Device::AuraArgb {
            physical_path: "usb-x/input1".into(),
            headers: vec![0],
        };
        let listed = effects(&argb);
        assert_eq!(listed.len(), 7);
        assert!(
            listed.iter().all(|info| !info.experimental),
            "all confirmed"
        );
        assert!(supports(&argb, Effect::Rainbow) && supports(&argb, Effect::StarryNight));
        assert!(
            !supports(&argb, Effect::Wave),
            "no animation with 41 05 alone"
        );
        assert!(!supports(&argb, Effect::Comet));

        let gpu = ene("AUMA0-E6K5-0107");
        assert_eq!(effects(&gpu).len(), 7);
        assert!(supports(&gpu, Effect::Rainbow) && !supports(&gpu, Effect::Wave));
        for info in effects(&gpu) {
            assert_eq!(info.experimental, info.effect != Effect::Static);
            assert!(info.effect.ene_mode().is_some());
        }
        let screen = Device::Screen {
            usb_id: "6-9.3".into(),
            daemon_socket: "/run/user/1000/lianli-daemon.sock".into(),
        };
        let offered: Vec<Effect> = effects(&screen).iter().map(|i| i.effect).collect();
        assert_eq!(
            offered,
            [
                Effect::Static,
                Effect::Breathing,
                Effect::ColorCycle,
                Effect::Rainbow,
                Effect::StarryNight
            ]
        );
        assert!(!supports(&screen, Effect::Strobing));
    }

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
    fn static_is_the_default_effect() {
        assert_eq!("static".parse::<Effect>().unwrap(), Effect::Static);
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
