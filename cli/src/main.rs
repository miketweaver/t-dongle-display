//! tdongle: draw on a LilyGO T-Dongle-S3 running tdongle-display firmware.
//!
//! Zero dependencies: std plus five libc functions declared below. Linux only.

mod image;

use std::env;
use std::ffi::{c_int, c_ulong};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, ErrorKind, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const USB_VID: &str = "303a"; // Espressif
const USB_PID: &str = "1001"; // ESP32-S3 built-in USB-Serial/JTAG

// ---------------------------------------------------------------- libc ----
mod sys {
    use super::*;

    /// Opaque, oversized and aligned: we only hand it to libc, so its exact
    /// layout (which varies a little between libcs/arches) does not matter.
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

    extern "C" {
        pub fn tcgetattr(fd: c_int, t: *mut Termios) -> c_int;
        pub fn tcsetattr(fd: c_int, act: c_int, t: *const Termios) -> c_int;
        pub fn cfmakeraw(t: *mut Termios);
        pub fn tcflush(fd: c_int, q: c_int) -> c_int;
        pub fn poll(fds: *mut PollFd, n: c_ulong, timeout_ms: c_int) -> c_int;
    }
}

// -------------------------------------------------------------- serial ----
struct Port {
    f: File,
    buf: Vec<u8>,
}

impl Port {
    /// Opens the tty in raw mode. DTR/RTS are left exactly as the kernel sets
    /// them on open (both asserted). On the ESP32-S3 USB-Serial/JTAG port,
    /// DTR low + RTS high resets the chip, so we never touch those lines.
    fn open(path: &Path) -> io::Result<Port> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(sys::O_NOCTTY | sys::O_NONBLOCK)
            .open(path)?;
        let fd = f.as_raw_fd();
        unsafe {
            let mut t = sys::Termios([0; 256]);
            if sys::tcgetattr(fd, &mut t) != 0 {
                return Err(io::Error::last_os_error());
            }
            // Raw: no echo (echo would bounce the firmware's replies back to it
            // as new commands), no line editing, no CR/NL translation.
            sys::cfmakeraw(&mut t);
            if sys::tcsetattr(fd, sys::TCSANOW, &t) != 0 {
                return Err(io::Error::last_os_error());
            }
            sys::tcflush(fd, sys::TCIFLUSH);
        }
        Ok(Port { f, buf: Vec::new() })
    }

    /// Discards anything already queued (e.g. a ROM boot banner).
    fn drain(&mut self) {
        let mut tmp = [0u8; 512];
        while let Ok(n) = self.f.read(&mut tmp) {
            if n == 0 {
                break;
            }
        }
        self.buf.clear();
    }

    fn write_line(&mut self, line: &str) -> io::Result<()> {
        let mut data = line.as_bytes().to_vec();
        data.push(b'\n');
        self.write_bytes(&data)
    }

    fn write_bytes(&mut self, data: &[u8]) -> io::Result<()> {
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
    fn read_line(&mut self, timeout: Duration) -> io::Result<Option<String>> {
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
            let mut pfd = sys::PollFd { fd: self.f.as_raw_fd(), events: sys::POLLIN, revents: 0 };
            let r = unsafe { sys::poll(&mut pfd, 1, left.as_millis().min(i32::MAX as u128) as c_int) };
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
                Ok(0) => return Err(io::Error::new(ErrorKind::UnexpectedEof, "device disconnected")),
                Ok(n) => self.buf.extend_from_slice(&tmp[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Sends one command and returns its single-line reply.
    fn cmd(&mut self, line: &str, timeout: Duration) -> io::Result<Option<String>> {
        self.write_line(line)?;
        self.read_line(timeout)
    }
}

// ----------------------------------------------------------- discovery ----
struct Dongle {
    dev: PathBuf,
    serial: String,
}

fn read_attr(dir: &Path, name: &str) -> String {
    fs::read_to_string(dir.join(name)).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// All ttyACM devices with the ESP32-S3 USB-Serial/JTAG id, via sysfs.
fn find_dongles() -> Vec<Dongle> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir("/sys/class/tty") else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with("ttyACM") {
            continue;
        }
        // /sys/class/tty/ttyACM0/device -> USB interface dir; its parent is the USB device.
        let Ok(iface) = fs::canonicalize(e.path().join("device")) else { continue };
        let Some(usb) = iface.parent() else { continue };
        if read_attr(usb, "idVendor") == USB_VID && read_attr(usb, "idProduct") == USB_PID {
            out.push(Dongle { dev: PathBuf::from("/dev").join(&name), serial: read_attr(usb, "serial") });
        }
    }
    out.sort_by(|a, b| a.dev.cmp(&b.dev));
    out
}

fn probe(dev: &Path, timeout: Duration) -> Result<String, String> {
    let mut p = Port::open(dev).map_err(|e| describe_open_err(dev, &e))?;
    p.drain();
    match p.cmd("INFO", timeout) {
        Ok(Some(s)) if s.starts_with("OK tdongle-display") => Ok(s[3..].to_string()),
        Ok(Some(s)) => Err(format!("unexpected reply {s:?} (other firmware?)")),
        Ok(None) => Err("no reply (tdongle-display firmware not running?)".into()),
        Err(e) => Err(e.to_string()),
    }
}

fn describe_open_err(dev: &Path, e: &io::Error) -> String {
    match e.kind() {
        ErrorKind::PermissionDenied => format!(
            "{}: permission denied. Run setup.sh to install the udev rule, or add yourself to the group that owns it ({}).",
            dev.display(),
            group_hint(dev)
        ),
        ErrorKind::NotFound => format!("{}: no such device", dev.display()),
        _ => format!("{}: {e}", dev.display()),
    }
}

fn group_hint(dev: &Path) -> String {
    use std::os::unix::fs::MetadataExt;
    let Ok(md) = fs::metadata(dev) else { return "see ls -l".into() };
    let gid = md.gid();
    fs::read_to_string("/etc/group")
        .ok()
        .and_then(|g| {
            g.lines().find_map(|l| {
                let f: Vec<&str> = l.split(':').collect();
                (f.len() > 2 && f[2] == gid.to_string()).then(|| f[0].to_string())
            })
        })
        .unwrap_or_else(|| format!("gid {gid}"))
}

fn resolve_port(explicit: Option<&str>, timeout: Duration) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return Ok(PathBuf::from(p));
    }
    if let Ok(p) = env::var("TDONGLE_PORT") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    if Path::new("/dev/tdongle").exists() {
        return Ok(PathBuf::from("/dev/tdongle"));
    }
    let found = find_dongles();
    match found.len() {
        0 => Err("no T-Dongle found (no ttyACM device with USB id 303a:1001). Is it plugged in?".into()),
        1 => Ok(found.into_iter().next().unwrap().dev),
        _ => {
            for d in &found {
                if probe(&d.dev, timeout.min(Duration::from_millis(800))).is_ok() {
                    return Ok(d.dev.clone());
                }
            }
            let list: Vec<String> = found.iter().map(|d| d.dev.display().to_string()).collect();
            Err(format!("several 303a:1001 devices, none answering; pick one with --port: {}", list.join(", ")))
        }
    }
}

