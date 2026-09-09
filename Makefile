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
CC         ?= gcc
LD         ?= ld

# Minimal C support (P2b): freestanding, no libc, no PIE, our own crt0 + user.ld.
CFLAGS     := -ffreestanding -nostdlib -static -no-pie -fno-stack-protector \
              -fno-pic -mno-red-zone -mcmodel=small -O2 -Wall -Wextra

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
C_BUILD    := build/c
C_ELF      := $(C_BUILD)/chello.elf
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

.PHONY: all release user kernel boot cprogram esp run test clean

all: esp

release: all

user:
	cd $(USER_DIR) && $(CARGO) +nightly build $(CARGO_FLAG) -p init -p crash -p spin -p echo -p heap -p physcheck

cprogram: $(C_ELF)

kernel:
	cd $(KERNEL_DIR) && $(CARGO) build $(CARGO_FLAG)

boot:
	cd $(BOOT_DIR) && $(CARGO) build $(CARGO_FLAG)

# A C program: crt0.S + hello.c + rondos.h, linked with the same user.ld.
$(C_ELF): $(USER_DIR)/c/crt0.S $(USER_DIR)/c/hello.c $(USER_DIR)/c/rondos.c \
           $(USER_DIR)/c/rondos.h $(USER_DIR)/user.ld
	@mkdir -p $(C_BUILD)
	$(CC) -c -o $(C_BUILD)/crt0.o $(USER_DIR)/c/crt0.S
	$(CC) $(CFLAGS) -I $(USER_DIR)/c -c $(USER_DIR)/c/hello.c -o $(C_BUILD)/hello.o
	$(CC) $(CFLAGS) -I $(USER_DIR)/c -c $(USER_DIR)/c/rondos.c -o $(C_BUILD)/rondos.o
	$(LD) -T $(USER_DIR)/user.ld --no-pie -o $@ $(C_BUILD)/crt0.o $(C_BUILD)/hello.o $(C_BUILD)/rondos.o

# The boot tar is what the UEFI stub hands over as BootInfo.initrd: the kernel
# finds /bin/* inside it (ustar, flat names).
$(BOOT_TAR): user $(C_ELF)
	@mkdir -p build
	python3 tools/mktar.py $@ \
	  bin/init=$(USER_BIN)/init \
	  bin/crash=$(USER_BIN)/crash \
	  bin/spin=$(USER_BIN)/spin \
	  bin/echo=$(USER_BIN)/echo \
	  bin/heap=$(USER_BIN)/heap \
	  bin/physcheck=$(USER_BIN)/physcheck \
	  bin/chello=$(C_ELF) \
	  bin/hello.c=$(USER_DIR)/c/hello.c

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
	@command -v $(CC) >/dev/null || (echo "==> missing C compiler: $(CC)"; exit 1)
	@command -v python3 >/dev/null || (echo "==> missing python3 (tools/mktar.py)"; exit 1)
	@rm -f $(SERIAL_LOG)
	@# The kernel halts on purpose, so QEMU is always killed by the timeout;
	@# correctness is judged from the log below, not from the exit status.
	@TMPDIR=$(CURDIR)/$(ESP_TMP) timeout 40 $(QEMU) $(QEMU_FLAGS) \
	  -serial file:$(SERIAL_LOG) >/dev/null 2>&1 || true
	@grep -v '^\[2J' $(SERIAL_LOG)
	@! grep -qE "PANIC:|kernel #PF|kernel #GP|DOUBLE FAULT|\[FAIL\]" $(SERIAL_LOG) \
	  || (echo "==> kernel fault / failed report in the log"; exit 1)
	@grep -qE "smoke: ALL PASS \([0-9]+/[0-9]+\)" $(SERIAL_LOG) \
	  || (echo "==> smoke tests FAIL"; exit 1)
	@total=$$(grep -oE "smoke: ALL PASS \(([0-9]+)/" $(SERIAL_LOG) | grep -oE "[0-9]+" | head -1); \
	  if [ "$$total" -lt 21 ]; then echo "==> only $$total smoke reports ran"; exit 1; fi
	@grep -q "bootinfo: adopted UEFI structure" $(SERIAL_LOG) \
	  || (echo "==> booted, but not through the UEFI stub"; exit 1)
	@echo "==> smoke tests PASS (UEFI)"
	@grep -q "user: init: hello from ring 3" $(SERIAL_LOG) \
	  && echo "==> init reached ring 3" \
	  || (echo "==> init did not run"; exit 1)
	@grep -q "user: init: all children reaped, exiting" $(SERIAL_LOG) \
	  && echo "==> init spawned, waited for and killed its children" \
	  || (echo "==> init child handling failed"; exit 1)
	@grep -q "user: init: channel echoed" $(SERIAL_LOG) \
	  && echo "==> channel round-trip through a delegated capability" \
	  || (echo "==> channel echo failed"; exit 1)
	@grep -q "user: chello: hello from C on RondOS" $(SERIAL_LOG) \
	  && echo "==> C program ran" \
	  || (echo "==> C program failed"; exit 1)
	@grep -q "user: init: tmpfs file round-trips" $(SERIAL_LOG) \
	  && echo "==> tmpfs round-trip + readdir" \
	  || (echo "==> tmpfs failed"; exit 1)
	@grep -q "user: init: device capability enforced" $(SERIAL_LOG) \
	  && grep -q "user: physcheck: mem_map_phys denied" $(SERIAL_LOG) \
	  && echo "==> device capability enforced" \
	  || (echo "==> device capability check failed"; exit 1)
	@grep -q "user: init: handle passing ok" $(SERIAL_LOG) \
	  && grep -q "user: init: unlink ok" $(SERIAL_LOG) \
	  && echo "==> handle passing over a channel + seek/unlink" \
	  || (echo "==> P2d checks failed"; exit 1)
	@grep -q "user: heap: 2000-element Vec ok" $(SERIAL_LOG) \
	  && grep -q "user: chello: malloc/free ok" $(SERIAL_LOG) \
	  && echo "==> user heap (Rust alloc + C malloc)" \
	  || (echo "==> heap failed"; exit 1)

clean:
	rm -rf build
	cd $(KERNEL_DIR) && $(CARGO) clean
	cd $(BOOT_DIR) && $(CARGO) clean
	cd $(USER_DIR) && $(CARGO) clean
