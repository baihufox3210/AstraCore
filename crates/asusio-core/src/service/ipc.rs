//! Newline-delimited JSON protocol between clients (CLI, GUI) and `asusiod`.
//! One request and one response per connection.
use asusio_policy::Operation;
use asusio_types::Rgb;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

const MAX_REQUEST_BYTES: u64 = 64 * 1024;
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
/// Lighting writes can wait on several two-second HID queries.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Request {
    Status,
    /// Preview (or, with `execute`, apply and record) a lighting request.
    /// `lighting_config` must be an absolute path owned by the requesting user.
    SetLighting {
        lighting_config: PathBuf,
        target: String,
        rgb: Rgb,
        brightness: u8,
        calibrated: bool,
        execute: bool,
    },
    /// Reapply the recorded lighting state now.
    RestoreLighting,
}

impl Request {
    pub fn operation(&self) -> Operation {
        match self {
            Self::Status => Operation::ReadStatus,
            Self::SetLighting { execute: false, .. } => Operation::PreviewLighting,
            Self::SetLighting { execute: true, .. } => Operation::SetLighting,
            Self::RestoreLighting => Operation::RestoreLighting,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok { data: serde_json::Value },
    Error { message: String },
}

impl Response {
    pub fn from_result<T: Serialize>(result: io::Result<T>) -> Self {
        match result.and_then(|data| serde_json::to_value(data).map_err(io::Error::other)) {
            Ok(data) => Self::Ok { data },
            Err(error) => Self::Error {
                message: error.to_string(),
            },
        }
    }
}

/// Per-target result of a lighting request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetResult {
    pub alias: String,
    pub output: Rgb,
    /// `None` on success, or when nothing was executed.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LightingResult {
    pub executed: bool,
    pub targets: Vec<TargetResult>,
    /// Set when hardware was written but the state for boot restore could not be saved.
    pub persist_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreEntry {
    pub alias: String,
    pub applied: bool,
    pub attempts: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FanState {
    Starting,
    Active {
        celsius: f64,
        percent: u8,
    },
    /// Control was returned to firmware; the curve stays off until the daemon restarts.
    Released {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanStatus {
    pub channel: u8,
    pub sensor: String,
    #[serde(flatten)]
    pub state: FanState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    /// Configuration used for boot restore, once lighting has been recorded.
    pub lighting_config: Option<PathBuf>,
    pub state_file: PathBuf,
    pub restore: Vec<RestoreEntry>,
    pub fans: Vec<FanStatus>,
}

fn read_line(reader: impl Read, limit: u64) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    BufReader::new(reader)
        .take(limit + 1)
        .read_until(b'\n', &mut line)?;
    if line.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message exceeds size limit",
        ));
    }
    if line.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incomplete message",
        ));
    }
    Ok(line)
}

fn write_line(mut writer: impl Write, value: &impl Serialize) -> io::Result<()> {
    let mut message = serde_json::to_vec(value).map_err(io::Error::other)?;
    message.push(b'\n');
    writer.write_all(&message)?;
    writer.flush()
}

/// Server side: read one bounded request.
pub fn read_request(reader: impl Read) -> io::Result<Request> {
    serde_json::from_slice(&read_line(reader, MAX_REQUEST_BYTES)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Server side: write one response.
pub fn write_response(writer: impl Write, response: &Response) -> io::Result<()> {
    write_line(writer, response)
}

/// Whether a daemon socket exists at `socket` (without connecting).
pub fn available(socket: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(socket).is_ok_and(|metadata| metadata.file_type().is_socket())
}

/// Client side: send `request` and decode the response data.
pub fn call<T: DeserializeOwned>(socket: &Path, request: &Request) -> io::Result<T> {
    let mut stream = UnixStream::connect(socket).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot connect to asusiod at {}: {error}", socket.display()),
        )
    })?;
    stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
    stream.set_write_timeout(Some(CLIENT_TIMEOUT))?;
    write_line(&mut stream, request)?;
    let response: Response = serde_json::from_slice(&read_line(&stream, MAX_RESPONSE_BYTES)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    match response {
        Response::Ok { data } => serde_json::from_value(data)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
        Response::Error { message } => Err(io::Error::other(format!("asusiod: {message}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wire_format_and_operations() {
        let request = Request::SetLighting {
            lighting_config: "/home/u/.config/asusio/lighting.json".into(),
            target: "all".into(),
            rgb: [1, 2, 3],
            brightness: 50,
            calibrated: true,
            execute: false,
        };
        let mut wire = Vec::new();
        write_line(&mut wire, &request).unwrap();
        assert!(wire.starts_with(br#"{"method":"set_lighting","lighting_config":"#));
        assert_eq!(read_request(wire.as_slice()).unwrap(), request);
        assert_eq!(request.operation(), Operation::PreviewLighting);
        assert_eq!(
            Request::RestoreLighting.operation(),
            Operation::RestoreLighting
        );
        assert!(
            read_request(br#"{"method":"status"}"#.as_slice()).is_err(),
            "no newline"
        );
        assert!(read_request(br#"{"method":"format_disk"}\n"#.as_slice()).is_err());
    }

    #[test]
    fn responses_carry_typed_data_or_errors() {
        let ok = Response::from_result(Ok(FanStatus {
            channel: 2,
            sensor: "k10temp/Tctl".into(),
            state: FanState::Active {
                celsius: 50.0,
                percent: 40,
            },
        }));
        let value = serde_json::to_value(&ok).unwrap();
        assert_eq!(value["status"], "ok");
        assert_eq!(value["data"]["state"], "active");
        let error = Response::from_result::<()>(Err(io::Error::other("boom")));
        assert_eq!(serde_json::to_value(&error).unwrap()["message"], "boom");
    }
}