// ----------------------------------------------------------------- cli ----
const HELP: &str = "\
tdongle - draw on a LilyGO T-Dongle-S3 screen over USB serial

USAGE:
    tdongle [OPTIONS] <COMMAND> [ARGS...]
    tdongle [OPTIONS] <text...>            same as `msg <text...>`

COMMANDS:
    msg <text...>            Big, centered, word-wrapped message
    log <text...>            Append a line to a scrolling console
    text <x> <y> <size> <color> <text...>
    clear [color]            Fill the screen (default black)
    image <file|-> [opts]    Show an image (PNG, JPEG, BMP, PPM; others via ImageMagick/ffmpeg)
        --fit | --fill | --stretch   letterbox (default), crop to fill, or distort
        --rot auto|keep|0-3          screen rotation; auto = landscape for wide images
        --bg <color>                 letterbox color (default black)
        --no-dither                  plain RGB565 quantization
        --save <out.ppm>             write the converted image instead of sending it
    send <line>... | -       Raw protocol lines; `-` reads them from stdin
    info | ping | temp       Query the device
    sleep [secs]             Screen off now, or set the idle timeout (0 = never)
    wake                     Wake the screen
    bl <0-255>               Backlight brightness
    led <color> [0-31]       Onboard RGB LED
    list                     List connected dongles and their firmware

OPTIONS:
    -p, --port <dev>         Serial device (default: $TDONGLE_PORT, /dev/tdongle, autodetect)
    -t, --timeout <ms>       Per-command reply timeout (default 2000)
    -w, --wait <secs>        Wait up to <secs> for the dongle to appear and answer
    -q, --quiet              Only print errors
    -h, --help               This help
    -V, --version            Version

