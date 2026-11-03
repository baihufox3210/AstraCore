//! Native ENE register transport. Explicit adapters/addresses only; no address remapping.
use crate::util::invalid_data as invalid;
use asusio_types::Rgb;
use serde::Serialize;
use std::{fs, io, os::fd::AsRawFd, path::Path};

/// Largest LED count of any supported firmware profile.
pub const MAX_LED_COUNT: u8 = 64;

/// Known ENE RGB controller addresses. EEPROM/SPD addresses are never accepted.
pub fn is_rgb_address(address: u8) -> bool {
    matches!(address, 0x70..=0x77 | 0x4f | 0x66 | 0x67 | 0x39..=0x3d)
}

#[derive(Debug, Serialize)]
pub struct EneInfo {
    pub bus: u32,
    pub address: u8,
    pub version: String,
    pub led_count: u8,
    pub effect_register: u16,
}

fn profile(version: &str) -> io::Result<(u16, u16, u8)> {
    match version {
        "DIMM_LED-0102" | "DIMM_LED-0103" => Ok((0x8010, 0x1c02, 5)),
        "AUDA0-E6K5-0101" => Ok((0x8160, 0x1c02, 32)),
        "AUMA0-E6K5-0107" => Ok((0x8160, 0x1c03, MAX_LED_COUNT)),
        _ => Err(invalid(
            "unrecognized ENE firmware; no lighting writes permitted",
        )),
    }
}

#[repr(C)]
union SmbusData {
    byte: u8,
    word: u16,
    block: [u8; 34],
}
#[repr(C)]
struct SmbusRequest {
    read_write: u8,
    command: u8,
    size: u32,
    data: *mut SmbusData,
}
struct Bus(fs::File);

