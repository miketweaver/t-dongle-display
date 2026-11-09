//! The few libc functions we need, declared by hand (no `libc` crate).

use std::ffi::{c_int, c_ulong};

/// Opaque, oversized and aligned: we only hand it to libc, so its exact
/// layout (which varies a little between libcs and architectures) does not matter.
#[repr(C, align(8))]
pub struct Termios(pub [u8; 256]);

#[repr(C)]
pub struct PollFd {
    pub fd: c_int,
    pub events: i16,
    pub revents: i16,
}

pub const TCSANOW: c_int = 0;
pub const TCIFLUSH: c_int = 0;
pub const POLLIN: i16 = 0x1;
pub const O_NOCTTY: i32 = 0o400;
pub const O_NONBLOCK: i32 = 0o4000;
const SIGPIPE: c_int = 13;
const SIG_DFL: usize = 0;

extern "C" {
    pub fn tcgetattr(fd: c_int, t: *mut Termios) -> c_int;
    pub fn tcsetattr(fd: c_int, act: c_int, t: *const Termios) -> c_int;
    pub fn cfmakeraw(t: *mut Termios);
    pub fn tcflush(fd: c_int, q: c_int) -> c_int;
    pub fn poll(fds: *mut PollFd, n: c_ulong, timeout_ms: c_int) -> c_int;
    fn isatty(fd: c_int) -> c_int;
    fn signal(signum: c_int, handler: usize) -> usize;
}

pub fn stdout_is_tty() -> bool {
    unsafe { isatty(1) == 1 }
}

/// Rust ignores SIGPIPE, which turns `tdongle list | head -0` into a panic.
/// Restoring the default makes it end quietly with status 141 like other tools.
pub fn default_sigpipe() {
    unsafe {
        signal(SIGPIPE, SIG_DFL);
    }
}

/// Current UTC time as RFC 3339 (`2026-11-09T05:40:00Z`), without libc time functions.
pub fn utc_now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's days_from_civil, inverted.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
