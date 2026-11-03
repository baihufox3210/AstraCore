//! Fan controls exposed by the verified nct6799 hwmon driver.
//! Fan tachometer numbers do not establish a PWM-to-header mapping.

use asusio_types::{DiscoveryReport, DiscoveryWarning};
use serde::Serialize;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

const CONTROLLER: &str = "nct6799";

#[derive(Debug, Clone, Serialize)]
pub struct FanSpeed {
    pub channel: u8,
    pub rpm: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PwmChannel {
    pub channel: u8,
    pub duty: u8,
    pub enable: u8,
    pub mode: Option<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FanController {
    pub name: String,
    pub sysfs_path: PathBuf,
    pub fan_speeds: Vec<FanSpeed>,
    pub pwm_channels: Vec<PwmChannel>,
}

fn read_number<T: std::str::FromStr>(path: &Path) -> io::Result<T> {
    fs::read_to_string(path)?.trim().parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid number: {}", path.display()),
        )
    })
}

fn channel_number(filename: &str, prefix: &str, suffix: &str) -> Option<u8> {
    filename
        .strip_prefix(prefix)?
        .strip_suffix(suffix)?
        .parse::<u8>()
        .ok()
        .filter(|number| *number > 0)
}

/// Scan `root` (normally [`asusio_discovery::sysfs::HWMON`]) for the supported controller.
pub fn discover_fan_controllers(root: &Path) -> io::Result<DiscoveryReport<FanController>> {
    let mut report = DiscoveryReport::default();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_name().to_string_lossy().starts_with("hwmon") {
            continue;
        }
        let Ok(name) = fs::read_to_string(path.join("name")) else {
            continue;
        };
        if name.trim() != CONTROLLER {
            continue;
        }

        let mut controller = FanController {
            name: CONTROLLER.to_owned(),
            sysfs_path: path.clone(),
            fan_speeds: Vec::new(),
            pwm_channels: Vec::new(),
        };
        for attr in fs::read_dir(&path)? {
            let attr = attr?;
            let filename = attr.file_name().to_string_lossy().into_owned();
            if let Some(channel) = channel_number(&filename, "fan", "_input") {
                match read_number::<u64>(&attr.path()) {
                    Ok(rpm) => controller.fan_speeds.push(FanSpeed { channel, rpm }),
                    Err(error) => report.warnings.push(DiscoveryWarning {
                        path: attr.path(),
                        reason: error.to_string(),
                    }),
                }
            } else if let Some(channel) = channel_number(&filename, "pwm", "") {
                let enable_path = path.join(format!("pwm{channel}_enable"));
                match (
                    read_number::<u8>(&attr.path()),
                    read_number::<u8>(&enable_path),
                ) {
                    (Ok(duty), Ok(enable)) => controller.pwm_channels.push(PwmChannel {
                        channel,
                        duty,
                        enable,
                        mode: read_number(&path.join(format!("pwm{channel}_mode"))).ok(),
                    }),
                    (Err(error), _) | (_, Err(error)) => report.warnings.push(DiscoveryWarning {
                        path: attr.path(),
                        reason: error.to_string(),
                    }),
                }
            }
        }
        controller.fan_speeds.sort_by_key(|fan| fan.channel);
        controller.pwm_channels.sort_by_key(|pwm| pwm.channel);
        report.devices.push(controller);
    }
    report
        .devices
        .sort_by(|a, b| a.sysfs_path.cmp(&b.sysfs_path));
    report.sort_warnings();
    Ok(report)
}

fn selected_channel(root: &Path, channel: u8) -> io::Result<(PathBuf, PwmChannel)> {
    let report = discover_fan_controllers(root)?;
    if report.devices.len() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "expected one nct6799 controller",
        ));
    }
    let controller = &report.devices[0];
    let pwm = controller
        .pwm_channels
        .iter()
        .find(|pwm| pwm.channel == channel)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "PWM channel not available"))?;
    Ok((controller.sysfs_path.clone(), pwm.clone()))
}

/// Check that exactly one controller exposes `channel`, without writing anything.
pub fn validate_channel(root: &Path, channel: u8) -> io::Result<()> {
    selected_channel(root, channel).map(|_| ())
}

/// Validate a 30-100% manual duty and convert it to the raw 0-255 PWM value.
pub fn manual_duty(percent: u8) -> io::Result<u8> {
    if !(30..=100).contains(&percent) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "manual duty must be 30-100%",
        ));
    }
    Ok(((u16::from(percent) * 255 + 50) / 100) as u8)
}

trait PwmIo {
    fn read(&self, path: &Path) -> io::Result<u8>;
    fn write(&self, path: &Path, value: u8) -> io::Result<()>;
}

struct SysfsPwmIo;

impl PwmIo for SysfsPwmIo {
    fn read(&self, path: &Path) -> io::Result<u8> {
        read_number(path)
    }

    fn write(&self, path: &Path, value: u8) -> io::Result<()> {
        fs::write(path, value.to_string())
    }
}

fn apply_manual(io: &impl PwmIo, path: &Path, pwm: &PwmChannel, raw: u8) -> io::Result<()> {
    let duty_path = path.join(format!("pwm{}", pwm.channel));
    let enable_path = path.join(format!("pwm{}_enable", pwm.channel));
    let result = (|| {
        // The nct6775 driver rejects duty writes while firmware auto mode is active.
        if pwm.enable != 1 {
            io.write(&enable_path, 1)?;
        }
        io.write(&duty_path, raw)?;
        if io.read(&enable_path)? != 1 || io.read(&duty_path)? != raw {
            return Err(io::Error::other(
                "PWM readback did not match the requested state",
            ));
        }
        Ok(())
    })();
    if result.is_err() {
        // Best effort: restore the observed duty while in manual mode, then mode.
        if io.read(&enable_path).ok() == Some(1) {
            let _ = io.write(&duty_path, pwm.duty);
        }
        if pwm.enable != 1 {
            let _ = io.write(&enable_path, pwm.enable);
        }
    }
    result
}

