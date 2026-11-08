#!/bin/sh
# Set up a LilyGO T-Dongle-S3 as a serial display.
#
#   curl -fsSL https://github.com/miketweaver/t-dongle-display/releases/latest/download/setup.sh | sh
#   sh setup.sh [options]
#
# What it does, touching the OS as little as possible:
#   1. Installs the `tdongle` CLI (a static binary) to ~/.local/bin. No root needed.
#   2. Only if your user can't open the serial port: installs one udev rule,
#      /etc/udev/rules.d/70-tdongle.rules (asks for sudo first).
#   3. Flashes the tdongle-display firmware with a pinned, checksum-verified esptool
#      that is downloaded to a temp dir and deleted afterwards. No button needed.
#   4. Checks the dongle answers, and says hello on its screen.
#
# Options:
#   --port DEV        use this serial device instead of autodetecting
#   --version vX.Y.Z  install a specific release (default: latest)
#   --bin-dir DIR     where to put the CLI (default: $XDG_BIN_HOME or ~/.local/bin)
#   --no-flash        don't flash firmware
#   --force-flash     flash even if the same firmware version is already running
#   --no-udev         never install the udev rule
#   --from DIR        use local build artifacts in DIR instead of downloading a release
#   -y, --yes         don't ask before using sudo
#   --uninstall       remove the CLI and the udev rule
#   -h, --help
set -eu

REPO="miketweaver/t-dongle-display"
RULE_PATH="/etc/udev/rules.d/70-tdongle.rules"
ESPTOOL_VER="v5.6.0"
VID="303a"
PID="1001"

PORT="" VERSION="latest" BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}" FROM=""
DO_FLASH=1 FORCE_FLASH=0 DO_UDEV=1 YES=0 UNINSTALL=0

# ------------------------------------------------------------------ output --
if [ -t 1 ]; then B=$(printf '\033[1m'); G=$(printf '\033[32m'); Y=$(printf '\033[33m'); R=$(printf '\033[31m'); N=$(printf '\033[0m')
else B="" G="" Y="" R="" N=""; fi
step() { printf '%s==>%s %s\n' "$B" "$N" "$*"; }
ok()   { printf '  %s✓%s %s\n' "$G" "$N" "$*"; }
warn() { printf '  %s!%s %s\n' "$Y" "$N" "$*" >&2; }
die()  { printf '%serror:%s %s\n' "$R" "$N" "$*" >&2; exit 1; }

usage() {
  if [ -f "$0" ] && grep -q 'T-Dongle-S3 as a serial display' "$0" 2>/dev/null; then
    sed -n '2,/^set -eu/p' "$0" | sed '$d;s/^# \{0,1\}//'
  else
    echo "Usage: setup.sh [--port DEV] [--version vX.Y.Z] [--bin-dir DIR] [--no-flash] [--force-flash]"
    echo "                [--no-udev] [--from DIR] [--yes] [--uninstall]"
    echo "Docs: https://github.com/$REPO"
  fi
  exit 0
}

while [ $# -gt 0 ]; do
  case $1 in
    --port) PORT=${2:?--port needs a value}; shift ;;
    --version) VERSION=${2:?--version needs a value}; shift ;;
    --bin-dir) BIN_DIR=${2:?--bin-dir needs a value}; shift ;;
    --from) FROM=${2:?--from needs a value}; shift ;;
    --no-flash) DO_FLASH=0 ;;
    --force-flash) FORCE_FLASH=1 ;;
    --no-udev) DO_UDEV=0 ;;
    -y|--yes) YES=1 ;;
    --uninstall) UNINSTALL=1 ;;
    -h|--help) usage ;;
    *) die "unknown option: $1 (see --help)" ;;
  esac
  shift
done

[ "$(uname -s)" = Linux ] || die "Linux only"

