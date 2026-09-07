ENTRY(_start)

SECTIONS
{
  . = 0x00022000;                   /* physical load address */

  .text : { *(.text*) }
  . = ALIGN(0x1000);
  .rodata : { *(.rodata*) }
  . = ALIGN(0x1000);
  .data : { *(.data*) }
  . = ALIGN(0x1000);
  .bss : { *(.bss*) }
  . = ALIGN(0x1000);
  _kernel_end = .;
}