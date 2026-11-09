//! Argument parsing (table-driven from `spec`), help, and dispatch.

use crate::device::{self, Info, Port};
use crate::image;
use crate::model::{Drawn, ListEntry, Outcome, Ping, Sent, Shown, Status, Temp};
use crate::render::{self, ColorMode, Style};
use crate::spec::{self, Command, Flag, Group, COMMANDS, EXAMPLES, EXIT_CODES, FLAGS};
use crate::{skill, sys};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::time::{Duration, Instant};

/// A failure, with the exit code it maps to.
#[derive(Debug, PartialEq)]
pub enum Fail {
    Usage(String),
    Runtime(String),
}

impl Fail {
    pub fn code(&self) -> i32 {
        match self {
            Fail::Usage(_) => 2,
            Fail::Runtime(_) => 1,
        }
    }
}

type R<T> = Result<T, Fail>;

fn usage<T>(m: impl Into<String>) -> R<T> {
    Err(Fail::Usage(m.into()))
}

// ------------------------------------------------------------------ parse ----
#[derive(Debug)]
pub enum Action {
    Version,
    Skill,
    Help(Option<&'static Command>),
    Run(Plan),
}

#[derive(Debug)]
pub struct Plan {
    pub cmd: &'static Command,
    pub args: Vec<String>,
    pub port: Option<String>,
    pub wait: Option<Duration>,
    pub timeout: Duration,
    pub json: bool,
    pub quiet: bool,
    pub color: ColorMode,
    pub image: ImageOpts,
}

#[derive(Debug)]
pub struct ImageOpts {
    pub fit: image::Fit,
    pub rot: Option<u8>, // None = keep; Some(255) = auto
    pub bg: [u8; 3],
    pub dither: bool,
    pub save: Option<String>,
}

fn is_negative_number(a: &str) -> bool {
    a.len() > 1 && a.starts_with('-') && a[1..].chars().next().is_some_and(|c| c.is_ascii_digit())
}

/// Splits argv into flags (validated against `FLAGS`) and positional words.
/// Flag long name -> its value (None for switches).
type Flags = HashMap<&'static str, Option<String>>;

fn split_args(args: &[String]) -> R<(Flags, Vec<String>)> {
    let mut flags: Flags = HashMap::new();
    let mut pos = Vec::new();
    let mut it = args.iter().peekable();
    let mut rest = false;
    while let Some(a) = it.next() {
        if rest || a == "-" || !a.starts_with('-') || is_negative_number(a) {
            pos.push(a.clone());
            continue;
        }
        if a == "--" {
            rest = true;
            continue;
        }
        let (flag, inline): (&Flag, Option<String>) = if let Some(long) = a.strip_prefix("--") {
            let (name, val) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            match spec::find_flag_long(name) {
                Some(f) => (f, val),
                None => return usage(format!("unknown flag --{name} (see tdongle --help)")),
            }
        } else {
            let mut cs = a[1..].chars();
            match (cs.next(), cs.next()) {
                (Some(c), None) => match spec::find_flag_short(c) {
                    Some(f) => (f, None),
                    None => return usage(format!("unknown flag {a} (see tdongle --help)")),
                },
                _ => {
                    return usage(format!(
                        "unknown flag {a} (one letter per short flag; see tdongle --help)"
                    ))
                }
            }
        };
        let value = match (flag.value, inline) {
            (Some(_), Some(v)) => Some(v),
            (Some(meta), None) => match it.next() {
                Some(v) => Some(v.clone()),
                None => return usage(format!("--{} needs a value ({meta})", flag.long)),
            },
            (None, Some(_)) => return usage(format!("--{} takes no value", flag.long)),
            (None, None) => None,
        };
        flags.insert(flag.long, value);
    }
    Ok((flags, pos))
}

fn num<T: std::str::FromStr>(what: &str, v: &str) -> R<T> {
    v.parse()
        .map_err(|_| Fail::Usage(format!("{what}: not a number: {v:?}")))
}

fn ranged(what: &str, v: &str, lo: i64, hi: i64) -> R<i64> {
    let n: i64 = num(what, v)?;
    if n < lo || n > hi {
        return usage(format!("{what}: {n} is out of range ({lo}-{hi})"));
    }
    Ok(n)
}

fn color(what: &str, v: &str) -> R<[u8; 3]> {
    spec::parse_color(v).ok_or_else(|| {
        Fail::Usage(format!(
            "{what}: bad color {v:?} (a name like red or navy, or RRGGBB)"
        ))
    })
}

pub fn parse(args: &[String]) -> R<Action> {
    let (flags, pos) = split_args(args)?;
    let has = |n: &str| flags.contains_key(n);
    let val = |n: &str| flags.get(n).cloned().flatten();

    if has("version") {
        return Ok(Action::Version);
    }
    if has("skill") {
        // Read-only path: other flags are ignored, nothing is opened or written.
        return Ok(Action::Skill);
    }

    let (cmd, cargs, explicit) = match pos.first().and_then(|w| spec::find_command(w)) {
        Some(c) => (c, pos[1..].to_vec(), true),
        None if pos.is_empty() => (spec::find_command("status").unwrap(), vec![], false),
        None => (spec::find_command("msg").unwrap(), pos.clone(), false),
    };
    if has("help") {
        return Ok(Action::Help(explicit.then_some(cmd)));
    }

    for f in FLAGS.iter().filter(|f| f.group == Group::Image) {
        if has(f.long) && cmd.name != "image" {
            return usage(format!("--{} only applies to `tdongle image`", f.long));
        }
    }
    if cargs.len() < cmd.min || cmd.max.is_some_and(|m| cargs.len() > m) {
        return usage(format!("usage: tdongle {}", spec::usage_of(cmd)));
    }

    let fits: Vec<&str> = ["fit", "fill", "stretch"]
        .into_iter()
        .filter(|f| has(f))
        .collect();
    if fits.len() > 1 {
        return usage(format!(
            "--{} and --{} conflict; pick one",
            fits[0], fits[1]
        ));
    }
    let color_mode = match val("color").as_deref() {
        None => ColorMode::Auto,
        Some("auto") => ColorMode::Auto,
        Some("always") => ColorMode::Always,
        Some("never") => ColorMode::Never,
        Some(v) => {
            return usage(format!(
                "--color: expected auto, always or never, not {v:?}"
            ))
        }
    };
    let color_mode = if has("no-color") {
        if color_mode == ColorMode::Always {
            return usage("--no-color and --color always conflict");
        }
        ColorMode::Never
    } else {
        color_mode
    };

    let image = ImageOpts {
        fit: match fits.first() {
            Some(&"fill") => image::Fit::Cover,
            Some(&"stretch") => image::Fit::Stretch,
            _ => image::Fit::Contain,
        },
        rot: match val("rot").as_deref() {
            None | Some("auto") => Some(255),
            Some("keep") => None,
            Some(v) => Some(ranged("--rot", v, 0, 3)? as u8),
        },
        bg: match val("bg") {
            Some(c) => color("--bg", &c)?,
            None => [0, 0, 0],
        },
        dither: !has("no-dither"),
        save: val("save"),
    };

    // Validate command arguments here, so bad input is a usage error (exit 2).
    match cmd.name {
        "text" => {
            num::<i32>("text X", &cargs[0])?;
            num::<i32>("text Y", &cargs[1])?;
            ranged("text SIZE", &cargs[2], 1, 8)?;
            color("text COLOR", &cargs[3])?;
        }
        "clear" if !cargs.is_empty() => {
            color("clear", &cargs[0])?;
        }
        "led" => {
            color("led", &cargs[0])?;
            if let Some(l) = cargs.get(1) {
                ranged("led LEVEL", l, 0, 31)?;
            }
        }
        "brightness" => {
            ranged("brightness", &cargs[0], 0, 255)?;
        }
        "sleep" if !cargs.is_empty() => {
            ranged("sleep SECONDS", &cargs[0], 0, 4_294_967)?;
        }
        _ => {}
    }

    Ok(Action::Run(Plan {
        cmd,
        args: cargs,
        port: val("port"),
        wait: match val("wait") {
            Some(v) => Some(Duration::from_secs_f64(num::<f64>("--wait", &v)?.max(0.0))),
            None => None,
        },
        timeout: Duration::from_millis(match val("timeout") {
            Some(v) => num("--timeout", &v)?,
            None => spec::DEFAULT_TIMEOUT_MS,
        }),
        json: has("json"),
        quiet: has("quiet"),
        color: color_mode,
        image,
    }))
}

// ------------------------------------------------------------------- help ----
fn columns(rows: &[(String, String)], indent: usize) -> String {
    let w = rows
        .iter()
        .map(|r| r.0.chars().count())
        .max()
        .unwrap_or(0)
        .min(34);
    columns_w(rows, indent, w)
}

fn columns_w(rows: &[(String, String)], indent: usize, w: usize) -> String {
    rows.iter()
        .map(|(a, b)| {
            let pad = " ".repeat(indent);
            if a.chars().count() > w {
                format!("{pad}{a}\n{pad}{}  {b}\n", " ".repeat(w))
            } else {
                format!("{pad}{a:<w$}  {b}\n")
            }
        })
        .collect()
}

fn flag_rows(groups: &[Group]) -> String {
    // One width for every group, so the option columns line up.
    let w = FLAGS
        .iter()
        .map(|f| spec::flag_label(f).chars().count())
        .max()
        .unwrap_or(0);
    let mut s = String::new();
    for g in groups {
        s += &format!("\n{}:\n", g.title());
        let rows: Vec<(String, String)> = FLAGS
            .iter()
            .filter(|f| f.group == *g)
            .map(|f| (spec::flag_label(f), f.help.to_string()))
            .collect();
        s += &columns_w(&rows, 2, w);
    }
    s
}

fn exit_line() -> String {
    let parts: Vec<String> = EXIT_CODES
        .iter()
        .map(|(c, m, _)| format!("{c} {}", m.split(':').next().unwrap_or(m)))
        .collect();
    format!("\nexit codes: {}\n", parts.join(", "))
}

fn example_rows(filter: Option<&str>) -> String {
    let rows: Vec<(String, String)> = EXAMPLES
        .iter()
        .filter(|(c, _, _)| filter.is_none_or(|f| *c == f))
        .map(|(_, cmd, why)| (cmd.to_string(), why.to_string()))
        .collect();
    columns(&rows, 2)
}

pub fn help() -> String {
    let mut s = String::from(
        "usage: tdongle [OPTIONS] [COMMAND] [ARGS...]\n       tdongle [OPTIONS] TEXT...\n\n",
    );
    s += spec::QUESTION;
    s += "\n\ncommands (no command: status; any other text: msg):\n";
    let rows: Vec<(String, String)> = COMMANDS
        .iter()
        .map(|c| {
            let name = match c.alias {
                Some(a) => format!("{} ({a})", spec::usage_of(c)),
                None => spec::usage_of(c),
            };
            (name, c.summary.to_string())
        })
        .collect();
    s += &columns(&rows, 2);
    s += &flag_rows(&Group::ALL);
    s += &format!("\ncolors: {}, or RRGGBB\n", spec::color_names());
    s += "\nexamples:\n";
    s += &example_rows(None);
    s += &exit_line();
    s
}

pub fn command_help(c: &Command) -> String {
    let mut s = format!("usage: tdongle {} [OPTIONS]\n\n", spec::usage_of(c));
    if let Some(a) = c.alias {
        s += &format!("(also: tdongle {a})\n\n");
    }
    s += c.summary;
    s += "\n";
    if c.persists {
        s += "This setting is saved on the dongle and survives restarts.\n";
    }
    let groups: Vec<Group> = if c.name == "image" {
        vec![Group::Image, Group::Dongle, Group::Output]
    } else if c.name == "list" {
        vec![Group::Output]
    } else {
        vec![Group::Dongle, Group::Output]
    };
    s += &flag_rows(&groups);
    if matches!(c.name, "text" | "clear" | "led") {
        s += &format!("\ncolors: {}, or RRGGBB\n", spec::color_names());
    }
    let ex = example_rows(Some(c.name));
    if !ex.is_empty() {
        s += "\nexamples:\n";
        s += &ex;
    }
    s += &exit_line();
    s
}

// ---------------------------------------------------------------- execute ----
fn open_port(plan: &Plan) -> R<Port> {
    let src = device::port_source(
        plan.port.as_deref(),
        std::env::var("TDONGLE_PORT").ok().as_deref(),
        Path::new("/dev/tdongle").exists(),
    );
    let deadline = plan.wait.map(|w| Instant::now() + w);
    loop {
        let attempt = device::resolve_port(&src, plan.timeout).and_then(|dev| {
            let mut p = Port::open(&dev)?;
            p.drain();
            if deadline.is_some() {
                match p.cmd("PING", Duration::from_millis(500)) {
                    Ok(r) if r == "PONG" => {
                        // PINGs sent while the firmware was still booting may be
                        // answered late; swallow those PONGs.
                        std::thread::sleep(Duration::from_millis(300));
                        p.drain();
                    }
                    _ => return Err(format!("{}: not answering yet", dev.display())),
                }
            }
            Ok(p)
        });
        match attempt {
            Ok(p) => return Ok(p),
            Err(e) => match deadline {
                Some(t) if Instant::now() < t => std::thread::sleep(Duration::from_millis(250)),
                _ => return Err(Fail::Runtime(e)),
            },
        }
    }
}

fn info_of(port: &mut Port, t: Duration) -> R<Info> {
    let r = port.cmd_ok("INFO", t).map_err(Fail::Runtime)?;
    Info::parse(&r).ok_or_else(|| {
        Fail::Runtime(format!(
            "unexpected INFO reply {r:?} (not tdongle-display firmware?)"
        ))
    })
}

/// The protocol lines a drawing command sends.
fn lines_for(plan: &Plan) -> R<Vec<String>> {
    let a = &plan.args;
    let joined = a.join(" ");
    let lines = match plan.cmd.name {
        "msg" => vec![format!("MSG {joined}")],
        "log" => vec![format!("LOG {joined}")],
        "text" => vec![format!("TEXT {joined}")],
        "clear" => vec![format!("CLEAR {joined}").trim_end().to_string()],
        "sleep" => vec![format!("SLEEP {joined}").trim_end().to_string()],
        "wake" => vec!["WAKE".into()],
        "brightness" => vec![format!("BL {joined}")],
        "led" => vec![format!("LED {joined}")],
        "send" if a.len() == 1 && a[0] == "-" => io::stdin()
            .lock()
            .lines()
            .map_while(Result::ok)
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        "send" => a.clone(),
        other => unreachable!("{other} is not a drawing command"),
    };
    if lines.iter().any(|l| l.contains('\n') || l.contains('\r')) {
        return usage("text must not contain newlines");
    }
    Ok(lines)
}

fn draw(plan: &Plan, port: &mut Port) -> (Outcome, Option<Fail>) {
    let lines = match lines_for(plan) {
        Ok(l) => l,
        Err(e) => {
            return (
                Outcome::Drawn(Drawn {
                    command: plan.cmd.name.into(),
                    port: port.path.display().to_string(),
                    sent: vec![],
                }),
                Some(e),
            )
        }
    };
    let mut sent = Vec::new();
    let mut fail = None;
    for l in lines {
        match port.cmd(&l, plan.timeout) {
            Ok(r) => {
                if r.starts_with("ERR") && fail.is_none() {
                    fail = Some(Fail::Runtime(format!(
                        "the dongle rejected `{l}`: {}",
                        r[3..].trim()
                    )));
                }
                sent.push(Sent {
                    line: l,
                    reply: Some(r),
                });
            }
            Err(e) => {
                sent.push(Sent {
                    line: l,
                    reply: None,
                });
                fail.get_or_insert(Fail::Runtime(e));
                break; // the port is in an unknown state; don't keep going
            }
        }
    }
    let n = sent.len();
    let out = Outcome::Drawn(Drawn {
        command: plan.cmd.name.into(),
        port: port.path.display().to_string(),
        sent,
    });
    if n > 1 {
        if let Some(Fail::Runtime(m)) = fail {
            // Printed after the results, by run(): the output above is partial.
            fail = Some(Fail::Runtime(format!(
                "{m}; not every line succeeded, so the results above are partial"
            )));
        }
    }
    (out, fail)
}

fn fit_name(f: image::Fit) -> &'static str {
    match f {
        image::Fit::Contain => "fit",
        image::Fit::Cover => "fill",
        image::Fit::Stretch => "stretch",
    }
}

