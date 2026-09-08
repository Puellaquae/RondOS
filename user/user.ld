/* RondOS user program link script — fixed-address (non-PIE) ELF64.
 *
 * Every PT_LOAD is page aligned and p_vaddr == p_paddr, so the kernel loader
 * can map each segment directly.  .text is R+X, .data/.bss are RW + NX (the
 * kernel sets NX from the segment flags).  .note.rondos would ride along in a
 * PT_NOTE for the manifest (P2); the PHDRS skeleton is already here.
 */

ENTRY(_start)

PHDRS
{
  text PT_LOAD FLAGS(5);   /* R + X */
  data PT_LOAD FLAGS(6);   /* RW + NX */
}

SECTIONS
{
  . = 0x400000;

  .text : {
    *(.text._start)
    *(.text .text.*)
  } :text

  .rodata : ALIGN(0x1000) {
    *(.rodata .rodata.*)
    *(.note.rondos)
  } :text

  .data : ALIGN(0x1000) {
    *(.data .data.*)
  } :data

  .bss : ALIGN(0x1000) {
    *(.bss .bss.*)
    *(COMMON)
  } :data

  /DISCARD/ : {
    *(.eh_frame)
    *(.comment)
    *(.note.gnu.*)
  }
}
