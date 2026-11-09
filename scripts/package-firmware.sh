#!/bin/sh
# Build the firmware with arduino-cli and package it as dist/tdongle-firmware.tar.gz:
#   bootloader + partition table + boot_app0 + app + flash_args + VERSION
# (not the 16 MB merged image: that would also erase saved settings in NVS).
set -eu
cd "$(dirname "$0")/.."
FQBN="esp32:esp32:esp32s3:USBMode=hwcdc,CDCOnBoot=cdc,FlashSize=16M,PartitionScheme=app3M_fat9M_16MB"
BUILD=build/firmware
VERSION=$(scripts/version-header.sh)   # from cli/Cargo.toml

rm -rf "$BUILD" && mkdir -p "$BUILD" dist
arduino-cli compile -b "$FQBN" --build-path "$BUILD" firmware/tdongle_display

PKG="$BUILD/pkg"
mkdir -p "$PKG"
cp "$BUILD/tdongle_display.ino.bootloader.bin" "$PKG/bootloader.bin"
cp "$BUILD/tdongle_display.ino.partitions.bin" "$PKG/partitions.bin"
cp "$BUILD/boot_app0.bin" "$PKG/boot_app0.bin"
cp "$BUILD/tdongle_display.ino.bin" "$PKG/app.bin"
# Same flash settings and offsets the Arduino core uses (see its generated flash_args).
head -n1 "$BUILD/flash_args" > "$PKG/flash_args"
printf '0x0 bootloader.bin\n0x8000 partitions.bin\n0xe000 boot_app0.bin\n0x10000 app.bin\n' >> "$PKG/flash_args"
echo "$VERSION" > "$PKG/VERSION"
tar czf dist/tdongle-firmware.tar.gz -C "$PKG" .
echo "dist/tdongle-firmware.tar.gz (firmware $VERSION):"
cat "$PKG/flash_args"