fn show_image(plan: &Plan, port: Option<&mut Port>) -> R<Outcome> {
    let o = &plan.image;
    let path = &plan.args[0];
    let data = image::read_input(path).map_err(Fail::Runtime)?;
    let img = image::load(path, &data).map_err(Fail::Runtime)?;
    let auto_rot = if img.w > img.h { 1 } else { 0 };
    let mut shown = Shown {
        port: None,
        file: if path == "-" {
            "(stdin)".into()
        } else {
            path.clone()
        },
        source: (img.w, img.h),
        screen: (0, 0),
        rotation: 0,
        fit: fit_name(o.fit),
        dither: o.dither,
        background: o.bg,
        saved: None,
    };
    let Some(port) = port else {
        let rot = match o.rot {
            Some(255) => auto_rot,
            Some(r) => r,
            None => 0,
        };
        let (w, h) = device::dims_for_rot(rot);
        let px = image::to_rgb565_be(image::render(&img, w, h, o.fit, o.bg), w, h, o.dither);
        let save = o.save.clone().unwrap();
        std::fs::write(&save, image::rgb565_to_ppm(&px, w, h))
            .map_err(|e| Fail::Runtime(format!("{save}: {e}")))?;
        shown.screen = (w, h);
        shown.rotation = rot;
        shown.saved = Some(save);
        return Ok(Outcome::Shown(shown));
    };
    let t = plan.timeout;
    // Older firmware would draw "IMG ..." as text and then treat the pixels as commands.
    let info = info_of(port, t)?;
    if !info.at_least(1, 1) {
        return Err(Fail::Runtime(format!(
            "images need firmware 1.1.0 or newer (the dongle has {}); run setup.sh to update",
            info.version
        )));
    }
    let rot = match o.rot {
        None => info.rotation,
        Some(r) => {
            let r = if r == 255 { auto_rot } else { r };
            port.cmd_ok(&format!("ROT {r}"), t).map_err(Fail::Runtime)?;
            r
        }
    };
    let (w, h) = device::dims_for_rot(rot);
    let px = image::to_rgb565_be(image::render(&img, w, h, o.fit, o.bg), w, h, o.dither);
    // Chunked with acks: the device's USB driver drops data instead of throttling us.
    let ready = port
        .cmd_ok(&format!("IMG 0 0 {w} {h}"), t)
        .map_err(Fail::Runtime)?;
    let chunk = ready
        .strip_prefix("READY ")
        .and_then(|n| n.trim().parse::<usize>().ok())
        .ok_or_else(|| Fail::Runtime(format!("IMG: unexpected reply {ready:?}")))?
        .max(64);
    let parts: Vec<&[u8]> = px.chunks(chunk).collect();
    let slow = t.max(Duration::from_secs(3));
    for (i, part) in parts.iter().enumerate() {
        port.write_bytes(part)
            .map_err(|e| Fail::Runtime(port.io_err(e)))?;
        let last = i + 1 == parts.len();
        let reply = port
            .read_line(slow)
            .map_err(|e| Fail::Runtime(port.io_err(e)))?;
        match reply {
            Some(r) if !last && r == "NEXT" => {}
            Some(r) if last && r.starts_with("OK") => {}
            Some(r) => return Err(Fail::Runtime(format!("IMG: {r}"))),
            None => return Err(Fail::Runtime("IMG: no reply from the dongle".into())),
        }
    }
    shown.port = Some(port.path.display().to_string());
    shown.screen = (w, h);
    shown.rotation = rot;
    Ok(Outcome::Shown(shown))
}

