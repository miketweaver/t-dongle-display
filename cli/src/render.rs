//! Text output: a small Style for colour, and one renderer per outcome.

use crate::device::orientation;
use crate::model::{Outcome, Shown, Status};
use crate::spec;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

/// `auto` honours NO_COLOR, TERM=dumb and whether stdout is a terminal.
pub fn want_color(mode: ColorMode, no_color_env: bool, term: Option<&str>, tty: bool) -> bool {
    match mode {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => !no_color_env && term != Some("dumb") && tty,
    }
}

#[derive(Clone, Copy)]
pub struct Style {
    pub on: bool,
}

impl Style {
    fn wrap(&self, code: &str, s: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    pub fn bold(&self, s: &str) -> String {
        self.wrap("1", s)
    }
    pub fn dim(&self, s: &str) -> String {
        self.wrap("2", s)
    }
    pub fn head(&self, s: &str) -> String {
        self.wrap("1;36", s)
    }
    pub fn warn(&self, s: &str) -> String {
        self.wrap("33", s)
    }
    pub fn good(&self, s: &str) -> String {
        self.wrap("32", s)
    }
}

fn duration(secs: u32) -> String {
    match secs {
        0 => "never".into(),
        s if s % 3600 == 0 => format!("{} h", s / 3600),
        s if s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{s} s"),
    }
}

fn row(st: &Style, label: &str, value: &str) -> String {
    format!("  {}  {value}\n", st.dim(&format!("{label:<10}")))
}

fn status(s: &Status, st: &Style) -> String {
    let i = &s.info;
    let mut out = format!("{}\n", st.head(&format!("T-Dongle on {}", s.port)));
    out += &row(st, "firmware", &format!("{} {}", i.firmware, i.version));
    let state = match i.asleep {
        Some(true) => st.warn("asleep"),
        Some(false) => st.good("awake"),
        None => "?".into(),
    };
    let sleeps = match i.sleep_after_s {
        Some(0) => "never sleeps".to_string(),
        Some(n) => format!("sleeps after {} idle", duration(n)),
        None => String::new(),
    };
    out += &row(
        st,
        "screen",
        &format!(
            "{} {}x{}, {state}{}",
            orientation(i.rotation),
            i.width,
            i.height,
            if sleeps.is_empty() {
                String::new()
            } else {
                format!(", {sleeps}")
            }
        ),
    );
    if let Some(b) = i.brightness {
        out += &row(st, "backlight", &format!("{b} of 255"));
    }
    if let Some((c, mhz)) = s.chip {
        out += &row(st, "chip", &format!("{c:.1} °C, CPU {mhz} MHz"));
    }
    if let Some(serial) = &s.serial {
        out += &row(st, "serial", serial);
    }
    let hint = if i.asleep == Some(true) {
        Some("The screen is asleep; any drawing command wakes it.".to_string())
    } else if !i.at_least_version(spec::VERSION) {
        Some(format!(
            "Firmware {} is older than this CLI ({}); run setup.sh to update it.",
            i.version,
            spec::VERSION
        ))
    } else if i.sleep_after_s == Some(0) {
        Some(
            "The screen never sleeps; `tdongle sleep 300` turns it off after 5 idle minutes."
                .to_string(),
        )
    } else {
        None
    };
    if let Some(h) = hint {
        out += &st.dim(&h);
        out.push('\n');
    }
    out
}

fn shown(s: &Shown, st: &Style) -> String {
    let what = format!(
        "{} {}x{} -> {}x{} {}, {}",
        s.file,
        s.source.0,
        s.source.1,
        s.screen.0,
        s.screen.1,
        orientation(s.rotation),
        s.fit
    );
    match (&s.saved, &s.port) {
        (Some(path), _) => format!("Saved a preview of {what} to {}\n", st.bold(path)),
        (None, Some(port)) => format!("Showed {what} on {port}\n"),
        (None, None) => format!("Converted {what}\n"),
    }
}

/// The text form of an outcome; empty means "print nothing".
pub fn text(o: &Outcome, st: &Style) -> String {
    match o {
        Outcome::Status(s) => status(s, st),
        Outcome::Temp(t) => format!(
            "{:.1} °C {}\n",
            t.celsius,
            st.dim(&format!("chip temperature, CPU at {} MHz", t.cpu_mhz))
        ),
        Outcome::Ping(p) => format!("pong from {} in {:.1} ms\n", p.port, p.latency_ms),
        Outcome::List(v) if v.is_empty() => format!(
            "No T-Dongle found.\n{}\n",
            st.dim("Plug it in; it shows up as USB 303a:1001 (/dev/ttyACM*).")
        ),
        Outcome::List(v) => {
            let mut out = format!(
                "{}\n",
                st.head(&format!(
                    "{} T-Dongle{}",
                    v.len(),
                    if v.len() == 1 { "" } else { "s" }
                ))
            );
            for e in v {
                let desc = match (&e.info, &e.error) {
                    (Some(i), _) => format!(
                        "{} {}, {}{}",
                        i.firmware,
                        i.version,
                        orientation(i.rotation),
                        if i.asleep == Some(true) {
                            ", asleep"
                        } else {
                            ""
                        }
                    ),
                    (None, Some(err)) => st.warn(err),
                    (None, None) => "?".into(),
                };
                out += &format!("  {}  {}  {desc}\n", st.bold(&e.port), st.dim(&e.serial));
            }
            out
        }
        // Drawing commands print nothing; `send` prints the replies, which are its result.
        Outcome::Drawn(d) if d.command == "send" => d
            .sent
            .iter()
            .map(|s| format!("{}\n", s.reply.as_deref().unwrap_or("(no reply)")))
            .collect(),
        Outcome::Drawn(_) => String::new(),
        Outcome::Shown(s) => shown(s, st),
    }
}

trait AtLeast {
    fn at_least_version(&self, v: &str) -> bool;
}

impl AtLeast for crate::device::Info {
    fn at_least_version(&self, v: &str) -> bool {
        let p: Vec<u32> = v.split('.').filter_map(|x| x.parse().ok()).collect();
        p.len() < 2 || self.at_least(p[0], p[1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_color_honours_no_color_dumb_terminals_and_pipes() {
        assert!(want_color(ColorMode::Auto, false, Some("xterm"), true));
        assert!(!want_color(ColorMode::Auto, true, Some("xterm"), true));
        assert!(!want_color(ColorMode::Auto, false, Some("dumb"), true));
        assert!(!want_color(ColorMode::Auto, false, Some("xterm"), false));
        assert!(want_color(ColorMode::Always, true, Some("dumb"), false));
        assert!(!want_color(ColorMode::Never, false, None, true));
    }

    #[test]
    fn style_off_emits_no_escape_codes() {
        let st = Style { on: false };
        assert_eq!(st.head("x") + &st.dim("y") + &st.warn("z"), "xyz");
        assert!(Style { on: true }.bold("x").contains('\x1b'));
    }
}
