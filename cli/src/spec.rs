//! The one table of commands, flags, examples and exit codes. The parser,
//! `--help`, per-command help and `--skill` are all generated from it, and
//! tests check that none of them can drift.

pub const NAME: &str = "tdongle";
pub const ALIAS: &str = "tdongle-display";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPO: &str = "https://github.com/miketweaver/t-dongle-display";
pub const SETUP_URL: &str =
    "https://github.com/miketweaver/t-dongle-display/releases/latest/download/setup.sh";
pub const QUESTION: &str = "What should the T-Dongle show? Put text, drawings and images on a LilyGO T-Dongle-S3's screen over USB.";
pub const SKILL_PATH: &str = "~/.claude/skills/tdongle/SKILL.md";

pub const DEFAULT_TIMEOUT_MS: u64 = 2000;

#[derive(Debug)]
pub struct Command {
    pub name: &'static str,
    pub alias: Option<&'static str>,
    pub args: &'static str,
    pub min: usize,
    pub max: Option<usize>,
    pub summary: &'static str,
    /// Changes a setting that the dongle saves across restarts.
    pub persists: bool,
}

pub const COMMANDS: &[Command] = &[
    Command { name: "status", alias: Some("info"), args: "", min: 0, max: Some(0), persists: false,
        summary: "firmware version, screen state and chip temperature (the default with no arguments)" },
    Command { name: "msg", alias: None, args: "TEXT...", min: 1, max: None, persists: false,
        summary: "show TEXT as large as fits, centered and word-wrapped (also what any non-command text does)" },
    Command { name: "log", alias: None, args: "TEXT...", min: 0, max: None, persists: false,
        summary: "append a line to a scrolling console" },
    Command { name: "text", alias: None, args: "X Y SIZE COLOR TEXT...", min: 5, max: None, persists: false,
        summary: "draw TEXT at pixel X,Y without clearing (SIZE 1-8; 6x8 px per character at size 1)" },
    Command { name: "clear", alias: None, args: "[COLOR]", min: 0, max: Some(1), persists: false,
        summary: "fill the screen with COLOR (default: black)" },
    Command { name: "image", alias: Some("img"), args: "FILE", min: 1, max: Some(1), persists: false,
        summary: "show an image (PNG, JPEG, BMP, PPM; others via ImageMagick or ffmpeg); FILE `-` reads stdin" },
    Command { name: "send", alias: Some("raw"), args: "LINE... | -", min: 1, max: None, persists: false,
        summary: "send raw protocol lines (`-` reads them from stdin) and print the replies" },
    Command { name: "list", alias: None, args: "", min: 0, max: Some(0), persists: false,
        summary: "list every connected dongle and its firmware" },
    Command { name: "temp", alias: None, args: "", min: 0, max: Some(0), persists: false,
        summary: "chip temperature and CPU clock" },
    Command { name: "ping", alias: None, args: "", min: 0, max: Some(0), persists: false,
        summary: "check that the dongle answers, with the round-trip time" },
    Command { name: "sleep", alias: None, args: "[SECONDS]", min: 0, max: Some(1), persists: true,
        summary: "turn the screen off now, or set the idle timeout in seconds (0 = never; saved on the dongle)" },
    Command { name: "wake", alias: None, args: "", min: 0, max: Some(0), persists: false,
        summary: "turn the screen back on (any drawing command also wakes it)" },
    Command { name: "brightness", alias: Some("bl"), args: "LEVEL", min: 1, max: Some(1), persists: true,
        summary: "backlight brightness 0-255 (saved on the dongle)" },
    Command { name: "led", alias: None, args: "COLOR [LEVEL]", min: 1, max: Some(2), persists: false,
        summary: "set the onboard RGB LED (LEVEL 0-31, default 8; `led black` turns it off)" },
];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Group {
    Dongle,
    Image,
    Output,
    About,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Dongle => "choosing the dongle",
            Group::Image => "images (with `image`)",
            Group::Output => "output",
            Group::About => "about",
        }
    }
    pub const ALL: [Group; 4] = [Group::Dongle, Group::Image, Group::Output, Group::About];
}

pub struct Flag {
    pub short: Option<char>,
    pub long: &'static str,
    pub value: Option<&'static str>,
    pub group: Group,
    pub help: &'static str,
}

