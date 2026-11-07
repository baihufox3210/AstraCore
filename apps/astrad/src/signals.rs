//! SIGTERM/SIGINT handling through an atomic stop flag.
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

#[allow(unsafe_code)]
pub fn install() -> io::Result<()> {
    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    for signal in [libc::SIGTERM, libc::SIGINT] {
        // SAFETY: the handler only performs an atomic store, which is async-signal-safe.
        if unsafe { libc::signal(signal, handler) } == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

pub fn stopping() -> bool {
    STOP.load(Ordering::SeqCst)
}

pub fn request_stop() {
    STOP.store(true, Ordering::SeqCst);
}

/// Sleep up to `duration`, waking early on shutdown. Returns `false` when stopping.
pub fn sleep(duration: Duration) -> bool {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if stopping() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100).min(deadline - Instant::now()));
    }
    !stopping()
}
