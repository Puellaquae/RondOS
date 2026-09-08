/* kernel64 link script — higher-half kernel (M0).
 *
 * The image is linked linearly: VA = VIRT_BASE + PA, so the physical load
 * address of every section is its virtual address minus VIRT_BASE (the AT()
 * expressions).  QEMU's `-device loader,file=kernel.elf` places each PT_LOAD at
 * its p_paddr; the 32-bit trampoline (boot/multiboot32.s) maps
 * [PHYS_BASE, PHYS_BASE + 16 MiB) at VIRT_BASE with 2 MiB pages and jumps to
 * e_entry in long mode.
 *
 * PHYS_BASE must be 2 MiB aligned — 2 MiB pages can only map 2 MiB aligned
 * physical ranges.
 */

ENTRY(_start)

VIRT_BASE = 0xFFFFFFFF80000000;
PHYS_BASE = 0x200000;

SECTIONS
{
  . = VIRT_BASE + PHYS_BASE;

  .text : AT(ADDR(.text) - VIRT_BASE) { *(.text*) }
  . = ALIGN(0x1000);
  .rodata : AT(ADDR(.rodata) - VIRT_BASE) { *(.rodata*) }
  . = ALIGN(0x1000);
  .data : AT(ADDR(.data) - VIRT_BASE) { *(.data*) }
  . = ALIGN(0x1000);
  .bss : AT(ADDR(.bss) - VIRT_BASE) { *(.bss*) *(COMMON) }
  . = ALIGN(0x1000);

  _kernel_end = .;
  _kernel_end_phys = . - VIRT_BASE;

  /DISCARD/ : { *(.eh_frame) *(.comment) *(.note.gnu.*) }
}
