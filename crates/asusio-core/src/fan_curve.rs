//! Temperature-driven fan curves with fail-safe release to firmware control.
use asusio_backends::fan_hwmon;
use asusio_discovery::{hwmon, sysfs};
pub use asusio_hal::{PwmOutput, TemperatureSensor};
use asusio_types::SensorValue;
use serde::{Deserialize, Serialize};
use std::{io, path::Path};

/// Lowest manual duty a curve may request; matches the backend's manual range.
pub const MIN_PERCENT: u8 = 30;
/// Readings outside this range are treated as sensor faults.
const PLAUSIBLE_CELSIUS: std::ops::RangeInclusive<f64> = -20.0..=150.0;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// One hwmon temperature identified by chip name and sensor label, e.g. `k10temp` / `Tctl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensorRef {
    pub chip: String,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurvePoint {
    pub celsius: f64,
    pub percent: u8,
}

fn default_hysteresis() -> f64 {
    3.0
}

/// A curve for one nct6799 PWM channel. The channel-to-header mapping must be verified
/// by the user; asusio does not infer it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FanCurve {
    pub channel: u8,
    pub sensor: SensorRef,
    pub points: Vec<CurvePoint>,
    /// Duty only decreases after the temperature falls this far below where it last rose.
    #[serde(default = "default_hysteresis")]
    pub hysteresis_celsius: f64,
}

impl FanCurve {
    /// At least two points, strictly increasing temperatures, non-decreasing duty within
    /// 30..=100, ending at 100% so overheating always reaches full speed.
    pub fn validate(&self) -> io::Result<()> {
        let points = &self.points;
        if points.len() < 2 {
            return Err(invalid("fan curve needs at least two points"));
        }
        if points.iter().any(|point| {
            !PLAUSIBLE_CELSIUS.contains(&point.celsius)
                || !(MIN_PERCENT..=100).contains(&point.percent)
        }) {
            return Err(invalid(
                "fan curve points need -20..150 °C and 30..100 percent",
            ));
        }
        if points
            .windows(2)
            .any(|pair| pair[1].celsius <= pair[0].celsius || pair[1].percent < pair[0].percent)
        {
            return Err(invalid(
                "fan curve temperatures must increase and duty must not decrease",
            ));
        }
        if points.last().is_some_and(|point| point.percent != 100) {
            return Err(invalid("fan curve must end at 100 percent"));
        }
        if !(0.0..=10.0).contains(&self.hysteresis_celsius) {
            return Err(invalid("hysteresis must be 0..10 °C"));
        }
        Ok(())
    }