# ------------------------------------------------------------------ helpers --
# Read answers from the terminal even when the script itself arrives on stdin (curl | sh).
ask() {
  [ "$YES" = 1 ] && return 0
  if [ -r /dev/tty ] && [ -w /dev/tty ]; then
    printf '  %s [Y/n] ' "$1" > /dev/tty
    read -r a < /dev/tty || a=n
    case $a in ''|y|Y|yes|YES) return 0 ;; *) return 1 ;; esac
  fi
  warn "no terminal to ask on; pass --yes to allow: $1"
  return 1
}

as_root() {
  if [ "$(id -u)" = 0 ]; then "$@"
  elif command -v sudo >/dev/null 2>&1; then sudo "$@"
  elif command -v doas >/dev/null 2>&1; then doas "$@"
  else die "need root for: $* (no sudo or doas found)"; fi
}

fetch() { # url dest
  if command -v curl >/dev/null 2>&1; then curl -fsSL --retry 3 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then wget -q -O "$2" "$1"
  else die "need curl or wget"; fi
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
  elif command -v openssl >/dev/null 2>&1; then openssl dgst -sha256 -r "$1" | cut -d' ' -f1
  else echo ""; fi
}

verify() { # file expected-sha
  got=$(sha256_of "$1")
  [ -n "$got" ] || { warn "no sha256 tool found; skipping checksum of $(basename "$1")"; return 0; }
  [ "$got" = "$2" ] || die "checksum mismatch for $(basename "$1"): expected $2, got $got"
}

# All ttyACM devices with the ESP32-S3 USB-Serial/JTAG id.
find_ports() {
  for d in /sys/class/tty/ttyACM*; do
    [ -e "$d/device" ] || continue
    usb=$(readlink -f "$d/device/..")
    [ "$(cat "$usb/idVendor" 2>/dev/null)" = "$VID" ] && [ "$(cat "$usb/idProduct" 2>/dev/null)" = "$PID" ] \
      && echo "/dev/$(basename "$d")"
  done
  return 0
}

