//! Read-only live readings for status displays: CPU state, Tctl, fans, package power
//! and the active sched_ext scheduler. Only cheap sysfs reads; no device probing.
use crate::{
    fan_curve::{HwmonTemperature, SensorRef, TemperatureSensor},
    service::ipc::{CpuSummary, FanReading, PwmControl, PwmReading, Telemetry},
};
use asusio_backends::{cpu_pstate, fan_hwmon};
use asusio_discovery::sysfs;
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Energy counters are sampled no faster than this, and power is rounded to 0.1 W,
/// so only a coarse average is ever exposed (see the RAPL notes in the README).
const MIN_POWER_WINDOW: Duration = Duration::from_secs(1);

impl From<&io::Result<cpu_pstate::CpuState>> for CpuSummary {
    fn from(state: &io::Result<cpu_pstate::CpuState>) -> Self {
        match state {
            Ok(state) => Self {
                supported: state.support().is_ok(),
                reason: state.support().err(),
                governor: state.governor().map(str::to_owned),
                epp: state.epp().map(str::to_owned),
                boost: state.boost,
            },
            Err(error) => Self {
                supported: false,
                reason: Some(error.to_string()),
                governor: None,
                epp: None,
                boost: None,
            },
        }
    }
}

/// Package power from the powercap energy counter (root-readable on this platform).
pub struct PowerSampler {
    zone: Option<PathBuf>,
    last: Option<(u64, Instant)>,
    watts: Option<f64>,
}

fn read_u64(path: &Path) -> io::Result<u64> {
    fs::read_to_string(path)?
        .trim()
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid counter"))
}

/// The `package-0` zone directly under the powercap root, if any.
fn package_zone(root: &Path) -> Option<PathBuf> {
    fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            fs::read_to_string(path.join("name")).is_ok_and(|name| name.trim() == "package-0")
                && path.join("energy_uj").exists()
        })
}

impl PowerSampler {
    pub fn new(root: &Path) -> Self {
        Self {
            zone: package_zone(root),
            last: None,
            watts: None,
        }
    }

    /// Average package power since the previous sample. The first call only primes the
    /// counter; readers without permission get `None`.
    pub fn sample(&mut self) -> Option<f64> {
        let zone = self.zone.as_ref()?;
        let now = Instant::now();
        if self.last.is_some_and(|(_, at)| now - at < MIN_POWER_WINDOW) {
            return self.watts;
        }
        let Ok(energy) = read_u64(&zone.join("energy_uj")) else {
            self.watts = None;
            return None;
        };
        if let Some((previous, at)) = self.last {
            let range = read_u64(&zone.join("max_energy_range_uj")).unwrap_or(u64::MAX);
            self.watts = power(previous, energy, range, now - at);
        }
        self.last = Some((energy, now));
        self.watts
    }
}

/// Watts between two microjoule readings, handling one counter wrap.
fn power(previous: u64, current: u64, range: u64, elapsed: Duration) -> Option<f64> {
    let delta = if current >= previous {
        current - previous
    } else {
        range.checked_sub(previous)?.checked_add(current)?
    };
    let seconds = elapsed.as_secs_f64();
    (seconds > 0.0).then(|| (delta as f64 / 1e6 / seconds * 10.0).round() / 10.0)
}

/// The loaded sched_ext scheduler, e.g. `lavd_1.1.4_x86_64_unknown_linux_gnu`.
pub fn scheduler(root: &Path) -> Option<String> {
    let state = fs::read_to_string(root.join("state")).ok()?;
    if state.trim() != "enabled" {
        return None;
    }
    let ops = fs::read_to_string(root.join("root/ops")).ok()?;
    Some(ops.trim().to_owned()).filter(|ops| !ops.is_empty())
}

/// CPU die temperature from k10temp.
pub fn tctl() -> io::Result<f64> {
    HwmonTemperature::new(SensorRef {
        chip: "k10temp".into(),
        label: "Tctl".into(),
    })
    .read_celsius()
}

/// Collect every reading. Unavailable sources are left empty and listed in `errors`;
/// the caller fills in the profile and fan-curve fields.
pub fn read(power: Option<&mut PowerSampler>) -> Telemetry {
    let mut errors = Vec::new();
    let cpu = cpu_pstate::inspect(Path::new(sysfs::CPU));
    let tctl_celsius = tctl()
        .map_err(|error| errors.push(format!("Tctl: {error}")))
        .ok();
    let (mut fans, mut pwm) = (Vec::new(), Vec::new());
    match fan_hwmon::discover_fan_controllers(Path::new(sysfs::HWMON)) {
        Ok(report) => {
            for controller in &report.devices {
                fans.extend(controller.fan_speeds.iter().map(|fan| FanReading {
                    channel: fan.channel,
                    rpm: fan.rpm,
                }));
                pwm.extend(controller.pwm_channels.iter().map(|channel| PwmReading {
                    channel: channel.channel,
                    percent: ((u16::from(channel.duty) * 100 + 127) / 255) as u8,
                    enable: channel.enable,
                    control: if channel.enable == 1 {
                        PwmControl::Manual
                    } else {
                        PwmControl::Firmware
                    },
                }));
            }
        }
        Err(error) => errors.push(format!("fans: {error}")),
    }
    Telemetry {
        mode: None,
        in_sync: None,
        cpu: CpuSummary::from(&cpu),
        tctl_celsius,
        package_watts: power.and_then(PowerSampler::sample),
        scheduler: scheduler(Path::new(sysfs::SCHED_EXT)),
        fans,
        pwm,
        fan: None,
        errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_handles_wrap_and_rounds() {
        let second = Duration::from_secs(1);
        assert_eq!(power(1_000_000, 46_000_000, u64::MAX, second), Some(45.0));
        assert_eq!(power(1_000_000, 1_123_456, u64::MAX, second), Some(0.1));
        assert_eq!(power(90, 10, 100, Duration::from_micros(20)), Some(1.0));
        assert_eq!(power(200, 10, 100, second), None, "corrupt range");
        assert_eq!(power(0, 1, u64::MAX, Duration::ZERO), None);
    }

    #[test]
    fn reads_scheduler_and_package_zone_from_fixtures() {
        let root = std::env::temp_dir().join(format!("asusio-telemetry-{}", std::process::id()));
        fs::create_dir_all(root.join("sched/root")).unwrap();
        fs::write(root.join("sched/state"), "enabled\n").unwrap();
        fs::write(root.join("sched/root/ops"), "lavd_1.1.4\n").unwrap();
        assert_eq!(
            scheduler(&root.join("sched")).as_deref(),
            Some("lavd_1.1.4")
        );
        fs::write(root.join("sched/state"), "disabled\n").unwrap();
        assert_eq!(scheduler(&root.join("sched")), None);

        let zone = root.join("powercap/intel-rapl:0");
        fs::create_dir_all(zone.join("intel-rapl:0:0")).unwrap();
        fs::write(zone.join("name"), "package-0\n").unwrap();
        fs::write(zone.join("energy_uj"), "5000000\n").unwrap();
        let mut sampler = PowerSampler::new(&root.join("powercap"));
        assert_eq!(sampler.zone.as_deref(), Some(zone.as_path()));
        assert_eq!(sampler.sample(), None, "first sample primes the counter");
        assert_eq!(sampler.sample(), None, "too soon; cached value");
        fs::remove_dir_all(root).unwrap();
    }
}