Colors: black white red green blue yellow cyan magenta orange purple gray pink
navy darkgreen brown, or RRGGBB. Full protocol: `tdongle send HELP`.

EXIT STATUS: 0 ok, 1 device replied ERR, 2 usage, 3 device/IO error, 4 no reply";

struct Opts {
    port: Option<String>,
    timeout: Duration,
    wait: Option<Duration>,
    quiet: bool,
    rest: Vec<String>,
}

fn parse_args() -> Result<Opts, String> {
    let mut o = Opts { port: None, timeout: Duration::from_millis(2000), wait: None, quiet: false, rest: vec![] };
    let mut it = env::args().skip(1);
    while let Some(a) = it.next() {
        if !o.rest.is_empty() {
            o.rest.push(a);
            continue;
        }
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "-h" | "--help" => {
                println!("{HELP}");
                std::process::exit(0)
            }
            "-V" | "--version" => {
                println!("tdongle {VERSION}");
                std::process::exit(0)
            }
            "-q" | "--quiet" => o.quiet = true,
            "-p" | "--port" => o.port = Some(val("--port")?),
            "-t" | "--timeout" => {
                o.timeout = Duration::from_millis(val("--timeout")?.parse().map_err(|_| "--timeout: not a number")?)
            }
            "-w" | "--wait" => {
                o.wait = Some(Duration::from_secs_f64(val("--wait")?.parse().map_err(|_| "--wait: not a number")?))
            }
            "--" => o.rest.push(it.next().unwrap_or_default()),
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option {s}")),
            _ => o.rest.push(a),
        }
    }
    Ok(o)
}

/// Turns the subcommand into protocol lines.
fn build_lines(rest: &[String]) -> Result<Vec<String>, String> {
    let Some((cmd, args)) = rest.split_first() else { return Err("missing command (try --help)".into()) };
    let joined = args.join(" ");
    let need = |n: usize, usage: &str| {
        if args.len() < n {
            Err(format!("usage: tdongle {usage}"))
        } else {
            Ok(())
        }
    };
    let lines = match cmd.to_ascii_lowercase().as_str() {
        "msg" | "message" => {
            need(1, "msg <text...>")?;
            vec![format!("MSG {joined}")]
        }
        "log" => vec![format!("LOG {joined}")],
        "text" => {
            need(5, "text <x> <y> <size> <color> <text...>")?;
            vec![format!("TEXT {joined}")]
        }
        "clear" => vec![format!("CLEAR {joined}").trim_end().to_string()],
        "info" => vec!["INFO".into()],
        "ping" => vec!["PING".into()],
        "temp" => vec!["TEMP".into()],
        "wake" => vec!["WAKE".into()],
        "sleep" => vec![format!("SLEEP {joined}").trim_end().to_string()],
        "bl" | "brightness" => {
            need(1, "bl <0-255>")?;
            vec![format!("BL {joined}")]
        }
        "led" => {
            need(1, "led <color> [0-31]")?;
            vec![format!("LED {joined}")]
        }
        "send" | "raw" => {
            need(1, "send <line>... | -")?;
            if args.len() == 1 && args[0] == "-" {
                io::stdin()
                    .lock()
                    .lines()
                    .map_while(Result::ok)
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            } else {
                args.to_vec()
            }
        }
        // Anything else: show the whole argument list as a message.
        _ => vec![format!("MSG {}", rest.join(" "))],
    };
    for l in &lines {
        if l.contains('\n') || l.contains('\r') {
            return Err("command text must not contain newlines".into());
        }
    }
    Ok(lines)
}

// --------------------------------------------------------------- image ----
struct ImageOpts {
    path: String,
    fit: image::Fit,
    rot: Option<u8>, // None = keep current
    bg: [u8; 3],
    dither: bool,
    save: Option<String>,
}

