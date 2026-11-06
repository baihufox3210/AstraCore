//! Native device discovery, resolution and dispatch. Add backend-specific handling here.
use super::{
    config::{Device, invalid},
    info::Effect,
};
use asusio_backends::{
    aura_hid, ene_smbus, ene_smbus::ExpectedIdentity, lianli_ipc, lianli_screen, scope_keyboard,
};
use asusio_discovery::{hid, sysfs, usb};
use asusio_types::{DiscoveryReport, HidDevice, Rgb, UsbId, merge_warnings};
use serde::Serialize;
use std::{io, path::Path};

/// Native lighting interface families that asusio can drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceKind {
    /// ASUS Aura mainboard controller (ARGB headers and onboard locations).
    AuraHid,
    /// ASUS Scope II RX keyboard vendor interface.
    ScopeKeyboard,
    /// Lian Li Universal Screen LED USB device.
    UniversalScreen,
}

impl InterfaceKind {
    fn usb_id(self) -> UsbId {
        match self {
            Self::AuraHid => aura_hid::DEVICE,
            Self::ScopeKeyboard => scope_keyboard::DEVICE,
            Self::UniversalScreen => lianli_screen::DEVICE,
        }
    }

    /// Descriptor validation for HID kinds (sysfs only, no hidraw access).
    fn inspect_hid(self, device: &HidDevice) -> io::Result<()> {
        let root = Path::new(sysfs::HID);
        match self {
            Self::AuraHid => aura_hid::inspect(device, root).map(|_| ()),
            Self::ScopeKeyboard => scope_keyboard::inspect(device, root),
            Self::UniversalScreen => Err(invalid("not a HID interface")),
        }
    }
}

/// A validated native interface. `device_id` is the HID or USB sysfs ID for diagnostics.
#[derive(Debug, Clone, Serialize)]
pub struct NativeInterface {
    pub device_id: String,
    pub name: Option<String>,
    pub kind: InterfaceKind,
}

const HID_KINDS: [InterfaceKind; 2] = [InterfaceKind::AuraHid, InterfaceKind::ScopeKeyboard];

/// List native lighting interfaces from USB/HID metadata and descriptors.
/// Does not open hidraw or USB nodes and does not test write permission.
pub fn discover_interfaces() -> io::Result<DiscoveryReport<NativeInterface>> {
    let hid = hid::discover(sysfs::HID)?;
    let usb = usb::discover(sysfs::USB)?;
    let mut devices = Vec::new();
    for device in &hid.devices {
        if let Some(kind) = HID_KINDS
            .into_iter()
            .find(|kind| device.is(kind.usb_id()) && kind.inspect_hid(device).is_ok())
        {
            devices.push(NativeInterface {
                device_id: device.device_id.clone(),
                name: Some(device.name.clone()),
                kind,
            });
        }
    }
    for device in usb
        .devices
        .iter()
        .filter(|device| device.is(lianli_screen::DEVICE))
    {
        lianli_screen::inspect(&device.sysfs_id, Path::new(sysfs::USB))?;
        devices.push(NativeInterface {
            device_id: device.sysfs_id.clone(),
            name: device.product.clone(),
            kind: InterfaceKind::UniversalScreen,
        });
    }
    Ok(DiscoveryReport {
        devices,
        warnings: merge_warnings([hid.warnings, usb.warnings]),
    })
}

/// Exactly one validated HID interface of `kind` at a stable physical path.
fn find_hid(physical_path: &str, kind: InterfaceKind) -> io::Result<HidDevice> {
    let mut matches = Vec::new();
    for device in hid::discover(sysfs::HID)?.devices {
        if !device.is(kind.usb_id())
            || hid::physical_path(sysfs::HID, &device.device_id)? != physical_path
        {
            continue;
        }
        kind.inspect_hid(&device)?;
        matches.push(device);
    }
    if matches.len() != 1 {
        return Err(invalid(format!(
            "expected one HID interface at {physical_path}, found {}",
            matches.len()
        )));
    }
    Ok(matches.remove(0))
}

pub enum Endpoint {
    Hid(HidDevice),
    Ene(u32),
    Screen,
}

pub fn resolve(device: &Device) -> io::Result<Endpoint> {
    match device {
        Device::AuraArgb { physical_path, .. } | Device::AuraBoard { physical_path, .. } => Ok(
            Endpoint::Hid(find_hid(physical_path, InterfaceKind::AuraHid)?),
        ),
        Device::ScopeKeyboard { physical_path } => Ok(Endpoint::Hid(find_hid(
            physical_path,
            InterfaceKind::ScopeKeyboard,
        )?)),
        Device::Ene {
            adapter_name,
            adapter_parent,
            ..
        } => {
            let adapters = ene_smbus::adapters(Path::new(sysfs::I2C))?;
            let matches: Vec<_> = adapters
                .iter()
                .filter(|adapter| {
                    adapter.name == *adapter_name
                        && adapter.sysfs_path.parent() == Some(adapter_parent.as_path())
                })
                .collect();
            if matches.len() != 1 {
                return Err(invalid(format!(
                    "expected one I2C adapter matching {adapter_name}, found {}",
                    matches.len()
                )));
            }
            Ok(Endpoint::Ene(matches[0].bus))
        }
        Device::Screen { usb_id, .. } => {
            lianli_screen::inspect(usb_id, Path::new(sysfs::USB))?;
            Ok(Endpoint::Screen)
        }
    }
}

/// Write `rgb` with `effect`. Planning has already checked that the backend supports the
/// effect; backends without effects only ever receive Static.
pub fn apply(device: &Device, endpoint: &Endpoint, rgb: Rgb, effect: Effect) -> io::Result<()> {
    let root = Path::new(sysfs::HID);
    match (device, endpoint) {
        (Device::AuraArgb { headers, .. }, Endpoint::Hid(hid)) => {
            aura_hid::write_argb_effect(hid, root, headers, effect.aura_id(), rgb)
        }
        (Device::AuraBoard { locations, .. }, Endpoint::Hid(hid)) => {
            aura_hid::write_onboard_effect(hid, root, locations, effect.aura_id(), rgb)
        }
        (Device::ScopeKeyboard { .. }, Endpoint::Hid(hid)) => {
            scope_keyboard::write_static(hid, root, rgb)
        }
        (
            Device::Ene {
                address,
                firmware,
                led_count,
                ..
            },
            Endpoint::Ene(bus),
        ) => {
            let expected = ExpectedIdentity {
                firmware,
                led_count: *led_count,
            };
            ene_smbus::write_static(*bus, *address, rgb, Some(expected)).map(|_| ())
        }
        (
            Device::Screen {
                usb_id,
                daemon_socket,
            },
            Endpoint::Screen,
        ) => lianli_ipc::set_static(daemon_socket, usb_id, rgb),
        _ => Err(invalid("resolved backend mismatch")),
    }
}
