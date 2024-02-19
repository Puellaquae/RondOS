ENTRY(_start)

. = 0xc0022000;

SECTIONS
{

  .text : { *(.text*) }
  .data : { *(.data*) }
  .rodata : { *(.rodata*) }
  .bss : { *(.bss) }
  /DISCARD/ : { *(.eh_frame) *(.comment) *(.debug*) }
}
