# RondOS build system (x86-64 + UEFI)
#
#   make            - build the kernel, the UEFI stub and the ESP tree
#   make release    - release build
#   make run        - build then boot the ESP in QEMU + OVMF (serial on stdio)
#   make test       - headless boot; fail unless the smoke tests pass
#   make clean      - remove all build artifacts
#
# The i686 kernel and its NASM BIOS loader are preserved on the `legacy-i686`
# git branch (see README.md).  The 32-bit trampoline that M0.7 replaced lives
# in git history only.

QEMU       ?= qemu-system-x86_64
CARGO      ?= $(HOME)/.cargo/bin/cargo
OVMF       ?= /usr/share/ovmf/OVMF.fd

KERNEL_DIR := kernel64
BOOT_DIR   := boot/uefi
USER_DIR   := user
K64_TARGET := x86_64-unknown-none
UEFI_TARGET:= x86_64-unknown-uefi
USER_TARGET:= x86_64-rondos

ESP        := build/esp
ESP_KERNEL := $(ESP)/rondos/kernel.elf
ESP_STUB   := $(ESP)/EFI/BOOT/BOOTX64.EFI
ESP_TAR    := $(ESP)/rondos/boot.tar
ESP_TMP    := build/tmp
BOOT_TAR   := build/boot.tar
USER_BIN   = $(USER_DIR)/target/$(USER_TARGET)/$(PROFILE)
SERIAL_LOG := build/uefi-serial.log

CARGO_FLAG ?=
PROFILE    := debug
ifeq ($(filter release,$(MAKECMDGOALS)),release)
    CARGO_FLAG := --release
    PROFILE    := release
endif

QEMU_FLAGS := -bios $(OVMF) -m 512 \
              -drive file=fat:rw:$(ESP),format=raw \
              -display none -no-reboot

.PHONY: all release user kernel boot esp run test clean

all: esp

release: all

user:
	cd $(USER_DIR) && $(CARGO) +nightly build $(CARGO_FLAG) -p init -p crash -p spin

kernel:
	cd $(KERNEL_DIR) && $(CARGO) build $(CARGO_FLAG)

boot:
	cd $(BOOT_DIR) && $(CARGO) build $(CARGO_FLAG)

# The boot tar is what the UEFI stub hands over as BootInfo.initrd: the kernel
# finds /bin/* inside it (ustar, flat names).
$(BOOT_TAR): user
	@mkdir -p build
	python3 tools/mktar.py $@ \
	  bin/init=$(USER_BIN)/init \
	  bin/crash=$(USER_BIN)/crash \
	  bin/spin=$(USER_BIN)/spin

# The ESP is a directory; QEMU's vvfat exposes it as a FAT drive, so testing
# needs neither mkfs.vfat nor mtools.  TMPDIR is pinned inside the tree because
# vvfat writes a scratch file next to the image.
esp: kernel boot $(BOOT_TAR)
	@mkdir -p $(ESP)/EFI/BOOT $(ESP)/rondos $(ESP_TMP)
	cp $(KERNEL_DIR)/target/$(K64_TARGET)/$(PROFILE)/kernel64 $(ESP_KERNEL)
	cp $(BOOT_DIR)/target/$(UEFI_TARGET)/$(PROFILE)/rondos-boot.efi $(ESP_STUB)
	cp $(BOOT_TAR) $(ESP_TAR)

run: esp
	TMPDIR=$(CURDIR)/$(ESP_TMP) $(QEMU) $(QEMU_FLAGS) -serial stdio

test: esp
	@rm -f $(SERIAL_LOG)
	@TMPDIR=$(CURDIR)/$(ESP_TMP) timeout 30 $(QEMU) $(QEMU_FLAGS) \
	  -serial file:$(SERIAL_LOG) >/dev/null 2>&1 || true
	@grep -v '^\[2J' $(SERIAL_LOG)
	@grep -q "bootinfo: adopted UEFI structure" $(SERIAL_LOG) \
	  || (echo "==> booted, but not through the UEFI stub"; exit 1)
	@grep -q "smoke: ALL PASS" $(SERIAL_LOG) \
	  && echo "==> smoke tests PASS (UEFI)" \
	  || (echo "==> smoke tests FAIL"; exit 1)
	@grep -q "user: init: hello from ring 3" $(SERIAL_LOG) \
	  && echo "==> init reached ring 3" \
	  || (echo "==> init did not run"; exit 1)
	@grep -q "user: init: all children reaped, exiting" $(SERIAL_LOG) \
	  && echo "==> init spawned, waited for and killed its children" \
	  || (echo "==> init child handling failed"; exit 1)

clean:
	rm -rf build
	cd $(KERNEL_DIR) && $(CARGO) clean
	cd $(BOOT_DIR) && $(CARGO) clean
	cd $(USER_DIR) && $(CARGO) clean
