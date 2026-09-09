/* RondOS C runtime entry (P2b).
 *
 * The kernel enters user code with `rdi` = &StartupBlock and `rsp` pointing at
 * a valid user stack.  SysV wants `rsp % 16 == 8` *inside* a callee, i.e.
 * `rsp % 16 == 0` at the `call`, so align first.
 */

    .section .text._start, "ax", @progbits
    .global _start
    .type _start, @function
_start:
    xor  %rbp, %rbp
    and  $-16, %rsp
    mov  %rdi, %rdi            /* StartupBlock* is main's first argument */
    call main
    mov  %eax, %edi            /* sys_exit(main's return value) */
    mov  $0x10, %eax           /* RONDOS_SYS_EXIT */
    int  $0x80
    ud2                        /* a conforming kernel never returns */
    .size _start, . - _start

    .section .note.GNU-stack, "", @progbits
