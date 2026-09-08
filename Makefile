# RondOS build system (x86-64, UEFI-era scaffold)
#
#   make            - build the kernel + boot trampoline
#   make release    - release build
#   make run        - build then launch in QEMU
#   make test       - headless boot; fail unless the smoke tests pass
#   make clean      - remove all build artifacts
#
# The original i686 kernel and its NASM boot loader are preserved on the
# `legacy-i686` git branch (see README.md).

NASM       ?= nasm
QEMU       ?= qemu-system-x86_64
CARGO      ?= $(HOME)/.cargo/bin/cargo

KERNEL_DIR := kernel64
K64_TARGET := x86_64-unknown-none
K64_ELF    := $(KERNEL_DIR)/target/$(K64_TARGET)/debug/kernel64
K64_ELF_REL := $(KERNEL_DIR)/target/$(K64_TARGET)/release/kernel64
K64_ELF_REAL := $(K64_ELF)
K64_BOOT32 := $(KERNEL_DIR)/build/trampoline.bin
K64_LOG    := $(KERNEL_DIR)/build/serial.log

CARGO_FLAG ?=
ifeq ($(filter release,$(MAKECMDGOALS)),release)
    CARGO_FLAG := --release
    K64_ELF_REAL := $(K64_ELF_REL)
endif

.PHONY: all release kernel kernel64 trampoline run test clean

all: kernel trampoline

release: all

kernel kernel64:
	cd $(KERNEL_DIR) && $(CARGO) build $(CARGO_FLAG)

# The 32-bit trampoline needs the kernel's entry address baked in.
$(K64_BOOT32): $(KERNEL_DIR)/boot/multiboot32.s $(K64_ELF_REAL)
	@mkdir -p $(KERNEL_DIR)/build
	ENTRY=$$(readelf -h $(K64_ELF_REAL) | awk '/Entry point/{print $$4}'); \
	  echo "==> trampoline entry $$ENTRY"; \
	  $(NASM) -f bin -DENTRY_HI=$$ENTRY -o $@ $<

trampoline: $(K64_BOOT32)

# QEMU cannot boot a 64-bit ELF with -kernel (multiboot is 32-bit only), so:
#   -kernel trampoline.bin   boots the 32-bit trampoline
#   -device loader,file=...  places the kernel's ELF64 segments at their p_paddr
run: all
	$(QEMU) -cpu max -m 512 \
	  -kernel $(K64_BOOT32) \
	  -device loader,file=$(K64_ELF_REAL) \
	  -serial stdio -display none -no-reboot

test: all
	@rm -f $(K64_LOG)
	@timeout 30 $(QEMU) -cpu max -m 512 \
	  -kernel $(K64_BOOT32) \
	  -device loader,file=$(K64_ELF_REAL) \
	  -serial file:$(K64_LOG) -display none -no-reboot >/dev/null 2>&1 || true
	@cat $(K64_LOG)
	@grep -q "smoke: ALL PASS" $(K64_LOG) \
	  && echo "==> smoke tests PASS" \
	  || (echo "==> smoke tests FAIL"; exit 1)

clean:
	rm -rf $(KERNEL_DIR)/build
	cd $(KERNEL_DIR) && $(CARGO) clean
