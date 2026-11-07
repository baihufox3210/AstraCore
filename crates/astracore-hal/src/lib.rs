//! Hardware abstraction traits used by control loops.
//!
//! Policies such as fan curves are written against these traits so they can be tested
//! without hardware; concrete implementations live in `astracore-core`.
use std::io;

/// A temperature source, such as one hwmon sensor.
pub trait TemperatureSensor {
    /// Human-readable identity for logs and status, e.g. `k10temp/Tctl`.
    fn describe(&self) -> String;
    fn read_celsius(&mut self) -> io::Result<f64>;
}

/// A fan output that can be driven manually or handed back to firmware control.
pub trait PwmOutput {
    fn describe(&self) -> String;
    /// Set a manual duty in percent. Implementations enforce their own safe range.
    fn set_percent(&mut self, percent: u8) -> io::Result<()>;
    /// Return control to the firmware's automatic mode.
    fn release(&mut self) -> io::Result<()>;
}
