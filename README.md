# t-dongle-display

Turn a [LilyGO T-Dongle-S3](https://github.com/Xinyuan-LilyGO/T-Dongle-S3) (ESP32-S3 with a 0.96" 80×160 LCD)
into a tiny USB status display for any Linux machine. It shows up as a plain serial port.
You write lines of text to it, and it draws them.

```sh
tdongle "Build passed"
tdongle send "CLEAR navy" "TEXT 4 4 2 yellow Hi" "FCIRCLE 40 80 20 orange"
tdongle image photo.jpg
echo "MSG no tools needed" > /dev/ttyACM0
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

1. Installs the `tdongle` CLI to `~/.local/bin` (no root). The CLI is a 450 KB static binary with no dependencies.
2. Checks whether your user can open the serial port. Most distros already allow this
   (`dialout`/`uucp` group, or a logged-in seat). **If it can't**, it asks to install one udev
   rule, `/etc/udev/rules.d/70-tdongle.rules`, which gives the logged-in user access and adds `/dev/tdongle`.
   Nothing else on the system changes.
3. Flashes the firmware. It downloads a pinned, checksum-verified esptool into a temp dir and
   deletes it afterwards. **No BOOT button needed** (see below). It skips the flash if the same version is already running.
4. Says hello on the screen.

Options: `--port DEV`, `--version vX.Y.Z`, `--bin-dir DIR`, `--no-flash`, `--force-flash`,
`--no-udev`, `--yes`, `--uninstall`. `sh setup.sh --uninstall` removes the CLI and the udev rule.

## Usage

```
tdongle <text...>                       big centered, word-wrapped message (same as `msg`)
tdongle log <text...>                   scrolling console line
tdongle text <x> <y> <size> <color> <text...>
tdongle clear [color]
tdongle image <file|-> [--fit|--fill|--stretch] [--rot auto|keep|0-3] [--bg color] [--no-dither] [--save out.ppm]
tdongle send <line>... | -              raw protocol lines, `-` = from stdin
tdongle info | ping | temp
tdongle sleep [secs] | wake | bl <0-255> | led <color> [0-31]
tdongle list                            connected dongles + firmware version

-p/--port DEV   -t/--timeout MS   -w/--wait SECS   -q/--quiet
```

The port is found automatically. Order: `--port`, then `$TDONGLE_PORT`, then `/dev/tdongle`,
then any ttyACM device with USB id `303a:1001` (if there are several, the first one that answers).
Exit status: 0 ok, 1 the device replied `ERR`, 2 usage, 3 device/IO error, 4 no reply.

Examples:

```sh
tdongle -q log "$(date +%T) backup ok"                    # from cron
make && tdongle -q send "CLEAR darkgreen" "MSG ok" || tdongle -q send "CLEAR red" "MSG FAIL"
tdongle -w 10 msg "booted"                                # wait for the dongle (e.g. at boot)
```

## Images

```sh
tdongle image photo.jpg                  # letterboxed; wide images switch the screen to landscape
tdongle image logo.png --fill            # fill the screen, crop the overflow
tdongle image chart.png --rot keep       # keep the current rotation
curl -s https://example.com/x.png | tdongle image -
tdongle image photo.jpg --save preview.ppm   # see the result without a dongle
```

- **Built in:** PNG (every color type and bit depth, interlaced, transparency), baseline JPEG
  (including the EXIF orientation phones write), BMP, and PPM/PGM. The CLI decodes these itself, no libraries.
- **Other formats** (progressive JPEG, GIF, WebP, HEIC…) go through ImageMagick (`magick`/`convert`)
  or `ffmpeg` if one of them is installed.
- **Processing:** images are scaled with an area filter, composited over `--bg` where transparent, and
  Floyd–Steinberg dithered to the panel's RGB565 colors. A full-screen push takes about 0.3 s.
- `--rot auto` (the default) uses landscape (rotation 1) for images wider than tall, and portrait otherwise.
  Use `--rot 3` if your landscape image comes out upside down. The rotation stays in effect for later commands.

## How it works

```
your program ── /dev/ttyACM0 (cdc_acm, a standard kernel driver) ── USB ──► ESP32-S3 USB-Serial/JTAG
                                                                           │
                                     firmware: read line → parse → LovyanGFX → SPI → ST7735 LCD
```

Linux sees an ordinary USB serial port, not a display. The firmware does all the drawing,
so the host sends short text commands, never pixels.

### Flashing without a button

The firmware uses the ESP32-S3's **built-in USB-Serial/JTAG** peripheral (`USBMode=hwcdc`), not
TinyUSB. That peripheral is fixed hardware and keeps working even if the app crashes. esptool can
always reset the chip into the ROM bootloader through it, so you never need the BOOT button.
The USB id is the same in the app and in the bootloader: `303a:1001`, serial = chip MAC.

If the dongle has firmware that uses native USB (TinyUSB, e.g. some LilyGO examples),
hold BOOT while plugging it in once. After that, button-free flashing works.

### DTR/RTS: don't reset it by accident

On this port, **DTR low with RTS high resets the chip**. The kernel raises both lines on open,
which is harmless, so `echo > /dev/ttyACM0`, `cat` and the `tdongle` CLI never reset it.
pyserial's `ser.dtr = False`, and terminal programs that toggle the lines, will reboot it.

### Power

- CPU at 80 MHz.
- The screen sleeps after 5 minutes without drawing commands (backlight off, panel in sleep
  mode) and wakes on the next drawing command. `tdongle sleep 120` changes the timeout and
  `tdongle sleep 0` disables it (the setting is saved on the dongle). Queries (`info`, `temp`, `ping`)
  don't wake the screen.
- Automatic light sleep is not used. It stops the USB-Serial/JTAG clock, so the port would
  disappear from Linux and button-free flashing would break.

## Protocol

One command per line (`\n` or `\r`). Every line gets exactly one reply: `OK…` or `ERR <reason>`.
A line that isn't a command is shown as a message.
Colors: `black white red green blue yellow cyan magenta orange purple gray pink navy darkgreen brown`, or `RRGGBB` / `#RRGGBB`.
Coordinates are pixels. Rotation 0 is portrait 80×160.

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
| `IMG x y w h` | binary image upload, see below |
| `GPIO pin 0\|1` | debug: drive a pin |

### `IMG`: image upload

1. Host: `IMG x y w h\n` (at most 12800 pixels; a full screen is 80×160 or 160×80).
2. Device: `READY <chunk>`.
3. Host sends `w*h*2` bytes of RGB565, **big-endian** (high byte first), rows top to bottom, in
   pieces of at most `<chunk>` bytes. After each piece except the last, it waits for `NEXT`.
4. Device draws the image, then replies `OK`. If more than 2 s pass without data, it replies `ERR IMG: timeout…`.

The handshake is required. The ESP32-S3 USB-Serial/JTAG driver silently drops data when its
receive buffer is full, instead of making the host wait. Use a raw-mode tty (`stty raw -echo`),
or just `tdongle image`.

## Optional extras

- **Pin to one dongle:** 303a:1001 is shared by every ESP32 in USB-Serial/JTAG mode. Add
  `ATTRS{serial}=="<serial from tdongle list>",` to the udev rule.
- **Greet on plug-in:** [`extras/tdongle-hello@.service`](extras/tdongle-hello@.service) draws the
  hostname and time whenever the dongle is connected.

## Building from source

```
firmware/tdongle_display/   Arduino sketch (esp32 core 3.3.11, LovyanGFX 1.2.28)
cli/                        Rust CLI, std only
setup.sh                    installer (POSIX sh)
udev/70-tdongle.rules       the rule setup.sh installs if needed
scripts/package-firmware.sh builds dist/tdongle-firmware.tar.gz
```

```sh
make flash                  # build + flash firmware with arduino-cli (no button)
make dist                   # CLI + firmware package into dist/
make install                # setup.sh --from dist
```

Releases are built by GitHub Actions when a `v*` tag is pushed (static musl CLIs for
x86_64/aarch64/armv7, the firmware package, `setup.sh`, and `SHA256SUMS`).

## License

MIT