fn execute(plan: &Plan) -> (Option<Outcome>, Option<Fail>) {
    let t = plan.timeout;
    match plan.cmd.name {
        "list" => {
            let entries = device::find_dongles()
                .into_iter()
                .map(|d| {
                    let (info, error) = match device::probe(&d.dev, t) {
                        Ok(i) => (Some(i), None),
                        Err(e) => (None, Some(e)),
                    };
                    ListEntry {
                        port: d.dev.display().to_string(),
                        serial: d.serial,
                        info,
                        error,
                    }
                })
                .collect();
            return (Some(Outcome::List(entries)), None);
        }
        "image" if plan.image.save.is_some() => {
            return match show_image(plan, None) {
                Ok(o) => (Some(o), None),
                Err(e) => (None, Some(e)),
            }
        }
        _ => {}
    }
    let mut port = match open_port(plan) {
        Ok(p) => p,
        Err(e) => return (None, Some(e)),
    };
    let portname = port.path.display().to_string();
    let result: R<Outcome> = match plan.cmd.name {
        "status" => info_of(&mut port, t).map(|info| {
            let chip = port
                .cmd_ok("TEMP", t)
                .ok()
                .and_then(|r| device::parse_temp(&r));
            Outcome::Status(Status {
                serial: device::serial_of(&port.path),
                port: portname.clone(),
                info,
                chip,
            })
        }),
        "temp" => port.cmd_ok("TEMP", t).map_err(Fail::Runtime).and_then(|r| {
            device::parse_temp(&r)
                .map(|(celsius, cpu_mhz)| {
                    Outcome::Temp(Temp {
                        port: portname.clone(),
                        celsius,
                        cpu_mhz,
                    })
                })
                .ok_or_else(|| Fail::Runtime(format!("unexpected TEMP reply {r:?}")))
        }),
        "ping" => {
            let start = Instant::now();
            port.cmd_ok("PING", t).map_err(Fail::Runtime).and_then(|r| {
                if r == "PONG" {
                    Ok(Outcome::Ping(Ping {
                        port: portname.clone(),
                        latency_ms: start.elapsed().as_secs_f64() * 1000.0,
                    }))
                } else {
                    Err(Fail::Runtime(format!("unexpected PING reply {r:?}")))
                }
            })
        }
        "image" => show_image(plan, Some(&mut port)),
        _ => {
            let (o, f) = draw(plan, &mut port);
            return (Some(o), f);
        }
    };
    match result {
        Ok(o) => (Some(o), None),
        Err(e) => (None, Some(e)),
    }
}

