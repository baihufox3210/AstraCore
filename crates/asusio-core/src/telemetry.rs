//! Read-only live readings for status displays: CPU state, load, frequency,
//! temperatures, power and voltages; memory; fan and pump headers; NVIDIA GPUs; and
//! the active sched_ext scheduler. Only sysfs/procfs reads and NVML queries; no device
//! probing and no writes.
use crate::{
    board::{self, BoardProfile, FanKind},
    fan_curve::{HwmonTemperature, SensorRef, TemperatureSensor},
    service::ipc::{
        CpuFrequency, CpuSummary, CpuUsage, FanHeaderReading, LabeledValue, MemoryUsage,
        PwmControl, PwmReading, Telemetry,
    },
};
use asusio_backends::{cpu_pstate, fan_hwmon, nvidia_nvml::Nvml};
use asusio_discovery::{hwmon, sysfs};
use asusio_types::SensorValue;
use std::{
    collections::BTreeSet,
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

fn read_u64(path: &Path) -> io::Result<u64> {
    fs::read_to_string(path)?
        .trim()
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid counter"))
}

/// A powercap zone with this `name`, directly under `root` or one level below.
fn power_zone(root: &Path, name: &str) -> Option<PathBuf> {
    let children = |dir: &Path| -> Vec<PathBuf> {
        fs::read_dir(dir)
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default()
    };
    let mut candidates = children(root);
    candidates.sort();
    let nested: Vec<_> = candidates
        .iter()
        .filter(|path| path.is_dir())
        .flat_map(|path| children(path))
        .collect();
    candidates.into_iter().chain(nested).find(|path| {
        fs::read_to_string(path.join("name")).is_ok_and(|text| text.trim() == name)
            && path.join("energy_uj").exists()
    })
}

/// Average power of one powercap zone (root-readable on this platform).
pub struct PowerSampler {
    zone: Option<PathBuf>,
    last: Option<(u64, Instant)>,
    watts: Option<f64>,
}

impl PowerSampler {
    pub fn new(root: &Path, zone: &str) -> Self {
        Self {
            zone: power_zone(root, zone),
            last: None,
            watts: None,
        }
    }

    /// Average power since the previous sample. The first call only primes the
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

/// Busy and total jiffies of one `/proc/stat` CPU line.
fn cpu_ticks(line: &str) -> Option<(u64, u64)> {
    let values: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .map(|value| value.parse().ok())
        .collect::<Option<_>>()?;
    if values.len() < 4 {
        return None;
    }
    // user nice system idle iowait irq softirq steal; guest time is already in user.
    let total: u64 = values.iter().take(8).sum();
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    Some((total - idle, total))
}

/// Overall and per-CPU ticks from `/proc/stat` text.
fn parse_stat(text: &str) -> Option<Vec<(u64, u64)>> {
    let mut ticks = Vec::new();
    for line in text.lines().filter(|line| line.starts_with("cpu")) {
        ticks.push(cpu_ticks(line)?);
    }
    (!ticks.is_empty()).then_some(ticks)
}

fn usage(previous: &[(u64, u64)], current: &[(u64, u64)]) -> Option<CpuUsage> {
    if previous.len() != current.len() || current.is_empty() {
        return None;
    }
    let percent = |(busy0, total0): (u64, u64), (busy1, total1): (u64, u64)| {
        let total = total1.saturating_sub(total0);
        if total == 0 {
            return 0.0;
        }
        let value = busy1.saturating_sub(busy0) as f64 * 100.0 / total as f64;
        (value * 10.0).round() / 10.0
    };
    let mut all = previous.iter().zip(current).map(|(a, b)| percent(*a, *b));
    Some(CpuUsage {
        total_percent: all.next()?,
        cores_percent: all.collect(),
    })
}

/// CPU load from `/proc/stat` deltas.
pub struct LoadSampler {
    last: Option<Vec<(u64, u64)>>,
}

impl LoadSampler {
    pub fn sample(&mut self) -> Option<CpuUsage> {
        let current = parse_stat(&fs::read_to_string("/proc/stat").ok()?)?;
        let result = self.last.as_ref().and_then(|last| usage(last, &current));
        self.last = Some(current);
        result
    }
}

/// Average and highest effective frequency over online policies, in MHz.
fn cpu_frequency(root: &Path) -> Option<CpuFrequency> {
    let mut khz = Vec::new();
    for entry in fs::read_dir(root.join("cpufreq")).ok()?.flatten() {
        let path = entry.path();
        if !entry.file_name().to_string_lossy().starts_with("policy") {
            continue;
        }
        // cpuinfo_avg_freq is the hardware-measured average; older kernels only have
        // the requested scaling_cur_freq.
        let value = read_u64(&path.join("cpuinfo_avg_freq"))
            .or_else(|_| read_u64(&path.join("scaling_cur_freq")));
        if let Ok(value) = value {
            khz.push(value);
        }
    }
    let max = *khz.iter().max()?;
    let average = khz.iter().sum::<u64>() / khz.len() as u64;
    Some(CpuFrequency {
        average_mhz: (average / 1000) as u32,
        max_mhz: (max / 1000) as u32,
    })
}

/// Used and total memory from `/proc/meminfo` text (`MemTotal - MemAvailable`).
fn memory(text: &str) -> Option<MemoryUsage> {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
            .map(|kib| kib * 1024)
    };
    let total = field("MemTotal:")?;
    let available = field("MemAvailable:")?;
    Some(MemoryUsage {
        used_bytes: total.saturating_sub(available),
        total_bytes: total,
    })
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

/// `Tccd1`, `Tccd2`, ... from k10temp.
fn ccd_temperatures() -> Vec<LabeledValue> {
    let Ok(report) = hwmon::discover(sysfs::HWMON) else {
        return Vec::new();
    };
    let mut values: Vec<_> = report
        .devices
        .iter()
        .filter(|device| device.name == "k10temp")
        .flat_map(|device| &device.sensors)
        .filter(|sensor| sensor.label.starts_with("Tccd"))
        .filter_map(|sensor| match sensor.value {
            SensorValue::Temperature { milli_celsius } => Some(LabeledValue {
                label: sensor.label.clone(),
                value: milli_celsius as f64 / 1000.0,
            }),
            SensorValue::FanSpeedRpm { .. } => None,
        })
        .collect();
    values.sort_by(|a, b| a.label.cmp(&b.label));
    values
}

/// The hwmon directory of the chip named `name`, if exactly one exists.
fn hwmon_dir(name: &str) -> Option<PathBuf> {
    let matches: Vec<_> = fs::read_dir(sysfs::HWMON)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| fs::read_to_string(path.join("name")).is_ok_and(|text| text.trim() == name))
        .collect();
    (matches.len() == 1).then(|| matches[0].clone())
}

/// Board voltages, named and scaled by the profile.
fn voltages(profile: &BoardProfile) -> Vec<LabeledValue> {
    let Some(dir) = hwmon_dir(&profile.controller) else {
        return Vec::new();
    };
    profile
        .voltages
        .iter()
        .filter_map(|input| {
            let millivolts = read_u64(&dir.join(format!("in{}_input", input.input))).ok()?;
            let volts = millivolts as f64 / 1000.0 * input.scale;
            Some(LabeledValue {
                label: input.label.clone(),
                value: (volts * 1000.0).round() / 1000.0,
            })
        })
        .collect()
}

/// Combine tachometers and PWM outputs into headers. A header counts as connected once
/// it has reported a speed; `seen` remembers that across samples, so a fan that stops
/// at low temperature stays listed.
fn fan_headers(
    profile: Option<&BoardProfile>,
    seen: &mut BTreeSet<u8>,
    errors: &mut Vec<String>,
) -> (Vec<FanHeaderReading>, Vec<String>) {
    let report = match fan_hwmon::discover_fan_controllers(Path::new(sysfs::HWMON)) {
        Ok(report) => report,
        Err(error) => {
            errors.push(format!("fans: {error}"));
            return (Vec::new(), Vec::new());
        }
    };
    let (mut connected, mut unused) = (Vec::new(), Vec::new());
    for controller in &report.devices {
        for fan in &controller.fan_speeds {
            if fan.rpm > 0 {
                seen.insert(fan.channel);
            }
            let header = profile.and_then(|p| p.header_for_tach(fan.channel));
            if !seen.contains(&fan.channel) {
                unused.push(
                    header.map_or_else(|| format!("fan{}", fan.channel), |h| h.header.clone()),
                );
                continue;
            }
            let pwm_channel = header.map_or(Some(fan.channel), |h| h.pwm);
            let pwm = pwm_channel.and_then(|channel| {
                controller
                    .pwm_channels
                    .iter()
                    .find(|pwm| pwm.channel == channel)
                    .map(|pwm| PwmReading {
                        channel: pwm.channel,
                        percent: ((u16::from(pwm.duty) * 100 + 127) / 255) as u8,
                        enable: pwm.enable,
                        control: if pwm.enable == 1 {
                            PwmControl::Manual
                        } else {
                            PwmControl::Firmware
                        },
                    })
            });
            connected.push(FanHeaderReading {
                channel: fan.channel,
                header: header.map(|h| h.header.clone()),
                label: header.map(|h| h.label.clone()),
                kind: header.map_or(FanKind::Unknown, |h| h.kind),
                shares_pwm_with: header.and_then(|h| h.shares_pwm_with.clone()),
                rpm: fan.rpm,
                pwm,
            });
        }
    }
    (connected, unused)
}

/// All state carried between telemetry samples. The daemon keeps one for its lifetime;
/// a one-shot reader gets no power or load until the second sample.
pub struct Sampler {
    package: PowerSampler,
    core: PowerSampler,
    load: LoadSampler,
    seen: BTreeSet<u8>,
    profile: Option<BoardProfile>,
    nvml: Option<Nvml>,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        let powercap = Path::new(sysfs::POWERCAP);
        Self {
            package: PowerSampler::new(powercap, "package-0"),
            core: PowerSampler::new(powercap, "core"),
            load: LoadSampler { last: None },
            seen: BTreeSet::new(),
            profile: board::detect(),
            nvml: None,
        }
    }

    pub fn profile(&self) -> Option<&BoardProfile> {
        self.profile.as_ref()
    }

    /// Connected and unconnected fan headers without taking any other reading.
    pub fn headers(&mut self) -> (Vec<FanHeaderReading>, Vec<String>) {
        let mut errors = Vec::new();
        fan_headers(self.profile.as_ref(), &mut self.seen, &mut errors)
    }

    /// Prime the counters so the next [`read`](Self::read) has load and power.
    pub fn prime(&mut self) {
        self.load.sample();
        self.package.sample();
        self.core.sample();
    }

    /// Take every reading. Unavailable sources are left empty and listed in `errors`;
    /// the caller fills in profile and fan-curve fields.
    pub fn read(&mut self) -> Telemetry {
        let mut errors = Vec::new();
        let cpu_root = Path::new(sysfs::CPU);
        let cpu = cpu_pstate::inspect(cpu_root);
        let tctl_celsius = tctl()
            .map_err(|error| errors.push(format!("Tctl: {error}")))
            .ok();
        let (fans, unused_headers) =
            fan_headers(self.profile.as_ref(), &mut self.seen, &mut errors);
        if self.nvml.is_none() {
            self.nvml = Nvml::open().ok();
        }
        let (gpus, gpu_error) = match &self.nvml {
            Some(nvml) => (nvml.read(), None),
            None => (Vec::new(), Some("NVIDIA NVML is not available".to_owned())),
        };
        Telemetry {
            mode: None,
            in_sync: None,
            cpu: CpuSummary::from(&cpu),
            tctl_celsius,
            ccd_celsius: ccd_temperatures(),
            cpu_usage: self.load.sample(),
            cpu_frequency: cpu_frequency(cpu_root),
            package_watts: self.package.sample(),
            core_watts: self.core.sample(),
            memory: fs::read_to_string("/proc/meminfo")
                .ok()
                .and_then(|text| memory(&text)),
            voltages: self.profile.as_ref().map(voltages).unwrap_or_default(),
            scheduler: scheduler(Path::new(sysfs::SCHED_EXT)),
            board: self
                .profile
                .as_ref()
                .and_then(|p| p.boards.first().cloned()),
            fans,
            unused_headers,
            fan: None,
            gpus,
            gpu_error,
            errors,
        }
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
    fn finds_package_and_nested_core_zones() {
        let root = std::env::temp_dir().join(format!("asusio-powercap-{}", std::process::id()));
        let package = root.join("intel-rapl:0");
        let core = package.join("intel-rapl:0:0");
        fs::create_dir_all(&core).unwrap();
        for (zone, name) in [(&package, "package-0"), (&core, "core")] {
            fs::write(zone.join("name"), format!("{name}\n")).unwrap();
            fs::write(zone.join("energy_uj"), "5000000\n").unwrap();
        }
        assert_eq!(power_zone(&root, "package-0"), Some(package));
        assert_eq!(power_zone(&root, "core"), Some(core.clone()));
        let mut sampler = PowerSampler::new(&root, "core");
        assert_eq!(sampler.sample(), None, "first sample primes the counter");
        assert_eq!(sampler.sample(), None, "too soon; cached value");
        assert_eq!(power_zone(&root, "dram"), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_scheduler_from_fixtures() {
        let root = std::env::temp_dir().join(format!("asusio-sched-{}", std::process::id()));
        fs::create_dir_all(root.join("root")).unwrap();
        fs::write(root.join("state"), "enabled\n").unwrap();
        fs::write(root.join("root/ops"), "lavd_1.1.4\n").unwrap();
        assert_eq!(scheduler(&root).as_deref(), Some("lavd_1.1.4"));
        fs::write(root.join("state"), "disabled\n").unwrap();
        assert_eq!(scheduler(&root), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cpu_usage_from_stat_deltas() {
        let before = "cpu  100 0 100 800 0 0 0 0 0 0\ncpu0 50 0 50 400 0 0 0 0\ncpu1 50 0 50 400 0 0 0 0\nintr 1\n";
        let after = "cpu  200 0 150 850 0 0 0 0 0 0\ncpu0 150 0 50 400 0 0 0 0\ncpu1 50 0 100 450 0 0 0 0\n";
        let usage = usage(&parse_stat(before).unwrap(), &parse_stat(after).unwrap()).unwrap();
        assert_eq!(usage.total_percent, 75.0);
        assert_eq!(usage.cores_percent, [100.0, 50.0]);
        assert!(parse_stat("cpu  x\n").is_none());
        let fewer = parse_stat("cpu  1 0 0 1\n").unwrap();
        assert!(
            super::usage(&fewer, &parse_stat(after).unwrap()).is_none(),
            "CPU count changed"
        );
    }

    #[test]
    fn memory_uses_available_not_free() {
        let text = "MemTotal:       32000000 kB\nMemFree:         1000000 kB\nMemAvailable:   20000000 kB\n";
        let memory = memory(text).unwrap();
        assert_eq!(memory.total_bytes, 32_000_000 * 1024);
        assert_eq!(memory.used_bytes, 12_000_000 * 1024);
        assert!(super::memory("MemTotal: 1 kB\n").is_none());
    }
}
