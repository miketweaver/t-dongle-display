# tdongle

What should the T-Dongle show? `tdongle` turns a
[LilyGO T-Dongle-S3](https://github.com/Xinyuan-LilyGO/T-Dongle-S3) (ESP32-S3 with a 0.96" 80×160 LCD)
into a tiny USB status display for any Linux machine: messages, logs, drawings and images,
from one static binary with no dependencies. With no arguments it tells you how the dongle is doing:

```
$ tdongle
T-Dongle on /dev/ttyACM0
  firmware    tdongle-display 2.0.0
  screen      portrait 80x160, awake, sleeps after 5 min idle
  backlight   160 of 255
  chip        36.0 °C, CPU 80 MHz
  serial      F4:12:FA:00:00:00
```

## Install

Plug in the dongle, then:

```sh
curl -fsSL https://github.com/miketweaver/t-dongle-display/releases/latest/download/setup.sh | sh
```

Or download [`setup.sh`](https://github.com/miketweaver/t-dongle-display/releases/latest/download/setup.sh),
read it, and run `sh setup.sh`. It works on any Linux on x86_64, aarch64 or armv7 with
`curl` or `wget`, `tar` and `sha256sum`. It needs no Python, Rust, Arduino or esptool.

What it does, touching the OS as little as possible:

1. Installs `tdongle` to `~/.local/bin`, with `tdongle-display` as a long alias (no root).
2. Checks whether your user can open the serial port. Most distros already allow this
   (`dialout`/`uucp` group, or a logged-in seat). **If it can't**, it asks to install one udev
   rule, `/etc/udev/rules.d/70-tdongle.rules`, which gives the logged-in user access and adds `/dev/tdongle`.
   Nothing else on the system changes.
3. Flashes the firmware. It downloads a pinned, checksum-verified esptool into a temp dir and
   deletes it afterwards. **No BOOT button needed** (see [About the firmware](#about-the-firmware)).
   It skips the flash if the same version is already running.
4. Says hello on the screen.

Options: `--port DEV`, `--version vX.Y.Z`, `--bin-dir DIR`, `--no-flash`, `--force-flash`,
`--no-udev`, `--yes`, `--uninstall`. `sh setup.sh --uninstall` removes the CLI, the alias and the udev rule.

## Usage

`tdongle --help` lists everything, and `tdongle <command> --help` shows one command.

### Text

```sh
tdongle "Build passed"                     # any text that isn't a command: big, centered, wrapped
tdongle msg -- -5 degrees                  # `--` for text that starts with a dash
tdongle log "$(date +%T) backup ok"        # scrolling console, one line per call
tdongle text 4 4 2 yellow "Hi"             # text at a pixel position, size 1-8, without clearing
```

### Drawing

```sh
tdongle clear navy
tdongle send "CLEAR navy" "FCIRCLE 40 80 20 orange" "TEXT 4 4 2 yellow Hi"   # raw protocol lines
printf 'LOG one\nLOG two\n' | tdongle send -
```

See [Protocol](#protocol) for every drawing line. Colors: `black white red green blue yellow cyan
magenta orange purple gray pink navy darkgreen brown`, or `RRGGBB`.

### Images

```sh
tdongle image photo.jpg                       # letterboxed; wide images turn the screen sideways
tdongle image logo.png --fill --rot keep      # fill the screen (crop), keep the current rotation
curl -s https://example.com/x.png | tdongle image -
tdongle image photo.jpg --save preview.ppm    # see the result without a dongle
```

- **Built in:** PNG (every color type and bit depth, interlaced, transparency), baseline JPEG
  (including the EXIF orientation phones write), BMP, and PPM/PGM. No libraries.
- **Other formats** (progressive JPEG, GIF, WebP, HEIC…) go through ImageMagick or `ffmpeg` if installed.
- Images are scaled with an area filter, composited over `--bg` where transparent, and
  Floyd–Steinberg dithered to the panel's RGB565 colors. A full-screen push takes about 0.3 s.
- `--rot auto` (the default) uses rotation 1 for images wider than tall, 0 otherwise; `--rot 3`
  if yours comes out upside down. The rotation stays in effect for later commands.

### Status and power

```sh
tdongle                  # status (also: tdongle status / info)
tdongle temp             # chip temperature
tdongle ping             # does it answer? round-trip time
tdongle list             # every connected dongle
tdongle sleep            # screen off now; any drawing command wakes it
tdongle sleep 600        # screen sleeps after 10 idle minutes (0 = never); saved on the dongle
tdongle brightness 80    # backlight 0-255; saved on the dongle
tdongle led green 4      # onboard RGB LED
```

### Picking a dongle

The port is found automatically: `--port`, then `$TDONGLE_PORT`, then `/dev/tdongle`, then the
first ttyACM device with USB id `303a:1001` that answers. `-w 10` waits up to 10 s for the dongle
to appear (useful at boot or right after plugging in).

## Use it from an AI assistant

`tdongle --skill` prints an Agent Skill that teaches an assistant (Claude Code and others) to drive it:

```sh
mkdir -p ~/.claude/skills/tdongle && tdongle --skill > ~/.claude/skills/tdongle/SKILL.md
```

Rerun it after upgrading. The skill is generated from the same tables as `--help`, including how
the port resolves on this machine.

## Configuration

There is no config file. Settings resolve in this order, first match winning:

1. command-line flags (`--port`, `--timeout`, image flags)
2. the environment: `TDONGLE_PORT`, plus `NO_COLOR` for colour
3. `/dev/tdongle`, if the udev rule is installed
4. built-in defaults (autodetect, 2000 ms timeout, `--fit --rot auto --bg black`)

Brightness and the screen's sleep timeout are stored **on the dongle** (`tdongle brightness`,
`tdongle sleep N`), so they follow it to other machines.

## Caching

`tdongle` never uses the network, so there is nothing to cache.

## Output notes

- **stdout is only the result.** Drawing commands print nothing on success; `send` prints the raw
  replies; queries print a short report. Errors are one line on stderr: `tdongle: <message>`.
- **Colour** is on for terminals and off for pipes; `--color always|never`, `--no-color`, `NO_COLOR`
  and `TERM=dumb` are honoured.
- **`--json`** prints one pretty-printed object with a stable shape, ending in `generated_at` (UTC).
  The first field is the answer: `connected` for status, `ok` for drawing and images,
  `temperature_c`, `count`. `tdongle --skill` documents every shape with examples.
- **Exit codes:** `0` success, `1` runtime failure (no dongle, permission denied, no reply, an `ERR`
  reply, an unreadable image), `2` bad usage, `130` interrupted, `141` stdout closed early.
  If some lines of a `send` fail, the replies so far are printed and it exits 1.

## About the firmware

```
your program ── /dev/ttyACM0 (cdc_acm, a standard kernel driver) ── USB ──► ESP32-S3 USB-Serial/JTAG
                                                                           │
                                     firmware: read line → parse → LovyanGFX → SPI → ST7735 LCD
```

Linux sees an ordinary USB serial port, not a display. The firmware does all the drawing, so the
host sends short text commands (and, for images, raw pixels).

**Flashing without a button.** The firmware uses the ESP32-S3's built-in USB-Serial/JTAG
peripheral (`USBMode=hwcdc`), not TinyUSB. That peripheral is fixed hardware and keeps working even
if the app crashes, and esptool can always reset the chip into the ROM bootloader through it. The
USB id is the same in the app and the bootloader: `303a:1001`, serial = chip MAC. A dongle running
firmware that uses native USB (some LilyGO examples) needs BOOT held while plugging it in once.

**DTR/RTS.** On this port, DTR low with RTS high resets the chip. The kernel raises both on open,
which is harmless, so `echo > /dev/ttyACM0`, `cat` and `tdongle` never reset it. pyserial's
`ser.dtr = False`, and terminal programs that toggle the lines, will.

**Power.** The CPU runs at 80 MHz. The screen sleeps after 5 idle minutes (backlight off, panel
asleep) and wakes on the next drawing command; queries don't wake it. Automatic light sleep is not
used: it stops the USB-Serial/JTAG clock, so the port would vanish and button-free flashing would break.

### Protocol

One command per line (`\n` or `\r`); every line gets exactly one reply, `OK…` or `ERR <reason>`.
A line that isn't a command is shown as a message. Coordinates are pixels; rotation 0 is portrait 80×160.

| Command | Effect |
|---|---|
| `MSG <text>` | clear, then draw the text as large as fits, centered and wrapped |
| `CMSG <fg> <bg> <text>` | same, with colors |
| `CLEAR [color]` | fill the screen (default black) |
| `TEXT x y size color <text>` | text at x,y (size 1–8, 6×8 px per character at size 1) |
| `LOG <text>` | append a line to a scrolling console |
| `PIXEL x y c` · `LINE x0 y0 x1 y1 c` · `RECT x y w h c` · `FILL x y w h c` | shapes |
| `CIRCLE x y r c` · `FCIRCLE x y r c` | circles |
| `ROT 0-3` | rotation (1/3 = landscape 160×80); clears the screen |
| `BL 0-255` | backlight (saved) |
| `SLEEP` · `SLEEP <secs>` | screen off now · idle timeout, 0 = never (saved, default 300) |
| `WAKE` | wake the screen (any drawing command also wakes it) |
| `LED color [0-31]` | onboard APA102 RGB LED |
| `SPLASH` | redraw the boot screen |
| `PING` → `PONG` · `INFO` · `TEMP` · `HELP` | queries |
| `IMG x y w h` | binary image upload, below |
| `GPIO pin 0\|1` | debug: drive a pin |

**`IMG`:** the host sends `IMG x y w h` (at most 12800 pixels) and the device replies `READY <chunk>`.
The host then sends `w*h*2` bytes of RGB565, big-endian, rows top to bottom, in pieces of at most
`<chunk>` bytes, waiting for `NEXT` after each piece but the last; the device draws and replies `OK`
(or `ERR IMG: timeout…` after 2 s without data). The handshake is required: the USB-Serial/JTAG
driver drops data when its receive buffer is full instead of making the host wait.

**Extras.** 303a:1001 is shared by every ESP32 in USB-Serial/JTAG mode; to match only your dongle,
add `ATTRS{serial}=="<serial from tdongle list>",` to the udev rule.
[`extras/tdongle-hello@.service`](extras/tdongle-hello@.service) draws the hostname and time
whenever the dongle is plugged in.

## Development

```
cli/                        Rust CLI, std only (src/cli.rs parses; src/spec.rs is the one table
                            behind the parser, --help and --skill)
firmware/tdongle_display/   Arduino sketch (esp32 core 3.3.11, LovyanGFX 1.2.28)
setup.sh                    installer (POSIX sh)
udev/70-tdongle.rules       the rule setup.sh installs if needed
scripts/                    firmware packaging and the version header
```

```sh
cd cli && cargo test                   # offline unit tests (parser, help/skill sync, JSON, colour)
cd cli && cargo fmt --check && cargo clippy --all-targets -- -D warnings
make flash                             # build + flash the firmware with arduino-cli (no button)
make dist                              # CLI + firmware package into dist/
make install                           # setup.sh --from dist
```

The version lives in one place, `cli/Cargo.toml`; `scripts/version-header.sh` writes it into the
firmware at build time. GitHub Actions runs the checks on every push and, for a `v*` tag matching
that version, publishes a release: static musl CLIs for x86_64/aarch64/armv7, the firmware
package, `setup.sh` and `SHA256SUMS`.

## License

MIT
