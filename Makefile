# Developer targets. End users: run setup.sh instead.
FQBN  := esp32:esp32:esp32s3:USBMode=hwcdc,CDCOnBoot=cdc,FlashSize=16M,PartitionScheme=app3M_fat9M_16MB
PORT  ?= $(firstword $(wildcard /dev/tdongle) /dev/ttyACM0)
SKETCH := firmware/tdongle_display
TARGET ?= x86_64-unknown-linux-musl

.PHONY: firmware flash cli dist install monitor clean

firmware:
	arduino-cli compile -b "$(FQBN)" $(SKETCH)

# esptool resets the S3 into the ROM bootloader over USB-Serial/JTAG: no button needed.
flash:
	arduino-cli compile -b "$(FQBN)" --upload -p $(PORT) $(SKETCH)

cli:
	cd cli && cargo build --release --locked --target $(TARGET)

# Same artifacts a release has, for `setup.sh --from dist`.
dist: cli
	scripts/package-firmware.sh
	cp cli/target/$(TARGET)/release/tdongle dist/tdongle-$(firstword $(subst -, ,$(TARGET)))-linux

install: dist
	sh setup.sh --from dist

monitor:
	python3 -m serial.tools.miniterm --raw $(PORT) 115200

clean:
	rm -rf build dist cli/target
