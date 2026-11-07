//! Helpers shared by backend transports.
use astracore_types::HidDevice;
use std::{fs, io, io::Write};

pub(crate) fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(crate) fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

pub(crate) fn hidraw_path(device: &HidDevice) -> io::Result<&str> {
    device
        .hidraw_path
        .as_deref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "hidraw node not found"))
}

/// Open the device's hidraw node for writing reports.
pub(crate) fn open_hidraw(device: &HidDevice) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .open(hidraw_path(device)?)
}

/// Send one report with a single `write`; a short write is an error and is never retried.
pub(crate) fn write_once(writer: &mut impl Write, report: &[u8], what: &str) -> io::Result<()> {
    if writer.write(report)? != report.len() {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            format!("short {what} write; report was not retried"),
        ));
    }
    Ok(())
}