/// Set an explicitly selected PWM channel to a manual duty.
/// The caller must map the channel to a physical fan before invoking this.
pub fn set_manual(root: &Path, channel: u8, percent: u8) -> io::Result<()> {
    let raw = manual_duty(percent)?;
    let (path, pwm) = selected_channel(root, channel)?;
    if !(1..=5).contains(&pwm.enable) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected PWM control mode",
        ));
    }
    apply_manual(&SysfsPwmIo, &path, &pwm, raw)
}

/// Return one channel to the Smart Fan IV mode observed on this controller.
pub fn restore_firmware_auto(root: &Path, channel: u8) -> io::Result<()> {
    let (path, _) = selected_channel(root, channel)?;
    let enable_path = path.join(format!("pwm{channel}_enable"));
    fs::write(&enable_path, "5")?;
    if read_number::<u8>(&enable_path)? != 5 {
        return Err(io::Error::other("firmware automatic mode readback failed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        sync::atomic::{AtomicUsize, Ordering},
    };

    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("asusio-fan-test-{}-{id}", std::process::id()));
            let path = root.join("hwmon9");
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("name"), "nct6799\n").unwrap();
            fs::write(path.join("fan2_input"), "875\n").unwrap();
            fs::write(path.join("pwm2"), "94\n").unwrap();
            fs::write(path.join("pwm2_enable"), "5\n").unwrap();
            fs::write(path.join("pwm2_mode"), "1\n").unwrap();
            Self(root)
        }

        fn attr(&self, name: &str) -> PathBuf {
            self.0.join("hwmon9").join(name)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn lists_only_existing_channels_without_assuming_fan_mapping() {
        let fixture = Fixture::new();
        let report = discover_fan_controllers(&fixture.0).unwrap();
        assert_eq!(report.devices.len(), 1);
        assert_eq!(report.devices[0].fan_speeds[0].channel, 2);
        assert_eq!(report.devices[0].pwm_channels[0].channel, 2);
        assert_eq!(report.devices[0].pwm_channels[0].enable, 5);
    }

    #[test]
    fn manual_write_and_firmware_restore_are_explicit() {
        let fixture = Fixture::new();
        assert!(set_manual(&fixture.0, 2, 20).is_err());
        assert_eq!(fs::read_to_string(fixture.attr("pwm2")).unwrap(), "94\n");
        set_manual(&fixture.0, 2, 60).unwrap();
        assert_eq!(fs::read_to_string(fixture.attr("pwm2")).unwrap(), "153");
        assert_eq!(
            fs::read_to_string(fixture.attr("pwm2_enable")).unwrap(),
            "1"
        );
        restore_firmware_auto(&fixture.0, 2).unwrap();
        assert_eq!(
            fs::read_to_string(fixture.attr("pwm2_enable")).unwrap(),
            "5"
        );
    }

    struct DriverLikeIo {
        duty: Cell<u8>,
        enable: Cell<u8>,
        reject_duty: bool,
        writes: RefCell<Vec<(&'static str, u8)>>,
    }

    impl PwmIo for DriverLikeIo {
        fn read(&self, path: &Path) -> io::Result<u8> {
            if path.to_string_lossy().ends_with("_enable") {
                Ok(self.enable.get())
            } else {
                Ok(self.duty.get())
            }
        }

        fn write(&self, path: &Path, value: u8) -> io::Result<()> {
            if path.to_string_lossy().ends_with("_enable") {
                self.enable.set(value);
                self.writes.borrow_mut().push(("enable", value));
            } else {
                if self.enable.get() != 1 {
                    return Err(io::Error::from_raw_os_error(16));
                }
                if self.reject_duty {
                    return Err(io::Error::from_raw_os_error(5));
                }
                self.duty.set(value);
                self.writes.borrow_mut().push(("duty", value));
            }
            Ok(())
        }
    }

    #[test]
    fn switches_to_manual_before_writing_duty() {
        let io = DriverLikeIo {
            duty: Cell::new(94),
            enable: Cell::new(5),
            reject_duty: false,
            writes: RefCell::new(Vec::new()),
        };
        let pwm = PwmChannel {
            channel: 2,
            duty: 94,
            enable: 5,
            mode: Some(1),
        };
        apply_manual(&io, Path::new("/fixture"), &pwm, 153).unwrap();
        assert_eq!(*io.writes.borrow(), [("enable", 1), ("duty", 153)]);
    }

    #[test]
    fn restores_auto_mode_when_manual_duty_write_fails() {
        let io = DriverLikeIo {
            duty: Cell::new(94),
            enable: Cell::new(5),
            reject_duty: true,
            writes: RefCell::new(Vec::new()),
        };
        let pwm = PwmChannel {
            channel: 2,
            duty: 94,
            enable: 5,
            mode: Some(1),
        };
        assert!(apply_manual(&io, Path::new("/fixture"), &pwm, 153).is_err());
        assert_eq!(io.enable.get(), 5);
        assert_eq!(io.duty.get(), 94);
    }
}
