//! Unix socket server. The socket mode is the first access gate; the policy is
//! checked again per request using the kernel-reported peer credentials.
use crate::{daemon::Daemon, signals};
use asusio_core::{
    lighting::Request as LightingRequest,
    service::ipc::{self, Request, Response},
};
use asusio_policy::Caller;
use std::{
    fs, io,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::Path,
    sync::Arc,
    thread,
    time::Duration,
};

/// Access the socket: everyone (per-request policy decides) when active sessions are
/// allowed; otherwise root plus the client group, or the daemon user only.
fn bind(path: &Path, group: Option<u32>, open: bool) -> io::Result<UnixListener> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(path)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    // The default umask leaves the new socket owner-writable only until the mode below.
    let listener = UnixListener::bind(path)?;
    if let Some(gid) = group {
        std::os::unix::fs::chown(path, None, Some(gid))?;
    }
    let mode = match (open, group) {
        (true, _) => 0o666,
        (false, Some(_)) => 0o660,
        (false, None) => 0o600,
    };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(listener)
}

#[allow(unsafe_code)]
fn peer(stream: &UnixStream) -> io::Result<Caller> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: SO_PEERCRED fills a ucred of the given length on a valid socket fd.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // Supplementary groups of the connecting process.
    let mut groups = vec![credentials.gid];
    if let Ok(status) = fs::read_to_string(format!("/proc/{}/status", credentials.pid)) {
        groups.extend(
            status
                .lines()
                .find_map(|line| line.strip_prefix("Groups:"))
                .into_iter()
                .flat_map(str::split_whitespace)
                .filter_map(|gid| gid.parse::<u32>().ok()),
        );
    }
    Ok(Caller {
        uid: credentials.uid,
        groups,
        active_session: has_active_session(credentials.uid),
    })
}

/// systemd-logind records `STATE=active` for users with a session in the foreground.
fn has_active_session(uid: u32) -> bool {
    fs::read_to_string(format!("/run/systemd/users/{uid}"))
        .is_ok_and(|text| text.lines().any(|line| line == "STATE=active"))
}

fn dispatch(daemon: &Daemon, caller: &Caller, request: Request) -> Response {
    match request {
        Request::Status => Response::from_result(Ok(daemon.status())),
        Request::SetLighting {
            lighting_config,
            target,
            rgb,
            brightness,
            calibrated,
            execute,
        } => {
            let request = LightingRequest::new(&target, rgb)
                .brightness(brightness)
                .calibrated(calibrated);
            Response::from_result(daemon.set_lighting(
                lighting_config,
                caller.uid,
                &request,
                execute,
            ))
        }
        Request::RestoreLighting => Response::from_result(Ok(daemon.restore_now())),
    }
}

fn handle(daemon: &Daemon, stream: UnixStream) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let caller = peer(&stream)?;
    let response = match ipc::read_request(&stream) {
        Err(error) => Response::Error {
            message: format!("invalid request: {error}"),
        },
        Ok(request) => match daemon.policy.authorize(&caller, request.operation()) {
            Err(denied) => {
                eprintln!("denied: {denied}");
                Response::Error {
                    message: denied.to_string(),
                }
            }
            Ok(()) => {
                if request.operation().writes_hardware() {
                    eprintln!("uid {}: {request:?}", caller.uid);
                }
                dispatch(daemon, &caller, request)
            }
        },
    };
    ipc::write_response(&stream, &response)
}

pub fn serve(daemon: &Arc<Daemon>) -> io::Result<()> {
    let path = &daemon.config.socket;
    let listener = bind(path, daemon.client_gid, daemon.config.allow_active_sessions)?;
    listener.set_nonblocking(true)?;
    eprintln!("listening on {}", path.display());
    while !signals::stopping() {
        match listener.accept() {
            Ok((stream, _)) => {
                let daemon = daemon.clone();
                thread::spawn(move || {
                    if let Err(error) = handle(&daemon, stream) {
                        eprintln!("client error: {error}");
                    }
                });
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => eprintln!("accept failed: {error}"),
        }
    }
    let _ = fs::remove_file(path);
    Ok(())
}