fn parse_color(s: &str) -> Option<[u8; 3]> {
    const NAMED: &[(&str, u32)] = &[
        ("black", 0x000000), ("white", 0xFFFFFF), ("red", 0xFF0000), ("green", 0x00FF00), ("blue", 0x0000FF),
        ("yellow", 0xFFFF00), ("cyan", 0x00FFFF), ("magenta", 0xFF00FF), ("orange", 0xFF8000), ("purple", 0x8000FF),
        ("gray", 0x808080), ("grey", 0x808080), ("pink", 0xFF60A0), ("navy", 0x000080), ("darkgreen", 0x006400),
        ("brown", 0x8B4513),
    ];
    let t = s.trim().to_ascii_lowercase();
    let v = match NAMED.iter().find(|(n, _)| *n == t) {
        Some((_, v)) => *v,
        None => {
            let h = t.trim_start_matches('#').trim_start_matches("0x");
            if h.len() != 6 {
                return None;
            }
            u32::from_str_radix(h, 16).ok()?
        }
    };
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn parse_image_opts(args: &[String]) -> Result<ImageOpts, String> {
    let usage = "usage: tdongle image <file|-> [--fit|--fill|--stretch] [--rot auto|keep|0-3] [--bg color] [--no-dither] [--save out.ppm]";
    let mut o = ImageOpts { path: String::new(), fit: image::Fit::Contain, rot: Some(255), bg: [0; 3], dither: true, save: None };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--fit" => o.fit = image::Fit::Contain,
            "--fill" => o.fit = image::Fit::Cover,
            "--stretch" => o.fit = image::Fit::Stretch,
            "--no-dither" => o.dither = false,
            "--rot" => {
                o.rot = match it.next().map(|s| s.as_str()) {
                    Some("auto") => Some(255),
                    Some("keep") => None,
                    Some(n @ ("0" | "1" | "2" | "3")) => Some(n.parse().unwrap()),
                    _ => return Err("--rot: auto, keep or 0-3".into()),
                }
            }
            "--bg" => o.bg = it.next().and_then(|c| parse_color(c)).ok_or("--bg: color name or RRGGBB")?,
            "--save" => o.save = Some(it.next().ok_or("--save needs a file name")?.clone()),
            s if s.starts_with("--") => return Err(format!("unknown image option {s}\n{usage}")),
            _ if o.path.is_empty() => o.path = a.clone(),
            _ => return Err(usage.into()),
        }
    }
    if o.path.is_empty() {
        return Err(usage.into());
    }
    Ok(o)
}

/// Screen size for a rotation of the 80x160 panel.
fn dims_for_rot(rot: u8) -> (usize, usize) {
    if rot % 2 == 0 {
        (80, 160)
    } else {
        (160, 80)
    }
}

/// "OK tdongle-display 1.1.0 80x160 rot=0 ..." -> rotation
fn parse_rot(info: &str) -> Option<u8> {
    info.split_whitespace().find_map(|t| t.strip_prefix("rot=")?.parse().ok())
}

/// Loads and converts the image; with a port, rotates the screen and pushes it.
fn cmd_image(o: &ImageOpts, port: Option<&mut Port>, timeout: Duration) -> Result<u8, (u8, String)> {
    let data = image::read_input(&o.path).map_err(|e| (3, e))?;
    let img = image::load(&o.path, &data).map_err(|e| (3, e))?;
    let auto_rot = if img.w > img.h { 1 } else { 0 };
    let Some(port) = port else {
        // --save: no device needed
        let rot = match o.rot {
            Some(255) => auto_rot,
            Some(r) => r,
            None => 0,
        };
        let (w, h) = dims_for_rot(rot);
        let px = image::to_rgb565_be(image::render(&img, w, h, o.fit, o.bg), w, h, o.dither);
        let path = o.save.as_deref().unwrap();
        fs::write(path, image::rgb565_to_ppm(&px, w, h)).map_err(|e| (3, format!("{path}: {e}")))?;
        println!("saved {w}x{h} preview to {path} (from {}x{})", img.w, img.h);
        return Ok(0);
    };
    let io_err = |e: io::Error| (3, e.to_string());
    // Older firmware would draw "IMG ..." as text and then treat the pixels as commands.
    let info = port.cmd("INFO", timeout).map_err(io_err)?.ok_or((4, "INFO: no reply".to_string()))?;
    let fw = info.split_whitespace().nth(2).unwrap_or("");
    let ver: Vec<u32> = fw.split('.').filter_map(|p| p.parse().ok()).collect();
    if ver.len() != 3 || (ver[0], ver[1]) < (1, 1) {
        return Err((3, format!("images need firmware 1.1.0 or newer (dongle has {fw:?}); rerun setup.sh to update")));
    }
    let rot = match o.rot {
        None => parse_rot(&info).ok_or((3, format!("unexpected INFO reply {info:?}")))?,
        Some(r) => {
            let r = if r == 255 { auto_rot } else { r };
            match port.cmd(&format!("ROT {r}"), timeout).map_err(io_err)? {
                Some(rep) if rep.starts_with("OK") => r,
                Some(rep) => return Err((1, format!("ROT {r}: {rep}"))),
                None => return Err((4, "ROT: no reply".into())),
            }
        }
    };
    let (w, h) = dims_for_rot(rot);
    let px = image::to_rgb565_be(image::render(&img, w, h, o.fit, o.bg), w, h, o.dither);
    // Chunked with acks: the device's USB driver drops data instead of throttling us.
    let chunk = match port.cmd(&format!("IMG 0 0 {w} {h}"), timeout).map_err(io_err)? {
        Some(r) if r.starts_with("READY ") => r[6..].trim().parse::<usize>().unwrap_or(0).max(64),
        Some(r) => return Err((1, format!("IMG: {r}"))),
        None => return Err((4, "IMG: no reply".into())),
    };
    let parts: Vec<&[u8]> = px.chunks(chunk).collect();
    for (i, part) in parts.iter().enumerate() {
        port.write_bytes(part).map_err(io_err)?;
        let last = i + 1 == parts.len();
        match port.read_line(timeout.max(Duration::from_secs(3))).map_err(io_err)? {
            Some(r) if !last && r == "NEXT" => {}
            Some(r) if last && r.starts_with("OK") => return Ok(0),
            Some(r) => return Err((1, format!("IMG: {r}"))),
            None => return Err((4, "IMG: no reply".into())),
        }
    }
    Ok(0)
}

