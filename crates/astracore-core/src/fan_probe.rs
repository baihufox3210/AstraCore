//! Finding out which fan inputs follow which PWM output, by raising one output at a time.
//!
//! The test only ever raises a duty, never lowers it, restores the firmware's mode after each
//! output, stops at a temperature limit, and skips outputs the firmware already runs near
//! full speed (a pump is often run that way). All hardware access goes through [`ProbeIo`],
//! so the logic is tested against a simulated chip.
use crate::fan_map::{FULL_SPEED_PERCENT, ProbedChannel, TachResponse, responds};
use std::{collections::BTreeMap, io, time::Duration};

/// Hardware access for the probe. Implemented over sysfs by the daemon.
pub trait ProbeIo {
    fn duty_percent(&mut self, pwm: u8) -> io::Result<u8>;
    /// `pwmN_enable`: 1 means someone already controls the output manually.
    fn enable(&mut self, pwm: u8) -> io::Result<u8>;
    fn rpms(&mut self) -> io::Result<BTreeMap<u8, u64>>;
    /// Remember the firmware's mode of `pwm` before it is taken over.
    fn before_takeover(&mut self, pwm: u8) -> io::Result<()>;
    fn set_manual(&mut self, pwm: u8, percent: u8) -> io::Result<()>;
    fn restore(&mut self, pwm: u8) -> io::Result<()>;
    fn cpu_celsius(&mut self) -> Option<f64>;
    /// Wait; `false` when the probe must stop (the daemon is shutting down).
    fn sleep(&mut self, duration: Duration) -> bool;
}

#[derive(Debug, Clone)]
pub struct ProbeConfig {
    /// Duty to raise an output to, in percent.
    pub target_percent: u8,
    /// Raise by at least this many percentage points.
    pub min_step: u8,
    /// Seconds after raising at which speeds are sampled; the last sample is the result.
    pub samples: Vec<u64>,
    /// Wait after restoring, so the fan is back at its firmware speed before the next test.
    pub recover: Duration,
    /// Stop everything and restore at this CPU temperature.
    pub abort_celsius: f64,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            target_percent: 80,
            min_step: 10,
            samples: vec![4, 8, 12],
            recover: Duration::from_secs(12),
            abort_celsius: 85.0,
        }
    }
}

/// Why a probe run ended early.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    Hot,
    Cancelled,
}

fn note(result: &mut ProbedChannel, text: impl Into<String>) {
    result.note = Some(text.into());
}

/// Test one output. The firmware mode is always restored before returning.
pub fn probe_channel(
    io: &mut dyn ProbeIo,
    cfg: &ProbeConfig,
    pwm: u8,
) -> (ProbedChannel, Option<Stop>) {
    let mut result = ProbedChannel {
        pwm,
        baseline_percent: 0,
        tested: false,
        note: None,
        tachs: Vec::new(),
    };
    let baseline = match io.duty_percent(pwm) {
        Ok(duty) => duty,
        Err(error) => {
            note(&mut result, format!("cannot read the duty: {error}"));
            return (result, None);
        }
    };
    result.baseline_percent = baseline;
    match io.enable(pwm) {
        Ok(1) => {
            note(&mut result, "another program already controls it manually");
            return (result, None);
        }
        Err(error) => {
            note(
                &mut result,
                format!("cannot read the control mode: {error}"),
            );
            return (result, None);
        }
        Ok(_) => {}
    }
    if baseline >= FULL_SPEED_PERCENT {
        note(
            &mut result,
            format!(
                "the firmware already runs it at {baseline}%, so it cannot be tested by raising it (pumps are often run this way)"
            ),
        );
        return (result, None);
    }
    let Ok(before) = io.rpms() else {
        note(&mut result, "cannot read the fan speeds");
        return (result, None);
    };
    let target = cfg
        .target_percent
        .max(baseline.saturating_add(cfg.min_step))
        .min(100);
    if let Err(error) = io.before_takeover(pwm) {
        note(
            &mut result,
            format!("cannot record the firmware mode: {error}"),
        );
        return (result, None);
    }
    if let Err(error) = io.set_manual(pwm, target) {
        let _ = io.restore(pwm);
        note(&mut result, format!("cannot raise it: {error}"));
        return (result, None);
    }

    let mut after = before.clone();
    let mut stop = None;
    let mut elapsed = 0;
    for &second in &cfg.samples {
        if !io.sleep(Duration::from_secs(second.saturating_sub(elapsed))) {
            stop = Some(Stop::Cancelled);
            break;
        }
        elapsed = second;
        if io.cpu_celsius().is_none_or(|c| c >= cfg.abort_celsius) {
            stop = Some(Stop::Hot);
            break;
        }
        if let Ok(speeds) = io.rpms() {
            after = speeds;
        }
    }
    let restored = io.restore(pwm);
    if stop.is_none() {
        io.sleep(cfg.recover);
    }

    result.tested = stop.is_none();
    result.tachs = after
        .iter()
        .filter_map(|(tach, after_rpm)| {
            let before_rpm = *before.get(tach)?;
            (stop.is_none() && responds(before_rpm, *after_rpm)).then_some(TachResponse {
                tach: *tach,
                before_rpm,
                after_rpm: *after_rpm,
            })
        })
        .collect();
    match (&stop, restored) {
        (Some(Stop::Hot), _) => note(&mut result, "stopped: the CPU is too hot"),
        (Some(Stop::Cancelled), _) => note(&mut result, "stopped: the service is shutting down"),
        (None, Err(error)) => note(
            &mut result,
            format!("WARNING: could not restore the firmware mode: {error}"),
        ),
        (None, Ok(())) => {}
    }
    (result, stop)
}

