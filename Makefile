# RondOS build system (Linux toolchain)
#
#   make            - debug build of kernel + loader + disk image
#   make release    - release build of kernel
#   make run        - build then launch in QEMU
#   make clean      - remove all build artifacts
#
#   x86-64 migration scaffold (docs/user-mode-design.md §7, M0.1/M0.2):
#   make kernel64   - build the x86-64 kernel
#   make run64      - boot it in QEMU (32-bit multiboot trampoline + loader device)
#   make test64     - headless boot, fail unless the smoke tests pass

NASM       ?= nasm
QEMU       ?= qemu-system-i386
QEMU64     ?= qemu-system-x86_64
CARGO      ?= $(HOME)/.cargo/bin/cargo

KERNEL_DIR := kernel
KERNEL_ELF := $(KERNEL_DIR)/target/i686-unknown-none/debug/kernel
KERNEL_ELF_REL := $(KERNEL_DIR)/target/i686-unknown-none/release/kernel

# x86-64 kernel (M0 scaffold; replaces kernel/ at M0.8)
K64_DIR     := kernel64
K64_TARGET  := x86_64-unknown-none
K64_ELF     := $(K64_DIR)/target/$(K64_TARGET)/debug/kernel64
K64_ELF_REL := $(K64_DIR)/target/$(K64_TARGET)/release/kernel64
K64_ELF_REAL := $(K64_ELF)
K64_BOOT32  := $(K64_DIR)/build/trampoline.bin
K64_LOG     := $(K64_DIR)/build/serial.log

DISK_IMG   := disk.img
DISK_SIZE_MIB := 2

# Boot tar image: appended after the kernel at a fixed 512 KiB offset
# (kernel must fit below it; see fs/mod.rs TAR_OFFSET_LBA = 1024).
TAR_IMG    := files.tar
TAR_OFFSET := 524288            # 512 KiB in bytes

STAGE1_BIN := loader/stage1.bin
STAGE2_BIN := loader/stage2.bin
LOADER_BIN := loader.bin

CARGO_FLAG ?=
KERNEL_ELF_REAL := $(KERNEL_ELF)

ifeq ($(filter release,$(MAKECMDGOALS)),release)
    CARGO_FLAG := --release
    KERNEL_ELF_REAL := $(KERNEL_ELF_REL)
    K64_ELF_REAL := $(K64_ELF_REL)
endif

# Cargo options for nightly JSON target spec.
CARGO_NIGHTLY_OPTS := -Z json-target-spec

.PHONY: all kernel loader disk run clean release kernel64 run64 test64

all: $(DISK_IMG)

release: $(DISK_IMG)

kernel:
	cd $(KERNEL_DIR) && $(CARGO) build $(CARGO_FLAG) $(CARGO_NIGHTLY_OPTS)

# ------------------------------------------------------- x86-64 M0 scaffold

kernel64:
	cd $(K64_DIR) && $(CARGO) build $(CARGO_FLAG)

# The 32-bit trampoline needs the kernel's entry address baked in.
$(K64_BOOT32): $(K64_DIR)/boot/multiboot32.s $(K64_ELF_REAL)
	@mkdir -p $(K64_DIR)/build
	ENTRY=$$(readelf -h $(K64_ELF_REAL) | awk '/Entry point/{print $$4}'); \
	  echo "==> trampoline entry $$ENTRY"; \
	  $(NASM) -f bin -DENTRY_HI=$$ENTRY -o $@ $<

# QEMU cannot boot a 64-bit ELF with -kernel (multiboot is 32-bit only), so:
#   -kernel trampoline.bin   boots the 32-bit trampoline
#   -device loader,file=...  places the kernel's ELF64 segments at their p_paddr
run64: kernel64 $(K64_BOOT32)
	$(QEMU64) -cpu max -m 512 \
	  -kernel $(K64_BOOT32) \
	  -device loader,file=$(K64_ELF_REAL) \
	  -serial stdio -display none -no-reboot

test64: kernel64 $(K64_BOOT32)
	@rm -f $(K64_LOG)
	@timeout 30 $(QEMU64) -cpu max -m 512 \
	  -kernel $(K64_BOOT32) \
	  -device loader,file=$(K64_ELF_REAL) \
	  -serial file:$(K64_LOG) -display none -no-reboot >/dev/null 2>&1 || true
	@cat $(K64_LOG)
	@grep -q "smoke: ALL PASS" $(K64_LOG) \
	  && echo "==> M0.1/M0.2/M0.3 smoke tests PASS" \
	  || (echo "==> M0.1/M0.2/M0.3 smoke tests FAIL"; exit 1)

$(STAGE1_BIN): loader/stage1.s
	$(NASM) -f bin $< -o $@ -l loader/stage1.lst

$(STAGE2_BIN): loader/stage2.s
	$(NASM) -f bin $< -o $@ -l loader/stage2.lst

loader: $(STAGE1_BIN) $(STAGE2_BIN)

$(LOADER_BIN): $(STAGE1_BIN) $(STAGE2_BIN)
	cat $(STAGE1_BIN) $(STAGE2_BIN) > $@

# ustar archive of files/ (short paths only; our kernel tar parser is simple).
$(TAR_IMG): $(shell find files -type f)
	tar --format=ustar -C files -cf $@ .

# disk.img layout:
#   sector 0                  : Stage1 (MBR)
#   sectors 1..32             : Stage2 (16 KiB)
#   sectors 33..              : Kernel ELF (as-is)
#   kernel padded to 512 KiB  : (boot tar image starts at LBA 1024)
#   tar archive               : read into ramfs at boot
#   padding to DISK_SIZE_MIB
$(DISK_IMG): $(LOADER_BIN) kernel $(TAR_IMG)
	@echo "==> Building $@"
	@rm -f $@
	# Stage1 (must fit in 1 sector)
	dd if=$(STAGE1_BIN) of=$@ bs=512 conv=notrunc status=none
	# Pad to sector 1
	truncate -s 512 $@
	# Stage2 (32 sectors = 16 KiB)
	cat $(STAGE2_BIN) >> $@
	truncate -s $$((33 * 512)) $@
	# Kernel ELF (raw, as produced by rust-lld)
	cat $(KERNEL_ELF_REAL) >> $@
	# The kernel must stay below the tar offset.
	@test $$(stat -c%s $(KERNEL_ELF_REAL)) -le $$(( $(TAR_OFFSET) - 33*512 )) \
		|| (echo "kernel too large for tar offset"; exit 1)
	# Pad to tar offset, append the boot tar image.
	truncate -s $(TAR_OFFSET) $@
	cat $(TAR_IMG) >> $@
	# Pad to whole disk
	truncate -s $$(( $(DISK_SIZE_MIB) * 1024 * 1024 )) $@
	@echo "==> Built $@"
	@ls -l $@

run: $(DISK_IMG)
	$(QEMU) -drive format=raw,media=disk,file=$(DISK_IMG) -serial stdio -monitor none

clean:
	rm -f $(STAGE1_BIN) $(STAGE2_BIN) $(LOADER_BIN) $(DISK_IMG) $(TAR_IMG)
	rm -f loader/*.lst kernel.bin
	rm -rf $(K64_DIR)/build
	cd $(KERNEL_DIR) && $(CARGO) clean
	cd $(K64_DIR) && $(CARGO) clean