fn cmd_list(timeout: Duration) -> u8 {
    let found = find_dongles();
    if found.is_empty() {
        eprintln!("no T-Dongle found (no ttyACM device with USB id 303a:1001)");
        return 3;
    }
    for d in found {
        let status = match probe(&d.dev, timeout) {
            Ok(info) => info,
            Err(e) => format!("? {e}"),
        };
        println!("{}  serial={}  {}", d.dev.display(), d.serial, status);
    }
    0
}

fn run() -> Result<u8, (u8, String)> {
    let o = parse_args().map_err(|e| (2, e))?;
    if o.rest.first().map(|s| s.as_str()) == Some("list") {
        return Ok(cmd_list(o.timeout));
    }
    let is_image = matches!(o.rest.first().map(|s| s.as_str()), Some("image" | "img"));
    let img_opts = if is_image { Some(parse_image_opts(&o.rest[1..]).map_err(|e| (2, e))?) } else { None };
    if let Some(io) = img_opts.as_ref().filter(|io| io.save.is_some()) {
        return cmd_image(io, None, o.timeout);
    }
    let lines = if is_image { vec![] } else { build_lines(&o.rest).map_err(|e| (2, e))? };

    // Resolve + open, retrying while --wait is in effect (e.g. right after a flash or plug-in).
    let wait_until = o.wait.map(|w| Instant::now() + w);
    let mut port = loop {
        let attempt = resolve_port(o.port.as_deref(), o.timeout).and_then(|dev| {
            let mut p = Port::open(&dev).map_err(|e| describe_open_err(&dev, &e))?;
            p.drain();
            if wait_until.is_some() {
                match p.cmd("PING", Duration::from_millis(500)) {
                    Ok(Some(r)) if r == "PONG" => {
                        // Earlier PINGs sent while the firmware was still booting may
                        // still be answered; swallow those late PONGs.
                        std::thread::sleep(Duration::from_millis(300));
                        p.drain();
                    }
                    _ => return Err(format!("{}: not answering yet", dev.display())),
                }
            }
            Ok(p)
        });
        match attempt {
            Ok(p) => break p,
            Err(e) => match wait_until {
                Some(t) if Instant::now() < t => std::thread::sleep(Duration::from_millis(250)),
                _ => return Err((3, e)),
            },
        }
    };

    if let Some(io) = &img_opts {
        return cmd_image(io, Some(&mut port), o.timeout);
    }

    let mut rc = 0u8;
    for l in &lines {
        match port.cmd(l, o.timeout) {
            Ok(Some(reply)) => {
                if reply.starts_with("ERR") {
                    eprintln!("{l}: {reply}");
                    rc = 1;
                } else if !o.quiet {
                    println!("{reply}");
                }
            }
            Ok(None) => {
                eprintln!("{l}: no reply within {} ms (is tdongle-display firmware running?)", o.timeout.as_millis());
                rc = rc.max(4);
            }
            Err(e) => return Err((3, format!("{l}: {e}"))),
        }
    }
    Ok(rc)
}

fn main() -> ExitCode {
    match run() {
        Ok(rc) => ExitCode::from(rc),
        Err((rc, msg)) => {
            eprintln!("tdongle: {msg}");
            ExitCode::from(rc)
        }
    }
}