impl Bus {
    #[allow(unsafe_code)]
    fn open(bus: u32, address: u8) -> io::Result<Self> {
        // Never accept EEPROM/SPD addresses or arbitrary bus targets.
        if !is_rgb_address(address) {
            return Err(invalid("address is outside the known ENE RGB address list"));
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("/dev/i2c-{bus}"))?;
        // SAFETY: flock takes an open fd and integer flags; the lock lasts until file close.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut functions: libc::c_ulong = 0;
        // SAFETY: I2C_FUNCS fills an unsigned long at this valid writable pointer.
        if unsafe { libc::ioctl(file.as_raw_fd(), 0x0705, &mut functions) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SMBus read/write byte-data and write word-data are required by ENE.
        let required = 0x0008_0000 | 0x0010_0000 | 0x0040_0000;
        if functions & required != required {
            return Err(invalid("adapter lacks required SMBus operations"));
        }
        // SAFETY: I2C_SLAVE takes a scalar 7-bit address. FORCE is deliberately not used.
        if unsafe { libc::ioctl(file.as_raw_fd(), 0x0703, libc::c_ulong::from(address)) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(file))
    }

    #[allow(unsafe_code)]
    fn transfer(&self, read: bool, command: u8, size: u32, word: u16) -> io::Result<u8> {
        let mut data = SmbusData { block: [0; 34] };
        if size == 3 {
            data.word = word;
        } else {
            data.byte = word as u8;
        }
        let mut request = SmbusRequest {
            read_write: u8::from(read),
            command,
            size,
            data: &mut data,
        };
        // SAFETY: these repr(C) layouts match linux/i2c-dev.h and linux/i2c.h.
        // The initialized data union and request live through the synchronous ioctl.
        if unsafe { libc::ioctl(self.0.as_raw_fd(), 0x0720, &mut request) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: byte is initialized, including for a byte-data read returned by the kernel.
        Ok(unsafe { data.byte })
    }

    fn read_byte(&self, register: u8) -> io::Result<u8> {
        self.transfer(true, register, 2, 0)
    }
    fn select(&self, register: u16) -> io::Result<()> {
        self.transfer(false, 0, 3, register.swap_bytes())
            .map(|_| ())
    }
    fn read_register(&self, register: u16) -> io::Result<u8> {
        self.select(register)?;
        self.read_byte(0x81)
    }
    fn write_register(&self, register: u16, value: u8) -> io::Result<()> {
        self.select(register)?;
        self.transfer(false, 1, 2, u16::from(value)).map(|_| ())
    }

    fn identify(&self, bus: u32, address: u8) -> io::Result<EneInfo> {
        identify_with(
            bus,
            address,
            |register| self.read_byte(register),
            |register| self.read_register(register),
        )
    }
}

fn identify_with(
    bus: u32,
    address: u8,
    mut read_byte: impl FnMut(u8) -> io::Result<u8>,
    mut read_register: impl FnMut(u16) -> io::Result<u8>,
) -> io::Result<EneInfo> {
    // This signature test uses only SMBus byte-data reads, before ENE pointer writes.
    for index in 0..16 {
        if read_byte(0xa0 + index)? != index {
            return Err(invalid("ENE register signature mismatch"));
        }
    }
    let mut name = [0; 16];
    for (index, byte) in name.iter_mut().enumerate() {
        *byte = read_register(0x1000 + index as u16)?;
    }
    let end = name
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(name.len());
    let version = std::str::from_utf8(&name[..end])
        .map_err(|_| invalid("invalid ENE firmware name"))?
        .to_owned();
    let (effect_register, count_register, max_count) = profile(&version)?;
    let led_count = read_register(count_register)?;
    if led_count == 0 || led_count > max_count {
        return Err(invalid("ENE LED count outside supported firmware bounds"));
    }
    Ok(EneInfo {
        bus,
        address,
        version,
        led_count,
        effect_register,
    })
}

/// Requires explicit execution because register reads include ENE pointer-selection writes.
pub fn probe(bus: u32, address: u8) -> io::Result<EneInfo> {
    Bus::open(bus, address)?.identify(bus, address)
}

/// Register values describe requested intensities, not measured optical output.
#[derive(Debug, Serialize)]
pub struct EneColorState {
    pub device: EneInfo,
    pub direct: u8,
    pub mode: u8,
    pub color_register: u16,
    pub colors_rgb: Vec<[u8; 3]>,
}

fn read_colors(
    count: u8,
    base: u16,
    mut read: impl FnMut(u16) -> io::Result<u8>,
) -> io::Result<Vec<[u8; 3]>> {
    (0..u16::from(count))
        .map(|index| {
            let offset = base + index * 3;
            let red = read(offset)?;
            let blue = read(offset + 1)?;
            let green = read(offset + 2)?;
            Ok([red, green, blue])
        })
        .collect()
}

/// Reads known state registers only; ENE pointer selection writes are needed for reads.
pub fn read_state(bus: u32, address: u8) -> io::Result<EneColorState> {
    let adapter = Bus::open(bus, address)?;
    let device = adapter.identify(bus, address)?;
    let direct = adapter.read_register(0x8020)?;
    let mode = adapter.read_register(0x8021)?;
    let color_register = if direct != 0 {
        if device.effect_register == 0x8160 {
            0x8100
        } else {
            0x8000
        }
    } else {
        device.effect_register
    };
    let colors_rgb = read_colors(device.led_count, color_register, |register| {
        adapter.read_register(register)
    })?;
    Ok(EneColorState {
        device,
        direct,
        mode,
        color_register,
        colors_rgb,
    })
}

fn static_plan(info: &EneInfo, rgb: Rgb) -> Vec<(u16, u8)> {
    let mut writes = Vec::new();
    for index in 0..u16::from(info.led_count) {
        for (component, value) in [rgb[0], rgb[2], rgb[1]].into_iter().enumerate() {
            writes.push((info.effect_register + index * 3 + component as u16, value));
        }
    }
    writes.extend([(0x8020, 0), (0x8021, 1), (0x80a0, 1)]);
    writes
}

/// Firmware identity a caller expects; checked on the locked descriptor before color writes.
#[derive(Debug, Clone, Copy)]
pub struct ExpectedIdentity<'a> {
    pub firmware: &'a str,
    pub led_count: u8,
}

/// Re-identify on the same locked fd before setting RGB; no Save (AA), slot or address writes.
/// With `expected`, a firmware or LED-count mismatch aborts before any color write.
pub fn write_static(
    bus: u32,
    address: u8,
    rgb: Rgb,
    expected: Option<ExpectedIdentity>,
) -> io::Result<EneInfo> {
    let adapter = Bus::open(bus, address)?;
    let info = adapter.identify(bus, address)?;
    if let Some(expected) = expected
        && (info.version != expected.firmware || info.led_count != expected.led_count)
    {
        return Err(invalid(
            "ENE identity differs from the configured target; no colors written",
        ));
    }
    for (register, value) in static_plan(&info, rgb) {
        adapter.write_register(register, value)?;
    }
    Ok(info)
}

#[derive(Debug, Serialize)]
pub struct AdapterInfo {
    pub bus: u32,
    pub name: String,
    pub sysfs_path: std::path::PathBuf,
}

/// Metadata only: no /dev/i2c access and no device probing.
/// `root` is normally [`asusio_discovery::sysfs::I2C`].
pub fn adapters(root: &Path) -> io::Result<Vec<AdapterInfo>> {
    let mut result = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(bus) = name
            .to_str()
            .and_then(|name| name.strip_prefix("i2c-"))
            .and_then(|name| name.parse().ok())
        else {
            continue;
        };
        result.push(AdapterInfo {
            bus,
            name: fs::read_to_string(entry.path().join("name"))?
                .trim()
                .to_owned(),
            sysfs_path: fs::canonicalize(entry.path())?,
        });
    }
    result.sort_by_key(|adapter| adapter.bus);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn color_readback_decodes_rbg_and_stops_on_read_failure() {
        let bytes = [0x12, 0x56, 0x34, 0xab, 0xef, 0xcd];
        assert_eq!(
            read_colors(2, 0x8160, |register| Ok(
                bytes[usize::from(register - 0x8160)]
            ))
            .unwrap(),
            vec![[0x12, 0x34, 0x56], [0xab, 0xcd, 0xef]]
        );
        let mut reads = 0;
        assert!(
            read_colors(2, 0x8160, |_| {
                reads += 1;
                Err(invalid("simulated read failure"))
            })
            .is_err()
        );
        assert_eq!(reads, 1);
    }
    #[test]
    fn signature_mismatch_never_selects_an_ene_register() {
        let result = identify_with(
            7,
            0x70,
            |_| Ok(0xff),
            |_| panic!("must not write a register pointer before signature validation"),
        );
        assert!(result.is_err());
    }
    #[test]
    fn identifies_firmware_and_rejects_invalid_led_count() {
        for count in [0, 8, 255] {
            let name = b"AUDA0-E6K5-0101\0";
            let result = identify_with(
                7,
                0x70,
                |register| Ok(register - 0xa0),
                |register| {
                    if register == 0x1c02 {
                        Ok(count)
                    } else {
                        Ok(name[usize::from(register - 0x1000)])
                    }
                },
            );
            if count == 8 {
                let info = result.unwrap();
                assert_eq!(info.led_count, 8);
                assert_eq!(info.effect_register, 0x8160);
            } else {
                assert!(result.is_err());
            }
        }
    }
    #[test]
    fn rejects_unknown_firmware_and_selects_correct_generation() {
        assert!(profile("UNKNOWN").is_err());
        assert_eq!(profile("AUMA0-E6K5-0107").unwrap(), (0x8160, 0x1c03, 64));
        assert!(!is_rgb_address(0x51) && is_rgb_address(0x67));
        assert_eq!(profile("DIMM_LED-0102").unwrap(), (0x8010, 0x1c02, 5));
    }
    #[test]
    fn static_plan_uses_rbg_and_never_saves_or_remaps_addresses() {
        let info = EneInfo {
            bus: 7,
            address: 0x70,
            version: "AUDA0-E6K5-0101".into(),
            led_count: 2,
            effect_register: 0x8160,
        };
        assert_eq!(
            static_plan(&info, [0x12, 0x34, 0x56]),
            vec![
                (0x8160, 0x12),
                (0x8161, 0x56),
                (0x8162, 0x34),
                (0x8163, 0x12),
                (0x8164, 0x56),
                (0x8165, 0x34),
                (0x8020, 0),
                (0x8021, 1),
                (0x80a0, 1)
            ]
        );
    }
    #[test]
    fn rejects_spd_before_opening_adapter() {
        assert!(Bus::open(u32::MAX, 0x51).is_err());
    }
}