    /// Linear interpolation, rounded up; clamped to the first and last points.
    pub fn duty_at(&self, celsius: f64) -> u8 {
        let points = &self.points;
        let (first, last) = (points[0], points[points.len() - 1]);
        if celsius <= first.celsius {
            return first.percent;
        }
        if celsius >= last.celsius {
            return last.percent;
        }
        let pair = points
            .windows(2)
            .find(|pair| celsius <= pair[1].celsius)
            .expect("temperature is within the curve range");
        let (low, high) = (pair[0], pair[1]);
        let fraction = (celsius - low.celsius) / (high.celsius - low.celsius);
        let duty = f64::from(low.percent) + fraction * f64::from(high.percent - low.percent);
        duty.ceil().min(100.0) as u8
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tick {
    pub celsius: f64,
    pub percent: u8,
}

/// Drives one output from one sensor. On any error the caller must call [`release`].
///
/// [`release`]: CurveController::release
pub struct CurveController<S, P> {
    curve: FanCurve,
    sensor: S,
    output: P,
    applied: Option<(u8, f64)>,
}

impl<S: TemperatureSensor, P: PwmOutput> CurveController<S, P> {
    pub fn new(curve: FanCurve, sensor: S, output: P) -> io::Result<Self> {
        curve.validate()?;
        Ok(Self {
            curve,
            sensor,
            output,
            applied: None,
        })
    }

    pub fn curve(&self) -> &FanCurve {
        &self.curve
    }

    pub fn sensor_name(&self) -> String {
        self.sensor.describe()
    }

    /// Read the sensor and update the duty when it rises, or falls past the hysteresis.
    pub fn tick(&mut self) -> io::Result<Tick> {
        let celsius = self.sensor.read_celsius()?;
        if !celsius.is_finite() || !PLAUSIBLE_CELSIUS.contains(&celsius) {
            return Err(invalid(format!(
                "{} reported an implausible {celsius} °C",
                self.sensor.describe()
            )));
        }
        let target = self.curve.duty_at(celsius);
        let change = match self.applied {
            None => true,
            Some((current, _)) if target > current => true,
            Some((current, since)) => {
                target < current && celsius <= since - self.curve.hysteresis_celsius
            }
        };
        if change {
            self.output.set_percent(target)?;
            self.applied = Some((target, celsius));
        }
        let percent = self.applied.map_or(target, |(percent, _)| percent);
        Ok(Tick { celsius, percent })
    }

    pub fn release(&mut self) -> io::Result<()> {
        self.applied = None;
        self.output.release()
    }
}

/// Reads one temperature through hwmon discovery; the chip/label pair must be unique.
pub struct HwmonTemperature {
    sensor: SensorRef,
}

impl HwmonTemperature {
    pub fn new(sensor: SensorRef) -> Self {
        Self { sensor }
    }
}

impl TemperatureSensor for HwmonTemperature {
    fn describe(&self) -> String {
        format!("{}/{}", self.sensor.chip, self.sensor.label)
    }

    fn read_celsius(&mut self) -> io::Result<f64> {
        let report = hwmon::discover(sysfs::HWMON)?;
        let readings: Vec<_> = report
            .devices
            .iter()
            .filter(|device| device.name == self.sensor.chip)
            .flat_map(|device| &device.sensors)
            .filter(|sensor| sensor.label == self.sensor.label)
            .filter_map(|sensor| match sensor.value {
                SensorValue::Temperature { milli_celsius } => Some(milli_celsius),
                SensorValue::FanSpeedRpm { .. } => None,
            })
            .collect();
        match readings.as_slice() {
            [milli] => Ok(*milli as f64 / 1000.0),
            [] => Err(invalid(format!(
                "temperature {} not found",
                self.describe()
            ))),
            _ => Err(invalid(format!(
                "temperature {} is ambiguous",
                self.describe()
            ))),
        }
    }
}

/// One nct6799 PWM channel through the validated sysfs backend.
pub struct Nct6799Pwm {
    channel: u8,
}

impl Nct6799Pwm {
    /// Fails unless exactly one controller exposes this channel.
    pub fn new(channel: u8) -> io::Result<Self> {
        fan_hwmon::validate_channel(Path::new(sysfs::HWMON), channel)?;
        Ok(Self { channel })
    }
}

impl PwmOutput for Nct6799Pwm {
    fn describe(&self) -> String {
        format!("nct6799 pwm{}", self.channel)
    }

    fn set_percent(&mut self, percent: u8) -> io::Result<()> {
        fan_hwmon::set_manual(Path::new(sysfs::HWMON), self.channel, percent)
    }

    fn release(&mut self) -> io::Result<()> {
        fan_hwmon::restore_firmware_auto(Path::new(sysfs::HWMON), self.channel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    fn curve() -> FanCurve {
        serde_json::from_value(serde_json::json!({
            "channel": 2,
            "sensor": {"chip": "k10temp", "label": "Tctl"},
            "points": [
                {"celsius": 40.0, "percent": 30},
                {"celsius": 60.0, "percent": 50},
                {"celsius": 80.0, "percent": 100}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn interpolates_rounding_up_and_clamps() {
        let curve = curve();
        curve.validate().unwrap();
        assert_eq!(curve.hysteresis_celsius, 3.0);
        assert_eq!(curve.duty_at(20.0), 30);
        assert_eq!(curve.duty_at(50.0), 40);
        assert_eq!(curve.duty_at(50.1), 41);
        assert_eq!(curve.duty_at(70.0), 75);
        assert_eq!(curve.duty_at(95.0), 100);
    }

    #[test]
    fn rejects_unsafe_curves() {
        let mut bad = curve();
        bad.points[2].percent = 90;
        assert!(bad.validate().is_err(), "must end at 100%");
        let mut bad = curve();
        bad.points[0].percent = 20;
        assert!(bad.validate().is_err(), "below minimum duty");
        let mut bad = curve();
        bad.points[1].celsius = 40.0;
        assert!(bad.validate().is_err(), "non-increasing temperature");
        let mut bad = curve();
        bad.points[1].percent = 25;
        assert!(bad.validate().is_err(), "decreasing duty");
        let mut bad = curve();
        bad.points.truncate(1);
        assert!(bad.validate().is_err(), "single point");
    }

    struct FakeSensor(Rc<RefCell<Vec<io::Result<f64>>>>);
    impl TemperatureSensor for FakeSensor {
        fn describe(&self) -> String {
            "fake".into()
        }
        fn read_celsius(&mut self) -> io::Result<f64> {
            self.0.borrow_mut().remove(0)
        }
    }

    #[derive(Default)]
    struct FakePwm(Rc<RefCell<Vec<String>>>);
    impl PwmOutput for FakePwm {
        fn describe(&self) -> String {
            "fake".into()
        }
        fn set_percent(&mut self, percent: u8) -> io::Result<()> {
            self.0.borrow_mut().push(format!("set {percent}"));
            Ok(())
        }
        fn release(&mut self) -> io::Result<()> {
            self.0.borrow_mut().push("release".into());
            Ok(())
        }
    }

    #[test]
    fn rises_immediately_and_falls_only_past_hysteresis() {
        let readings = Rc::new(RefCell::new(vec![
            Ok(50.0),
            Ok(70.0),
            Ok(68.0),
            Ok(66.5),
            Ok(160.0),
        ]));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut controller =
            CurveController::new(curve(), FakeSensor(readings), FakePwm(log.clone())).unwrap();
        assert_eq!(controller.tick().unwrap().percent, 40);
        assert_eq!(controller.tick().unwrap().percent, 75);
        assert_eq!(controller.tick().unwrap().percent, 75, "within hysteresis");
        assert_eq!(controller.tick().unwrap().percent, 67, "past hysteresis");
        assert!(controller.tick().is_err(), "implausible reading");
        controller.release().unwrap();
        assert_eq!(*log.borrow(), ["set 40", "set 75", "set 67", "release"]);
    }
}
