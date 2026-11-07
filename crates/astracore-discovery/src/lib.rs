pub mod hid;
pub mod hid_descriptor;
pub mod hwmon;
pub mod pci;
pub mod usb;

use astracore_types::BoardInfo;
use std::{fs, io, path::Path};

/// Default Linux sysfs roots. Discovery functions accept any root so tests can use fixtures.
pub mod sysfs {
    pub const DMI: &str = "/sys/class/dmi/id";
    pub const HWMON: &str = "/sys/class/hwmon";
    pub const PCI: &str = "/sys/bus/pci/devices";
    pub const USB: &str = "/sys/bus/usb/devices";
    pub const HID: &str = "/sys/bus/hid/devices";
    pub const I2C: &str = "/sys/bus/i2c/devices";
    pub const CPU: &str = "/sys/devices/system/cpu";
    pub const POWERCAP: &str = "/sys/class/powercap";
    pub const SCHED_EXT: &str = "/sys/kernel/sched_ext";
}

fn read_field(path: &Path) -> io::Result<String> {
    Ok(fs::read_to_string(path)?.trim().to_owned())
}

pub fn discover_board() -> io::Result<BoardInfo> {
    let root = Path::new(sysfs::DMI);

    let vendor = read_field(&root.join("board_vendor"))?;
    let model = read_field(&root.join("board_name"))?;

    let bios_version = read_field(&root.join("bios_version"))
        .ok()
        .filter(|value| !value.is_empty());

    Ok(BoardInfo {
        vendor,
        model,
        bios_version,
    })
}
