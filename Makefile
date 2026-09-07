# RondOS build system (Linux toolchain)
#
#   make            - debug build of kernel + loader + disk image
#   make release    - release build of kernel
#   make run        - build then launch in QEMU
#   make clean      - remove all build artifacts

NASM       ?= nasm
QEMU       ?= qemu-system-i386
CARGO      ?= $(HOME)/.cargo/bin/cargo

KERNEL_DIR := kernel
KERNEL_ELF := $(KERNEL_DIR)/target/i686-unknown-none/debug/kernel
KERNEL_ELF_REL := $(KERNEL_DIR)/target/i686-unknown-none/release/kernel

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
endif

# Cargo options for nightly JSON target spec.
CARGO_NIGHTLY_OPTS := -Z json-target-spec

.PHONY: all kernel loader disk run clean release

all: $(DISK_IMG)

release: $(DISK_IMG)

kernel:
	cd $(KERNEL_DIR) && $(CARGO) build $(CARGO_FLAG) $(CARGO_NIGHTLY_OPTS)

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
	cd $(KERNEL_DIR) && $(CARGO) clean