/// Test every output in turn, reporting progress as `(finished, total, current output)`.
/// After a stop, the outputs not yet tested are listed as untested.
pub fn probe_all(
    io: &mut dyn ProbeIo,
    cfg: &ProbeConfig,
    pwms: &[u8],
    progress: &mut dyn FnMut(usize, usize, u8),
) -> Vec<ProbedChannel> {
    let mut results = Vec::new();
    let mut stopped = false;
    for (index, &pwm) in pwms.iter().enumerate() {
        if stopped {
            results.push(ProbedChannel {
                pwm,
                baseline_percent: io.duty_percent(pwm).unwrap_or(0),
                tested: false,
                note: Some("not tested: the run was stopped".to_owned()),
                tachs: Vec::new(),
            });
            continue;
        }
        progress(index, pwms.len(), pwm);
        let (result, stop) = probe_channel(io, cfg, pwm);
        stopped = stop.is_some();
        results.push(result);
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chip with two PWM outputs: pwm2 drives fans 2 and 5 (a shared output), pwm4 drives
    /// fan 4, pwm7 runs a pump at full speed. Speeds follow the duty immediately.
    struct Chip {
        duty: BTreeMap<u8, u8>,
        enable: BTreeMap<u8, u8>,
        wiring: Vec<(u8, u8, u64)>, // (pwm, tach, rpm at 100%)
        log: Vec<String>,
        celsius: f64,
        fail_set: Option<u8>,
        cancel_after: Option<usize>,
        sleeps: usize,
    }

    impl Chip {
        fn new() -> Self {
            Self {
                duty: [(2, 35), (4, 35), (7, 100), (3, 60)].into(),
                enable: [(2, 5), (4, 5), (7, 5), (3, 5)].into(),
                wiring: vec![(2, 2, 2400), (2, 5, 2200), (4, 4, 2300), (7, 7, 2830)],
                log: Vec::new(),
                celsius: 50.0,
                fail_set: None,
                cancel_after: None,
                sleeps: 0,
            }
        }
    }

    impl ProbeIo for Chip {
        fn duty_percent(&mut self, pwm: u8) -> io::Result<u8> {
            self.duty
                .get(&pwm)
                .copied()
                .ok_or_else(|| io::Error::other("no such pwm"))
        }
        fn enable(&mut self, pwm: u8) -> io::Result<u8> {
            Ok(self.enable[&pwm])
        }
        fn rpms(&mut self) -> io::Result<BTreeMap<u8, u64>> {
            let mut speeds: BTreeMap<u8, u64> = (1..=7).map(|tach| (tach, 0)).collect();
            for &(pwm, tach, full) in &self.wiring {
                speeds.insert(tach, full * u64::from(self.duty[&pwm]) / 100);
            }
            Ok(speeds)
        }
        fn before_takeover(&mut self, pwm: u8) -> io::Result<()> {
            self.log.push(format!("record {pwm}"));
            Ok(())
        }
        fn set_manual(&mut self, pwm: u8, percent: u8) -> io::Result<()> {
            if self.fail_set == Some(pwm) {
                return Err(io::Error::other("permission denied"));
            }
            self.log.push(format!("set {pwm} {percent}"));
            self.enable.insert(pwm, 1);
            self.duty.insert(pwm, percent);
            Ok(())
        }
        fn restore(&mut self, pwm: u8) -> io::Result<()> {
            self.log.push(format!("restore {pwm}"));
            self.enable.insert(pwm, 5);
            Ok(())
        }
        fn cpu_celsius(&mut self) -> Option<f64> {
            Some(self.celsius)
        }
        fn sleep(&mut self, _duration: Duration) -> bool {
            self.sleeps += 1;
            self.cancel_after.is_none_or(|limit| self.sleeps <= limit)
        }
    }

    fn config() -> ProbeConfig {
        ProbeConfig::default()
    }

    #[test]
    fn finds_the_shared_output_and_never_lowers_a_duty() {
        let mut chip = Chip::new();
        let mut steps = Vec::new();
        let results = probe_all(
            &mut chip,
            &config(),
            &[2, 3, 4, 7],
            &mut |done, total, pwm| {
                steps.push((done, total, pwm));
            },
        );
        assert_eq!(steps, [(0, 4, 2), (1, 4, 3), (2, 4, 4), (3, 4, 7)]);
        let shared = &results[0];
        assert!(shared.tested);
        let tachs: Vec<_> = shared.tachs.iter().map(|t| t.tach).collect();
        assert_eq!(tachs, [2, 5], "pwm2 moved both inputs");
        let (before, after) = (shared.tachs[1].before_rpm, shared.tachs[1].after_rpm);
        assert!(after > before);
        assert!(results[1].tachs.is_empty(), "pwm3 has nothing connected");
        assert_eq!(
            results[2].tachs.iter().map(|t| t.tach).collect::<Vec<_>>(),
            [4]
        );
        // The pump output was never written.
        assert!(!results[3].tested);
        assert!(results[3].note.as_ref().unwrap().contains("100%"));
        assert!(!chip.log.iter().any(|entry| entry.contains(" 7 ")));
        // Every raise was upward from the firmware duty (35 -> 80, 60 -> 80).
        assert!(chip.log.contains(&"set 2 80".to_owned()));
        assert!(chip.log.contains(&"set 3 80".to_owned()));
        // Each taken-over output was recorded first and restored after.
        for pwm in [2, 3, 4] {
            let recorded = chip
                .log
                .iter()
                .position(|e| e == &format!("record {pwm}"))
                .unwrap();
            let set = chip
                .log
                .iter()
                .position(|e| e.starts_with(&format!("set {pwm} ")))
                .unwrap();
            let restored = chip
                .log
                .iter()
                .position(|e| e == &format!("restore {pwm}"))
                .unwrap();
            assert!(recorded < set && set < restored, "pwm{pwm}: {:?}", chip.log);
        }
        assert!(
            chip.enable.values().all(|mode| *mode == 5),
            "all back to firmware"
        );
    }

    #[test]
    fn raises_by_the_minimum_step_when_the_firmware_is_already_near_the_target() {
        let mut chip = Chip::new();
        chip.duty.insert(3, 78);
        let (result, _) = probe_channel(&mut chip, &config(), 3);
        assert!(chip.log.contains(&"set 3 88".to_owned()), "{:?}", chip.log);
        assert!(result.tested);
    }

    #[test]
    fn a_hot_cpu_stops_the_run_and_restores_the_output() {
        let mut chip = Chip::new();
        chip.celsius = 86.0;
        let results = probe_all(&mut chip, &config(), &[2, 4], &mut |_, _, _| {});
        assert!(!results[0].tested && !results[1].tested);
        assert!(results[0].note.as_ref().unwrap().contains("too hot"));
        assert!(results[1].note.as_ref().unwrap().contains("not tested"));
        assert!(chip.log.contains(&"restore 2".to_owned()));
        assert!(
            !chip.log.iter().any(|entry| entry.starts_with("set 4")),
            "pwm4 untouched"
        );
        assert!(chip.enable.values().all(|mode| *mode == 5));
    }

    #[test]
    fn cancelling_restores_and_stops() {
        let mut chip = Chip::new();
        chip.cancel_after = Some(1);
        let results = probe_all(&mut chip, &config(), &[2, 4], &mut |_, _, _| {});
        assert!(!results[0].tested);
        assert!(results[0].note.as_ref().unwrap().contains("shutting down"));
        assert!(chip.log.contains(&"restore 2".to_owned()));
        assert!(!chip.log.iter().any(|entry| entry.starts_with("set 4")));
    }

    #[test]
    fn a_failed_write_is_reported_and_the_mode_restored() {
        let mut chip = Chip::new();
        chip.fail_set = Some(2);
        let (result, stop) = probe_channel(&mut chip, &config(), 2);
        assert!(!result.tested && stop.is_none());
        assert!(result.note.unwrap().contains("permission denied"));
        assert!(chip.log.contains(&"restore 2".to_owned()));
    }

    #[test]
    fn manual_control_by_someone_else_is_left_alone() {
        let mut chip = Chip::new();
        chip.enable.insert(2, 1);
        let (result, _) = probe_channel(&mut chip, &config(), 2);
        assert!(!result.tested);
        assert!(chip.log.is_empty(), "nothing written: {:?}", chip.log);
    }

    #[test]
    fn an_unreadable_temperature_counts_as_hot() {
        struct Blind(Chip);
        impl ProbeIo for Blind {
            fn duty_percent(&mut self, p: u8) -> io::Result<u8> {
                self.0.duty_percent(p)
            }
            fn enable(&mut self, p: u8) -> io::Result<u8> {
                self.0.enable(p)
            }
            fn rpms(&mut self) -> io::Result<BTreeMap<u8, u64>> {
                self.0.rpms()
            }
            fn before_takeover(&mut self, p: u8) -> io::Result<()> {
                self.0.before_takeover(p)
            }
            fn set_manual(&mut self, p: u8, v: u8) -> io::Result<()> {
                self.0.set_manual(p, v)
            }
            fn restore(&mut self, p: u8) -> io::Result<()> {
                self.0.restore(p)
            }
            fn cpu_celsius(&mut self) -> Option<f64> {
                None
            }
            fn sleep(&mut self, d: Duration) -> bool {
                self.0.sleep(d)
            }
        }
        let mut chip = Blind(Chip::new());
        let (result, stop) = probe_channel(&mut chip, &config(), 2);
        assert_eq!(stop, Some(Stop::Hot));
        assert!(!result.tested);
        assert!(chip.0.enable.values().all(|mode| *mode == 5));
    }
}