// -------------------------------------------------------------------- run ----
fn print(s: &str) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
}

/// Runs the CLI with `args` (without the program name) and returns the exit code.
pub fn run(args: &[String]) -> i32 {
    sys::default_sigpipe();
    let action = match parse(args) {
        Ok(a) => a,
        Err(e) => return fail(e),
    };
    match action {
        Action::Version => {
            print(&format!("{} {}\n", spec::NAME, spec::VERSION));
            0
        }
        Action::Help(None) => {
            print(&help());
            0
        }
        Action::Help(Some(c)) => {
            print(&command_help(c));
            0
        }
        Action::Skill => {
            let ctx = skill::SkillCtx {
                port: device::port_source(
                    None,
                    std::env::var("TDONGLE_PORT").ok().as_deref(),
                    Path::new("/dev/tdongle").exists(),
                ),
            };
            print(&skill::render_skill(&ctx));
            0
        }
        Action::Run(plan) => {
            let (outcome, failure) = execute(&plan);
            if let Some(o) = &outcome {
                if plan.json {
                    print(&o.to_json(&sys::utc_now_rfc3339()).pretty());
                } else if !plan.quiet {
                    let st = Style {
                        on: render::want_color(
                            plan.color,
                            std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
                            std::env::var("TERM").ok().as_deref(),
                            sys::stdout_is_tty(),
                        ),
                    };
                    print(&render::text(o, &st));
                }
            }
            match failure {
                Some(f) => fail(f),
                None => 0,
            }
        }
    }
}