pub const FLAGS: &[Flag] = &[
    Flag { short: Some('p'), long: "port", value: Some("DEV"), group: Group::Dongle,
        help: "serial device (default: $TDONGLE_PORT, then /dev/tdongle, then autodetect)" },
    Flag { short: Some('w'), long: "wait", value: Some("SECS"), group: Group::Dongle,
        help: "wait up to SECS for the dongle to appear and answer (default: don't wait)" },
    Flag { short: Some('t'), long: "timeout", value: Some("MS"), group: Group::Dongle,
        help: "reply timeout per command (default: 2000)" },
    Flag { short: None, long: "fit", value: None, group: Group::Image,
        help: "show the whole image, letterboxed (default)" },
    Flag { short: None, long: "fill", value: None, group: Group::Image,
        help: "fill the screen, cropping the overflow" },
    Flag { short: None, long: "stretch", value: None, group: Group::Image,
        help: "fill the screen, ignoring the aspect ratio" },
    Flag { short: None, long: "rot", value: Some("ROT"), group: Group::Image,
        help: "screen rotation: auto, keep or 0-3 (default: auto = 1 for wide images, else 0)" },
    Flag { short: None, long: "bg", value: Some("COLOR"), group: Group::Image,
        help: "letterbox and transparency color (default: black)" },
    Flag { short: None, long: "no-dither", value: None, group: Group::Image,
        help: "plain RGB565 rounding instead of Floyd-Steinberg dithering" },
    Flag { short: None, long: "save", value: Some("FILE"), group: Group::Image,
        help: "write the converted image to FILE (PPM) instead of sending it; needs no dongle" },
    Flag { short: Some('j'), long: "json", value: None, group: Group::Output,
        help: "print one JSON document instead of text" },
    Flag { short: None, long: "color", value: Some("WHEN"), group: Group::Output,
        help: "auto, always or never (default: auto; honours NO_COLOR, TERM=dumb and whether stdout is a terminal)" },
    Flag { short: None, long: "no-color", value: None, group: Group::Output,
        help: "same as --color never" },
    Flag { short: Some('q'), long: "quiet", value: None, group: Group::Output,
        help: "print nothing on success (errors still go to stderr)" },
    Flag { short: None, long: "skill", value: None, group: Group::About,
        help: "print an Agent Skill (SKILL.md) that teaches an AI assistant to use this command, and exit" },
    Flag { short: Some('h'), long: "help", value: None, group: Group::About,
        help: "show this help, or a command's (`tdongle image --help`), and exit" },
    Flag { short: Some('V'), long: "version", value: None, group: Group::About,
        help: "show the version and exit" },
];

/// (command it belongs to, command line, comment). Every one runs as written.
pub const EXAMPLES: &[(&str, &str, &str)] = &[
    (
        "status",
        "tdongle",
        "is it connected? firmware, screen and temperature",
    ),
    ("msg", "tdongle \"Build passed\"", "a big centered message"),
    (
        "msg",
        "tdongle msg -- -5 degrees",
        "text that starts with a dash",
    ),
    (
        "log",
        "tdongle log \"$(date +%T) backup ok\"",
        "append to a scrolling log",
    ),
    (
        "send",
        "tdongle send \"CLEAR navy\" \"TEXT 4 4 2 yellow Hi\"",
        "raw drawing commands",
    ),
    (
        "image",
        "tdongle image photo.jpg",
        "letterboxed; wide images turn the screen sideways",
    ),
    (
        "image",
        "tdongle image logo.png --fill --rot keep",
        "fill the screen, keep the rotation",
    ),
    (
        "image",
        "curl -s https://example.com/a.png | tdongle image -",
        "an image from stdin",
    ),
    (
        "image",
        "tdongle image photo.jpg --save preview.ppm",
        "preview the result without a dongle",
    ),
    (
        "sleep",
        "tdongle sleep 600",
        "screen turns off after 10 idle minutes",
    ),
    (
        "list",
        "tdongle list --json",
        "every connected dongle, for scripts",
    ),
    (
        "",
        "mkdir -p ~/.claude/skills/tdongle && tdongle --skill > ~/.claude/skills/tdongle/SKILL.md",
        "teach an AI assistant (rerun after upgrading)",
    ),
];

pub const EXIT_CODES: &[(i32, &str, &str)] = &[
    (0, "success", "done"),
    (1, "runtime failure: no dongle found, permission denied, no reply, the dongle replied ERR, or a file could not be read or decoded",
        "read the one-line message on stderr; see Troubleshooting"),
    (2, "bad usage: unknown command or flag, bad number or color, conflicting flags",
        "fix the command line; `tdongle <command> --help` shows the usage"),
    (130, "interrupted (Ctrl-C)", "nothing to do"),
    (141, "stdout closed early (`tdongle list | head -0`)", "nothing to do"),
];

pub const COLORS: &[(&str, u32)] = &[
    ("black", 0x000000),
    ("white", 0xFFFFFF),
    ("red", 0xFF0000),
    ("green", 0x00FF00),
    ("blue", 0x0000FF),
    ("yellow", 0xFFFF00),
    ("cyan", 0x00FFFF),
    ("magenta", 0xFF00FF),
    ("orange", 0xFF8000),
    ("purple", 0x8000FF),
    ("gray", 0x808080),
    ("grey", 0x808080),
    ("pink", 0xFF60A0),
    ("navy", 0x000080),
    ("darkgreen", 0x006400),
    ("brown", 0x8B4513),
];

pub fn parse_color(s: &str) -> Option<[u8; 3]> {
    let t = s.trim().to_ascii_lowercase();
    let v = match COLORS.iter().find(|(n, _)| *n == t) {
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

pub fn color_names() -> String {
    let names: Vec<&str> = COLORS
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| *n != "grey")
        .collect();
    names.join(" ")
}

pub fn find_command(word: &str) -> Option<&'static Command> {
    let w = word.to_ascii_lowercase();
    COMMANDS
        .iter()
        .find(|c| c.name == w || c.alias == Some(w.as_str()))
}

pub fn find_flag_long(name: &str) -> Option<&'static Flag> {
    FLAGS.iter().find(|f| f.long == name)
}

pub fn find_flag_short(c: char) -> Option<&'static Flag> {
    FLAGS.iter().find(|f| f.short == Some(c))
}

/// `-p, --port DEV`
pub fn flag_label(f: &Flag) -> String {
    let mut s = match f.short {
        Some(c) => format!("-{c}, --{}", f.long),
        None => format!("    --{}", f.long),
    };
    if let Some(v) = f.value {
        s.push(' ');
        s.push_str(v);
    }
    s
}

/// `image FILE`, `status`
pub fn usage_of(c: &Command) -> String {
    if c.args.is_empty() {
        c.name.to_string()
    } else {
        format!("{} {}", c.name, c.args)
    }
}
