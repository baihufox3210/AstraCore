pub mod hid;
pub mod hwmon;
pub mod pci;
pub mod usb;

use asusio_types::BoardInfo;
use std::{fs, io, path::Path};

fn read_field(path: &Path) -> io::Result<String> {
    Ok(fs::read_to_string(path)?.trim().to_owned())
}

pub fn discover_board() -> io::Result<BoardInfo> {
    let root = Path::new("/sys/class/dmi/id");

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