fn fail(f: Fail) -> i32 {
    let msg = match &f {
        Fail::Usage(m) | Fail::Runtime(m) => m,
    };
    eprintln!("{}: {msg}", spec::NAME);
    f.code()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::PortSource;
    use crate::skill::{render_skill, SkillCtx};

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    fn sample_value(f: &Flag) -> &'static str {
        match f.long {
            "rot" => "1",
            "bg" => "navy",
            "color" => "never",
            "port" => "/dev/null",
            _ => "1",
        }
    }

    #[test]
    fn every_flag_in_the_table_parses() {
        for f in FLAGS {
            let mut a = vec![if f.group == Group::Image {
                "image".to_string()
            } else {
                "ping".to_string()
            }];
            if f.group == Group::Image {
                a.push("x.png".into());
            }
            a.push(format!("--{}", f.long));
            if f.value.is_some() {
                a.push(sample_value(f).into());
            }
            assert!(
                parse(&a).is_ok(),
                "--{} failed to parse: {:?}",
                f.long,
                parse(&a).err()
            );
            if let Some(c) = f.short {
                a[if f.group == Group::Image { 2 } else { 1 }] = format!("-{c}");
                assert!(parse(&a).is_ok(), "-{c} failed to parse");
            }
        }
    }

    #[test]
    fn every_flag_and_command_is_in_the_help_and_the_skill() {
        let h = help();
        let sk = render_skill(&SkillCtx {
            port: PortSource::Autodetect,
        });
        for f in FLAGS {
            assert!(
                h.contains(&format!("--{}", f.long)),
                "--{} missing from help",
                f.long
            );
            assert!(
                sk.contains(&format!("--{}", f.long)),
                "--{} missing from skill",
                f.long
            );
        }
        for c in COMMANDS {
            assert!(
                h.contains(&spec::usage_of(c)),
                "{} missing from help",
                c.name
            );
            let in_table = format!("`{}`", spec::usage_of(c)).replace('|', "\\|");
            assert!(sk.contains(&in_table), "{} missing from skill", c.name);
            assert!(command_help(c).starts_with(&format!("usage: tdongle {}", spec::usage_of(c))));
        }
        for (code, _, _) in EXIT_CODES {
            assert!(
                h.contains(&format!("{code} ")),
                "exit code {code} missing from help"
            );
            assert!(
                sk.contains(&format!("`{code}`")),
                "exit code {code} missing from skill"
            );
        }
    }

    /// Splits a shell line into words (double quotes only), for the examples.
    fn words(line: &str) -> Vec<String> {
        let (mut out, mut cur, mut q) = (vec![], String::new(), false);
        for ch in line.chars() {
            match ch {
                '"' => q = !q,
                ' ' if !q => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                c => cur.push(c),
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out
    }

    #[test]
    fn every_example_parses_and_help_ends_with_the_skill_install() {
        for (_, line, _) in EXAMPLES {
            let seg = line.rsplit(['|', '&']).next().unwrap().trim();
            let w = words(seg);
            assert_eq!(w[0], "tdongle", "{line}");
            let args: Vec<String> = w[1..]
                .iter()
                .filter(|a| *a != ">" && !a.starts_with('~'))
                .cloned()
                .collect();
            assert!(
                parse(&args).is_ok(),
                "example does not parse: {line}: {:?}",
                parse(&args).err()
            );
        }
        assert!(EXAMPLES.last().unwrap().1.contains("--skill >"));
        assert!((8..=12).contains(&EXAMPLES.len()));
    }

    #[test]
    fn bare_run_is_status_and_free_text_is_a_message() {
        let Ok(Action::Run(p)) = parse(&[]) else {
            panic!()
        };
        assert_eq!(p.cmd.name, "status");
        let Ok(Action::Run(p)) = parse(&argv(&["Build", "passed"])) else {
            panic!()
        };
        assert_eq!(
            (p.cmd.name, p.args.join(" ").as_str()),
            ("msg", "Build passed")
        );
        let Ok(Action::Run(p)) = parse(&argv(&["msg", "--", "-5", "--json"])) else {
            panic!()
        };
        assert_eq!(p.args.join(" "), "-5 --json");
        assert!(!p.json);
    }

    #[test]
    fn bad_usage_is_exit_2_and_names_the_problem() {
        for (a, needle) in [
            (vec!["--bogus"], "--bogus"),
            (vec!["clear", "chartreuse"], "chartreuse"),
            (vec!["brightness", "300"], "out of range"),
            (vec!["msg", "--fill"], "only applies"),
            (vec!["image", "a.png", "--fit", "--fill"], "conflict"),
            (vec!["--no-color", "--color", "always"], "conflict"),
            (vec!["text", "1", "2"], "usage: tdongle text"),
            (vec!["--port"], "needs a value"),
        ] {
            let e = parse(&argv(&a))
                .err()
                .unwrap_or_else(|| panic!("{a:?} parsed"));
            assert_eq!(e.code(), 2);
            let Fail::Usage(m) = e else { panic!() };
            assert!(m.contains(needle), "{a:?}: {m}");
        }
    }

    #[test]
    fn skill_front_matter_has_a_one_line_quoted_description_and_the_version() {
        let sk = render_skill(&SkillCtx {
            port: PortSource::Env("/dev/ttyACM7".into()),
        });
        let fm: Vec<&str> = sk.split("---\n").nth(1).unwrap().lines().collect();
        assert!(fm.len() > 2, "front matter should be a multi-line block");
        let d = fm.iter().find(|l| l.starts_with("description: ")).unwrap();
        assert!(d.starts_with("description: \"") && d.ends_with('"'));
        assert!(sk.contains(spec::VERSION));
        assert!(sk.contains("/dev/ttyACM7") && sk.contains("$TDONGLE_PORT"));
    }

    #[test]
    fn skill_with_state_changing_command_takes_the_read_only_path() {
        assert!(matches!(
            parse(&argv(&["brightness", "10", "--skill"])),
            Ok(Action::Skill)
        ));
        assert!(matches!(
            parse(&argv(&["sleep", "0", "--skill"])),
            Ok(Action::Skill)
        ));
    }
}
