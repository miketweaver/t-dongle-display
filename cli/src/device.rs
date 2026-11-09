//! Talking to the dongle: finding it, the raw serial port, and the line protocol.

use crate::sys;
use std::ffi::c_int;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const USB_VID: &str = "303a"; // Espressif
pub const USB_PID: &str = "1001"; // ESP32-S3 built-in USB-Serial/JTAG
pub const PANEL: (usize, usize) = (80, 160); // portrait width, height

// -------------------------------------------------------------- serial ----
pub struct Port {
    f: File,
    buf: Vec<u8>,
    pub path: PathBuf,
}

impl Port {
    /// Opens the tty in raw mode. DTR/RTS are left exactly as the kernel sets
    /// them on open (both asserted). On the ESP32-S3 USB-Serial/JTAG port,
    /// DTR low + RTS high resets the chip, so we never touch those lines.
    pub fn open(path: &Path) -> Result<Port, String> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(sys::O_NOCTTY | sys::O_NONBLOCK)
            .open(path)
            .map_err(|e| describe_open_err(path, &e))?;
        let fd = f.as_raw_fd();
        unsafe {
            let mut t = sys::Termios([0; 256]);
            if sys::tcgetattr(fd, &mut t) != 0 {
                return Err(format!(
                    "{}: not a serial port ({})",
                    path.display(),
                    io::Error::last_os_error()
                ));
            }
            // Raw: no echo (echo would bounce the firmware's replies back to it
            // as new commands), no line editing, no CR/NL translation.
            sys::cfmakeraw(&mut t);
            if sys::tcsetattr(fd, sys::TCSANOW, &t) != 0 {
                return Err(format!(
                    "{}: {}",
                    path.display(),
                    io::Error::last_os_error()
                ));
            }
            sys::tcflush(fd, sys::TCIFLUSH);
        }
        Ok(Port {
            f,
            buf: Vec::new(),
            path: path.to_path_buf(),
        })
    }

    /// Discards anything already queued (e.g. a ROM boot banner).
    pub fn drain(&mut self) {
        let mut tmp = [0u8; 512];
        while let Ok(n) = self.f.read(&mut tmp) {
            if n == 0 {
                break;
            }
        }
        self.buf.clear();
    }

    pub fn write_line(&mut self, line: &str) -> io::Result<()> {
        let mut data = line.as_bytes().to_vec();
        data.push(b'\n');
        self.write_bytes(&data)
    }

    pub fn write_bytes(&mut self, data: &[u8]) -> io::Result<()> {
        let mut off = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while off < data.len() {
            match self.f.write(&data[off..]) {
                Ok(n) => off += n,
                Err(e) if e.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Next non-empty line, or None on timeout.
    pub fn read_line(&mut self, timeout: Duration) -> io::Result<Option<String>> {
        let deadline = Instant::now() + timeout;
        loop {
            while let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
                let raw: Vec<u8> = self.buf.drain(..=i).collect();
                let s = String::from_utf8_lossy(&raw).trim().to_string();
                if !s.is_empty() {
                    return Ok(Some(s));
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            let mut pfd = sys::PollFd {
                fd: self.f.as_raw_fd(),
                events: sys::POLLIN,
                revents: 0,
            };
            let ms = left.as_millis().min(i32::MAX as u128) as c_int;
            let r = unsafe { sys::poll(&mut pfd, 1, ms) };
            if r < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if r == 0 {
                return Ok(None);
            }
            let mut tmp = [0u8; 512];
            match self.f.read(&mut tmp) {
                Ok(0) => {
                    return Err(io::Error::new(
                        ErrorKind::UnexpectedEof,
                        "device disconnected",
                    ))
                }
                Ok(n) => self.buf.extend_from_slice(&tmp[..n]),
                Err(e)
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Sends one command and returns its single-line reply.
    pub fn cmd(&mut self, line: &str, timeout: Duration) -> Result<String, String> {
        self.write_line(line).map_err(|e| self.io_err(e))?;
        match self.read_line(timeout).map_err(|e| self.io_err(e))? {
            Some(r) => Ok(r),
            None => Err(format!(
                "{}: no reply to `{}` within {} ms (is the tdongle-display firmware running? rerun setup.sh)",
                self.path.display(),
                first_word(line),
                timeout.as_millis()
            )),
        }
    }

    /// Like `cmd`, but an `ERR` reply is an error too.
    pub fn cmd_ok(&mut self, line: &str, timeout: Duration) -> Result<String, String> {
        let r = self.cmd(line, timeout)?;
        match r.strip_prefix("ERR") {
            Some(why) => Err(format!(
                "the dongle rejected `{}`: {}",
                short(line),
                why.trim()
            )),
            None => Ok(r),
        }
    }

    pub fn io_err(&self, e: io::Error) -> String {
        format!("{}: {e}", self.path.display())
    }
}

fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or(s)
}

fn short(s: &str) -> String {
    if s.chars().count() > 40 {
        format!("{}…", s.chars().take(40).collect::<String>())
    } else {
        s.to_string()
    }
}

// ----------------------------------------------------------- discovery ----
pub struct Dongle {
    pub dev: PathBuf,
    pub serial: String,
}

fn read_attr(dir: &Path, name: &str) -> String {
    fs::read_to_string(dir.join(name))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// All ttyACM devices with the ESP32-S3 USB-Serial/JTAG id, via sysfs.
pub fn find_dongles() -> Vec<Dongle> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir("/sys/class/tty") else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with("ttyACM") {
            continue;
        }
        // /sys/class/tty/ttyACM0/device -> USB interface dir; its parent is the USB device.
        let Ok(iface) = fs::canonicalize(e.path().join("device")) else {
            continue;
        };
        let Some(usb) = iface.parent() else { continue };
        if read_attr(usb, "idVendor") == USB_VID && read_attr(usb, "idProduct") == USB_PID {
            out.push(Dongle {
                dev: PathBuf::from("/dev").join(&name),
                serial: read_attr(usb, "serial"),
            });
        }
    }
    out.sort_by(|a, b| a.dev.cmp(&b.dev));
    out
}

/// USB serial number for a device node (follows /dev/tdongle style symlinks).
pub fn serial_of(dev: &Path) -> Option<String> {
    let real = fs::canonicalize(dev).ok()?;
    let name = real.file_name()?.to_string_lossy().to_string();
    find_dongles()
        .into_iter()
        .find(|d| {
            d.dev
                .file_name()
                .map(|n| n.to_string_lossy() == name)
                .unwrap_or(false)
        })
        .map(|d| d.serial)
}

fn describe_open_err(dev: &Path, e: &io::Error) -> String {
    match e.kind() {
        ErrorKind::PermissionDenied => format!(
            "{}: permission denied; run setup.sh to install the udev rule, or join the group that owns it ({})",
            dev.display(),
            group_of(dev)
        ),
        ErrorKind::NotFound => format!("{}: no such device", dev.display()),
        _ => format!("{}: {e}", dev.display()),
    }
}

fn group_of(dev: &Path) -> String {
    use std::os::unix::fs::MetadataExt;
    let Ok(md) = fs::metadata(dev) else {
        return "see ls -l".into();
    };
    let gid = md.gid().to_string();
    fs::read_to_string("/etc/group")
        .ok()
        .and_then(|g| {
            g.lines().find_map(|l| {
                let f: Vec<&str> = l.split(':').collect();
                (f.len() > 2 && f[2] == gid).then(|| f[0].to_string())
            })
        })
        .unwrap_or_else(|| format!("gid {gid}"))
}

/// Where the port comes from, in precedence order. Pure: the caller supplies
/// the environment so `--skill` can describe it without touching devices.
#[derive(Clone, Debug, PartialEq)]
pub enum PortSource {
    Flag(String),
    Env(String),
    Udev,
    Autodetect,
}

pub fn port_source(flag: Option<&str>, env: Option<&str>, udev_link: bool) -> PortSource {
    if let Some(p) = flag {
        return PortSource::Flag(p.to_string());
    }
    if let Some(p) = env.filter(|p| !p.is_empty()) {
        return PortSource::Env(p.to_string());
    }
    if udev_link {
        PortSource::Udev
    } else {
        PortSource::Autodetect
    }
}

pub fn resolve_port(src: &PortSource, timeout: Duration) -> Result<PathBuf, String> {
    match src {
        PortSource::Flag(p) | PortSource::Env(p) => Ok(PathBuf::from(p)),
        PortSource::Udev => Ok(PathBuf::from("/dev/tdongle")),
        PortSource::Autodetect => {
            let found = find_dongles();
            match found.len() {
                0 => Err(
                    "no T-Dongle found (no ttyACM device with USB id 303a:1001); is it plugged in?"
                        .into(),
                ),
                1 => Ok(found.into_iter().next().unwrap().dev),
                _ => {
                    for d in &found {
                        if probe(&d.dev, timeout.min(Duration::from_millis(800))).is_ok() {
                            return Ok(d.dev.clone());
                        }
                    }
                    let list: Vec<String> =
                        found.iter().map(|d| d.dev.display().to_string()).collect();
                    Err(format!(
                        "several 303a:1001 devices and none answering; pick one with --port: {}",
                        list.join(", ")
                    ))
                }
            }
        }
    }
}

/// Opens `dev` and reads INFO.
pub fn probe(dev: &Path, timeout: Duration) -> Result<Info, String> {
    let mut p = Port::open(dev)?;
    p.drain();
    let r = p.cmd("INFO", timeout)?;
    Info::parse(&r).ok_or_else(|| format!("unexpected reply {r:?} (not tdongle-display firmware?)"))
}

// ---------------------------------------------------------------- info ----
/// Parsed `INFO` reply: `OK tdongle-display 1.1.0 80x160 rot=0 bl=160 sleep=300s asleep=0`.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub firmware: String,
    pub version: String,
    pub width: usize,
    pub height: usize,
    pub rotation: u8,
    pub brightness: Option<u8>,
    pub sleep_after_s: Option<u32>,
    pub asleep: Option<bool>,
}

impl Info {
    pub fn parse(reply: &str) -> Option<Info> {
        let mut t = reply.split_whitespace();
        if t.next()? != "OK" {
            return None;
        }
        let firmware = t.next()?.to_string();
        if firmware != "tdongle-display" {
            return None;
        }
        let version = t.next()?.to_string();
        let (w, h) = t.next()?.split_once('x')?;
        let mut info = Info {
            firmware,
            version,
            width: w.parse().ok()?,
            height: h.parse().ok()?,
            rotation: 0,
            brightness: None,
            sleep_after_s: None,
            asleep: None,
        };
        for kv in t {
            match kv.split_once('=') {
                Some(("rot", v)) => info.rotation = v.parse().ok()?,
                Some(("bl", v)) => info.brightness = v.parse().ok(),
                Some(("sleep", v)) => info.sleep_after_s = v.trim_end_matches('s').parse().ok(),
                Some(("asleep", v)) => info.asleep = Some(v == "1"),
                _ => {}
            }
        }
        Some(info)
    }

    /// True for firmware >= major.minor. Non-numeric versions ("dev") are local builds: newest.
    pub fn at_least(&self, major: u32, minor: u32) -> bool {
        let v: Vec<u32> = self
            .version
            .split('.')
            .filter_map(|p| p.parse().ok())
            .collect();
        if v.len() < 2 {
            return true;
        }
        (v[0], v[1]) >= (major, minor)
    }
}

/// `OK 35.0C cpu=80MHz` -> (35.0, 80)
pub fn parse_temp(reply: &str) -> Option<(f64, u32)> {
    let mut t = reply.split_whitespace();
    if t.next()? != "OK" {
        return None;
    }
    let c = t.next()?.trim_end_matches('C').parse().ok()?;
    let mhz = t
        .find_map(|kv| {
            kv.strip_prefix("cpu=")?
                .trim_end_matches("MHz")
                .parse()
                .ok()
        })
        .unwrap_or(0);
    Some((c, mhz))
}

pub fn dims_for_rot(rot: u8) -> (usize, usize) {
    if rot.is_multiple_of(2) {
        PANEL
    } else {
        (PANEL.1, PANEL.0)
    }
}

pub fn orientation(rot: u8) -> &'static str {
    match rot % 4 {
        0 => "portrait",
        1 => "landscape",
        2 => "portrait (flipped)",
        _ => "landscape (flipped)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_reply_parses_every_field() {
        let i = Info::parse("OK tdongle-display 1.1.0 80x160 rot=1 bl=160 sleep=300s asleep=0")
            .unwrap();
        assert_eq!((i.width, i.height, i.rotation), (80, 160, 1));
        assert_eq!(
            (i.brightness, i.sleep_after_s, i.asleep),
            (Some(160), Some(300), Some(false))
        );
        assert!(i.at_least(1, 1) && !i.at_least(1, 2));
    }

    #[test]
    fn other_firmware_is_not_mistaken_for_ours() {
        assert!(Info::parse("OK something-else 1.0 80x160").is_none());
        assert!(Info::parse("PONG").is_none());
    }

    #[test]
    fn port_precedence_is_flag_then_env_then_udev_then_autodetect() {
        assert_eq!(
            port_source(Some("/a"), Some("/b"), true),
            PortSource::Flag("/a".into())
        );
        assert_eq!(
            port_source(None, Some("/b"), true),
            PortSource::Env("/b".into())
        );
        assert_eq!(port_source(None, Some(""), true), PortSource::Udev);
        assert_eq!(port_source(None, None, false), PortSource::Autodetect);
    }
}
