//! What each command produces. Text and JSON are both rendered from these.

use crate::device::{orientation, Info};
use crate::json::J;

pub struct Status {
    pub port: String,
    pub serial: Option<String>,
    pub info: Info,
    pub chip: Option<(f64, u32)>,
}

pub struct Temp {
    pub port: String,
    pub celsius: f64,
    pub cpu_mhz: u32,
}

pub struct Ping {
    pub port: String,
    pub latency_ms: f64,
}

pub struct ListEntry {
    pub port: String,
    pub serial: String,
    pub info: Option<Info>,
    pub error: Option<String>,
}

pub struct Sent {
    pub line: String,
    pub reply: Option<String>,
}

pub struct Drawn {
    pub command: String,
    pub port: String,
    pub sent: Vec<Sent>,
}

pub struct Shown {
    pub port: Option<String>,
    pub file: String,
    pub source: (u32, u32),
    pub screen: (usize, usize),
    pub rotation: u8,
    pub fit: &'static str,
    pub dither: bool,
    pub background: [u8; 3],
    pub saved: Option<String>,
}

pub enum Outcome {
    Status(Status),
    Temp(Temp),
    Ping(Ping),
    List(Vec<ListEntry>),
    Drawn(Drawn),
    Shown(Shown),
}

fn firmware(i: &Info) -> J {
    J::obj(vec![
        ("name", J::str(&i.firmware)),
        ("version", J::str(&i.version)),
    ])
}

fn screen(i: &Info) -> J {
    J::obj(vec![
        ("width", J::Int(i.width as i64)),
        ("height", J::Int(i.height as i64)),
        ("rotation", J::Int(i.rotation as i64)),
        ("orientation", J::str(orientation(i.rotation))),
        ("asleep", J::opt(i.asleep, J::Bool)),
        (
            "sleep_after_s",
            J::opt(i.sleep_after_s, |v| J::Int(v as i64)),
        ),
        ("brightness", J::opt(i.brightness, |v| J::Int(v as i64))),
    ])
}

pub fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

impl Outcome {
    pub fn to_json(&self, generated_at: &str) -> J {
        let mut j = match self {
            Outcome::Status(s) => J::obj(vec![
                ("connected", J::Bool(true)),
                ("port", J::str(&s.port)),
                ("serial", J::opt(s.serial.clone(), J::Str)),
                ("firmware", firmware(&s.info)),
                ("screen", screen(&s.info)),
                (
                    "chip",
                    J::opt(s.chip, |(c, mhz)| {
                        J::obj(vec![
                            ("temperature_c", J::Num(c)),
                            ("cpu_mhz", J::Int(mhz as i64)),
                        ])
                    }),
                ),
            ]),
            Outcome::Temp(t) => J::obj(vec![
                ("temperature_c", J::Num(t.celsius)),
                ("cpu_mhz", J::Int(t.cpu_mhz as i64)),
                ("port", J::str(&t.port)),
            ]),
            Outcome::Ping(p) => J::obj(vec![
                ("ok", J::Bool(true)),
                ("latency_ms", J::Num((p.latency_ms * 10.0).round() / 10.0)),
                ("port", J::str(&p.port)),
            ]),
            Outcome::List(v) => J::obj(vec![
                ("count", J::Int(v.len() as i64)),
                (
                    "dongles",
                    J::Arr(
                        v.iter()
                            .map(|e| {
                                J::obj(vec![
                                    ("port", J::str(&e.port)),
                                    ("serial", J::str(&e.serial)),
                                    ("responding", J::Bool(e.info.is_some())),
                                    ("firmware", J::opt(e.info.as_ref(), firmware)),
                                    ("screen", J::opt(e.info.as_ref(), screen)),
                                    ("error", J::opt(e.error.clone(), J::Str)),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ]),
            Outcome::Drawn(d) => J::obj(vec![
                (
                    "ok",
                    J::Bool(
                        d.sent
                            .iter()
                            .all(|s| s.reply.as_deref().is_some_and(|r| !r.starts_with("ERR"))),
                    ),
                ),
                ("command", J::str(&d.command)),
                ("port", J::str(&d.port)),
                (
                    "sent",
                    J::Arr(
                        d.sent
                            .iter()
                            .map(|s| {
                                J::obj(vec![
                                    ("line", J::str(&s.line)),
                                    ("reply", J::opt(s.reply.clone(), J::Str)),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ]),
            Outcome::Shown(s) => J::obj(vec![
                ("ok", J::Bool(true)),
                ("saved", J::opt(s.saved.clone(), J::Str)),
                ("port", J::opt(s.port.clone(), J::Str)),
                ("file", J::str(&s.file)),
                (
                    "source",
                    J::obj(vec![
                        ("width", J::Int(s.source.0 as i64)),
                        ("height", J::Int(s.source.1 as i64)),
                    ]),
                ),
                (
                    "screen",
                    J::obj(vec![
                        ("width", J::Int(s.screen.0 as i64)),
                        ("height", J::Int(s.screen.1 as i64)),
                        ("rotation", J::Int(s.rotation as i64)),
                        ("orientation", J::str(orientation(s.rotation))),
                    ]),
                ),
                ("fit", J::str(s.fit)),
                ("dither", J::Bool(s.dither)),
                ("background", J::str(hex(s.background))),
            ]),
        };
        if let J::Obj(v) = &mut j {
            v.push(("generated_at".into(), J::str(generated_at)));
        }
        j
    }
}
