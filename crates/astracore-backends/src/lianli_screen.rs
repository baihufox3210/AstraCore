//! Native USB bulk control for the 0416:8050 Universal Screen LED interface.
//! The LCD at 1cbe:a088 is a separate device and is never accessed here.

use crate::util::{invalid_data, invalid_input};
use astracore_types::{Rgb, UsbId};
use std::{
    fs, io,
    io::Read,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
};

pub const DEVICE: UsbId = UsbId::new(0x0416, 0x8050);
pub const LED_COUNT: usize = 60;

pub fn static_packets(rgb: Rgb) -> [[u8; 64]; 3] {
    std::array::from_fn(|index| {
        let mut packet = [0; 64];
        packet[0] = 0x11;
        packet[1] = (index * 20) as u8;
        for color in packet[4..].as_chunks_mut::<3>().0 {
            color.copy_from_slice(&rgb);
        }
        packet
    })
}

fn read(root: &Path, name: &str) -> io::Result<String> {
    Ok(fs::read_to_string(root.join(name))?.trim().to_owned())
}

/// Resolve an explicit USB sysfs ID to its `/dev/bus/usb` node; never choose the first match.
/// `sysfs_root` is normally [`astracore_discovery::sysfs::USB`]. This does not open the node.
pub fn inspect(device_id: &str, sysfs_root: &Path) -> io::Result<PathBuf> {
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    if !device_id
        .split_once('-')
        .is_some_and(|(bus, ports)| digits(bus) && ports.split('.').all(digits))
    {
        return Err(invalid_input("expected USB sysfs ID, for example 6-9.3"));
    }
    let device = sysfs_root.join(device_id);
    if !DEVICE.matches(&read(&device, "idVendor")?, &read(&device, "idProduct")?) {
        return Err(invalid_input(
            "expected Universal Screen LED device 0416:8050",
        ));
    }
    let interface = device.join(format!("{device_id}:1.0"));
    let endpoint = interface.join("ep_01");
    if read(&interface, "bInterfaceNumber")? != "00"
        || read(&interface, "bAlternateSetting")? != "0"
        || read(&endpoint, "bEndpointAddress")? != "01"
        || read(&endpoint, "bmAttributes")? != "02"
        || read(&endpoint, "wMaxPacketSize")? != "0040"
    {
        return Err(invalid_data(
            "unexpected USB interface or bulk OUT endpoint layout",
        ));
    }
    let number = |name| -> io::Result<u16> {
        read(&device, name)?
            .parse()
            .map_err(|_| invalid_data("invalid USB bus/device number"))
    };
    Ok(PathBuf::from(format!(
        "/dev/bus/usb/{:03}/{:03}",
        number("busnum")?,
        number("devnum")?
    )))
}

#[repr(C)]
struct BulkTransfer {
    endpoint: libc::c_uint,
    length: libc::c_uint,
    timeout_ms: libc::c_uint,
    data: *mut libc::c_void,
}

struct ClaimedInterface<'a>(&'a fs::File);
impl Drop for ClaimedInterface<'_> {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        let interface: libc::c_uint = 0;
        // SAFETY: the file outlives this guard and the pointer is valid for the ioctl.
        unsafe {
            libc::ioctl(
                self.0.as_raw_fd(),
                libc::_IOR::<libc::c_uint>(b'U'.into(), 16),
                &interface,
            );
        }
    }
}

fn operation_error(operation: &str, path: &Path, error: io::Error) -> io::Error {
    let hint = if error.raw_os_error() == Some(libc::EBUSY) {
        "; USB interface is already in use. Inspect with fuser -v on this node; a running lianli-daemon may own it. No driver was detached or service stopped"
    } else {
        ""
    };
    io::Error::new(
        error.kind(),
        format!("{operation} on {}: {error}{hint}", path.display()),
    )
}

/// Claim interface 0 temporarily; never detach a driver, reset USB, or change permissions.
#[allow(unsafe_code)]
pub fn write_static(device_id: &str, sysfs_root: &Path, rgb: Rgb) -> io::Result<()> {
    let path = inspect(device_id, sysfs_root)?;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| operation_error("open USB node", &path, error))?;
    // Recheck the opened USB node's VID/PID to reject a reused bus address.
    let mut descriptor = [0; 18];
    file.read_exact(&mut descriptor)?;
    let vendor = u16::from_le_bytes([descriptor[8], descriptor[9]]);
    let product = u16::from_le_bytes([descriptor[10], descriptor[11]]);
    if descriptor[0..2] != [18, 1] || UsbId::new(vendor, product) != DEVICE {
        return Err(invalid_data("opened USB node no longer matches 0416:8050"));
    }
    let interface: libc::c_uint = 0;
    // SAFETY: ioctl receives the Linux UAPI's unsigned interface index, valid for this call.
    let claimed = unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            libc::_IOR::<libc::c_uint>(b'U'.into(), 15),
            &interface,
        )
    };
    if claimed < 0 {
        return Err(operation_error(
            "claim interface 0",
            &path,
            io::Error::last_os_error(),
        ));
    }
    let _guard = ClaimedInterface(&file);
    for (index, mut packet) in static_packets(rgb).into_iter().enumerate() {
        let mut transfer = BulkTransfer {
            endpoint: 1,
            length: 64,
            timeout_ms: 1000,
            data: packet.as_mut_ptr().cast(),
        };
        // SAFETY: repr(C) matches usbdevfs_bulktransfer; packet and transfer remain alive
        // through this synchronous ioctl. The kernel cannot access beyond length=64.
        let result = unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                libc::_IOWR::<BulkTransfer>(b'U'.into(), 2),
                &mut transfer,
            )
        };
        if result < 0 {
            return Err(operation_error(
                &format!("bulk OUT packet {}", index + 1),
                &path,
                io::Error::last_os_error(),
            ));
        }
        if result != 64 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "short LED bulk transfer; no retry was sent",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frames_all_sixty_leds_in_three_rgb_packets() {
        let packets = static_packets([0x12, 0x34, 0x56]);
        for (index, packet) in packets.iter().enumerate() {
            assert_eq!(&packet[..4], &[0x11, (index * 20) as u8, 0, 0]);
            assert!(
                packet[4..]
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .all(|color| *color == [0x12, 0x34, 0x56])
            );
        }
        assert_eq!(packets.len() * 20, LED_COUNT);
    }
    #[test]
    fn rejects_paths_before_accessing_usb() {
        assert!(inspect("../6-9.3", Path::new("/does-not-exist")).is_err());
        assert!(inspect("..", Path::new("/does-not-exist")).is_err());
    }
}