other_espressif_usb() {
  for d in /sys/bus/usb/devices/*; do
    [ "$(cat "$d/idVendor" 2>/dev/null)" = "$VID" ] && [ "$(cat "$d/idProduct" 2>/dev/null)" != "$PID" ] && return 0
  done
  return 1
}

case $(uname -m) in
  x86_64|amd64) ARCH=x86_64 ESPTOOL_ARCH=amd64 ESPTOOL_SHA=1590723cfa9675983f49d0bf2895b8507f8d2879082e51bb7ef02a8bca88d354 ;;
  aarch64|arm64) ARCH=aarch64 ESPTOOL_ARCH=aarch64 ESPTOOL_SHA=7723120429394b4d6d974d694e5cf378e00259381a7f8856c2c88492041b9f59 ;;
  armv7*|armv8l|armhf) ARCH=armv7 ESPTOOL_ARCH=armv7 ESPTOOL_SHA=65e2f91fba2e79a75243454ded44da57532c74e242e17f9890aa7fe6cd588ee4 ;;
  *) die "unsupported CPU architecture: $(uname -m) (x86_64, aarch64 and armv7 are supported)" ;;
esac

# ---------------------------------------------------------------- uninstall --
if [ "$UNINSTALL" = 1 ]; then
  step "Uninstalling"
  if [ -e "$BIN_DIR/tdongle" ]; then rm -f "$BIN_DIR/tdongle"; ok "removed $BIN_DIR/tdongle"; fi
  if [ -e "$RULE_PATH" ]; then
    if ask "Remove $RULE_PATH (needs sudo)?"; then
      as_root rm -f "$RULE_PATH"
      as_root udevadm control --reload-rules 2>/dev/null || true
      as_root udevadm trigger --action=change --subsystem-match=tty 2>/dev/null || true
      ok "removed $RULE_PATH"
    fi
  fi
  ok "done (the firmware stays on the dongle)"
  exit 0
fi

WORK=$(mktemp -d "${TMPDIR:-/tmp}/tdongle-setup.XXXXXX")
trap 'rm -rf "$WORK"' EXIT INT TERM

# ---------------------------------------------------------------- downloads --
CLI_ASSET="tdongle-$ARCH-linux"
if [ -n "$FROM" ]; then
  step "Using local artifacts from $FROM"
  cp "$FROM/$CLI_ASSET" "$FROM/tdongle-firmware.tar.gz" "$WORK/" || die "missing artifacts in $FROM"
else
  if [ "$VERSION" = latest ]; then BASE="https://github.com/$REPO/releases/latest/download"
  else BASE="https://github.com/$REPO/releases/download/$VERSION"; fi
  step "Downloading $VERSION release ($ARCH)"
  fetch "$BASE/SHA256SUMS" "$WORK/SHA256SUMS" || die "could not download $BASE/SHA256SUMS"
  for f in "$CLI_ASSET" tdongle-firmware.tar.gz; do
    fetch "$BASE/$f" "$WORK/$f" || die "could not download $BASE/$f"
    want=$(grep " \*\{0,1\}$f\$" "$WORK/SHA256SUMS" | cut -d' ' -f1)
    [ -n "$want" ] || die "$f missing from SHA256SUMS"
    verify "$WORK/$f" "$want"
  done
  ok "downloaded and verified"
fi

# ---------------------------------------------------------------------- CLI --
step "Installing the tdongle CLI"
mkdir -p "$BIN_DIR"
cp "$WORK/$CLI_ASSET" "$BIN_DIR/tdongle.new" && chmod 0755 "$BIN_DIR/tdongle.new" && mv -f "$BIN_DIR/tdongle.new" "$BIN_DIR/tdongle"
TD="$BIN_DIR/tdongle"
ok "$TD ($("$TD" --version))"
case ":$PATH:" in *":$BIN_DIR:"*) ;; *) warn "$BIN_DIR is not on your PATH; add it, or run $TD directly" ;; esac

# ------------------------------------------------------------------- device --
step "Looking for the dongle"
if [ -z "$PORT" ]; then
  i=0
  while :; do
    PORTS=$(find_ports)
    [ -n "$PORTS" ] && break
    if other_espressif_usb; then
      die "found an Espressif USB device that isn't in USB-Serial/JTAG mode (other firmware using native USB).
       Hold the dongle's BOOT button while plugging it in once, then rerun this script."
    fi
    [ $i = 0 ] && printf '  waiting for a T-Dongle-S3 to be plugged in (30 s)...\n'
    i=$((i + 1)); [ $i -gt 30 ] && die "no T-Dongle-S3 found (USB id 303a:1001)"
    sleep 1
  done
  n=$(printf '%s\n' "$PORTS" | wc -l)
  # shellcheck disable=SC2086
  [ "$n" -eq 1 ] || die "found $n ESP32 USB-Serial/JTAG devices: $(printf "%s " $PORTS)- pick one with --port."
  PORT=$PORTS
fi
[ -e "$PORT" ] || die "$PORT does not exist"
ok "$PORT"

# ------------------------------------------------------------------- access --
step "Checking access to $PORT"
if [ -r "$PORT" ] && [ -w "$PORT" ]; then
  ok "your user can already open it; no system changes needed"
else
  GROUP=$(stat -c %G "$PORT" 2>/dev/null || echo "?")
  [ "$DO_UDEV" = 1 ] || die "no access to $PORT. Add yourself to group '$GROUP' (and log in again), or rerun without --no-udev."
  echo "  Your user can't open $PORT (owned by group '$GROUP')."
  echo "  One udev rule fixes that: $RULE_PATH (the logged-in user gets access, plus a /dev/tdongle link)."
  ask "Install it with sudo?" || die "no access. Alternative: add yourself to group '$GROUP' and log in again."
  cat > "$WORK/70-tdongle.rules" <<'EOF'
# T-Dongle-S3 running tdongle-display firmware (https://github.com/miketweaver/t-dongle-display)
# Installed by setup.sh; remove with `setup.sh --uninstall`.
#  SYMLINK  /dev/tdongle
#  uaccess  the logged-in desktop user gets read/write access (must run before 73-seat-late.rules)
#  ID_MM_.. keep ModemManager from probing it with AT commands
SUBSYSTEM=="tty", ATTRS{idVendor}=="303a", ATTRS{idProduct}=="1001", \
  SYMLINK+="tdongle", TAG+="uaccess", ENV{ID_MM_DEVICE_IGNORE}="1"
EOF
  as_root install -m 0644 "$WORK/70-tdongle.rules" "$RULE_PATH"
  as_root udevadm control --reload-rules
  as_root udevadm trigger --action=change --sysname-match="$(basename "$(readlink -f "$PORT")")"
  as_root udevadm settle --timeout=5 || true
  if [ -r "$PORT" ] && [ -w "$PORT" ]; then
    ok "installed $RULE_PATH; access granted"
  else
    die "rule installed, but still no access (your system may not use systemd-logind/elogind seats).
       Add yourself to group '$GROUP' and log in again:  sudo usermod -aG $GROUP $(id -un)"
  fi
fi

# -------------------------------------------------------------------- flash --
mkdir -p "$WORK/fw"
tar xzf "$WORK/tdongle-firmware.tar.gz" -C "$WORK/fw"
FW_VER=$(cat "$WORK/fw/VERSION" 2>/dev/null || echo "?")
CUR=$("$TD" -p "$PORT" -t 800 info 2>/dev/null | awk '$2=="tdongle-display"{print $3}') || true

if [ "$DO_FLASH" = 0 ]; then
  step "Skipping firmware flash (--no-flash)"
elif [ "$CUR" = "$FW_VER" ] && [ "$FORCE_FLASH" = 0 ]; then
  step "Firmware $FW_VER already running; skipping flash (--force-flash to reflash)"
else
  step "Flashing firmware $FW_VER${CUR:+ (currently $CUR)}"
  if command -v esptool >/dev/null 2>&1 && esptool version 2>/dev/null | grep -q '^v\{0,1\}5\.'; then
    ESPTOOL=esptool
    ok "using esptool from PATH"
  else
    t="esptool-$ESPTOOL_VER-linux-$ESPTOOL_ARCH.tar.gz"
    fetch "https://github.com/espressif/esptool/releases/download/$ESPTOOL_VER/$t" "$WORK/$t" \
      || die "could not download esptool $ESPTOOL_VER"
    verify "$WORK/$t" "$ESPTOOL_SHA"
    tar xzf "$WORK/$t" -C "$WORK"
    ESPTOOL="$WORK/esptool-linux-$ESPTOOL_ARCH/esptool"
    ok "esptool $ESPTOOL_VER (temporary, verified)"
  fi
  # The USB-Serial/JTAG peripheral lets esptool reset the chip into the ROM
  # bootloader itself: no BOOT button, even if the running app is broken.
  # Only bootloader, partition table, boot_app0 and app are written; saved settings survive.
  # shellcheck disable=SC2046
  ( cd "$WORK/fw" && "$ESPTOOL" --chip esp32s3 --port "$PORT" --before default-reset --after hard-reset \
      write-flash $(cat flash_args) ) > "$WORK/flash.log" 2>&1 \
    || { cat "$WORK/flash.log" >&2; die "flashing failed (log above)"; }
  ok "flashed"
fi

# ------------------------------------------------------------------- verify --
step "Talking to the dongle"
INFO=$("$TD" -p "$PORT" -w 15 info) || die "the dongle didn't answer on $PORT"
ok "$INFO"
"$TD" -p "$PORT" -q msg "Hello $(hostname 2>/dev/null || uname -n)" || true

DEV=$PORT; [ -e /dev/tdongle ] && DEV=/dev/tdongle
printf '\n%sReady.%s Try:\n' "$G" "$N"
printf '  tdongle "Hello world"\n  tdongle send "CLEAR navy" "TEXT 4 4 2 yellow Hi"\n  echo "MSG from the shell" > %s\n  tdongle --help\n' "$DEV"
