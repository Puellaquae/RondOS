; Stage1 — MBR (512 bytes)
; Loaded by BIOS at 0x7c00.
; Reads Stage2 (STAGE2_SECTORS sectors starting at LBA 1) into STAGE2_LOAD_SEG:0,
; verifies its "STG2" magic, then jumps to it in real mode.
; All output via serial (0x3F8).

SERIAL_PORT equ 0x3f8
STAGE2_LOAD_SEG equ 0x0100                                  ; load at 0x1000
                                                            ; (below 0x8000, where
                                                            ; BIOS EDD works)
STAGE2_SECTORS  equ 32                                      ; 16 KiB
STAGE2_ENTRY_OFF equ 4                                      ; skip "STG2" magic
KERNEL_LBA_BASE equ (1 + STAGE2_SECTORS)                    ; 33
; Kernel ELF staging area.  High conventional memory (0x50000) is clear of the
; kernel image (loaded at 0x22000, BSS to ~0x40000), so a large staging buffer
; never overlaps the copy destination.
STAGING_SEG     equ 0x5000                                  ; staging at 0x50000
STAGING_SECTORS equ 256                                     ; 128 KiB / 512
CHUNK_SECTORS   equ 16                                      ; 8 KiB per EDD call

[bits 16]
[org 0x7c00]

stage1_start:
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov fs, ax
    mov gs, ax
    mov esp, 0x6000                  ; stack above Stage2 load region

    call serial_init
    mov si, str_boot
    call serial_puts

    ; enable A20
    in al, 0x92
    or al, 0b0000_0010
    out 0x92, al

    ; read Stage2 via EDD (int 13h AH=42h)
    mov si, dap
    mov ah, 0x42
    mov dl, 0x80
    int 0x13
    jc disk_fail

    ; verify "STG2" magic at the very start of Stage2
    push ds
    mov ax, STAGE2_LOAD_SEG
    mov ds, ax
    xor si, si
    mov eax, [ds:si]
    pop ds
    cmp eax, 0x3247_5453                ; little-endian dword of "STG2"
    jne magic_fail

    ; read kernel ELF (first STAGING_SECTORS sectors) into staging at 0x10000
    xor bx, bx
read_kernel:
    mov ax, bx
    shl ax, 5                           ; bx sectors * 32 paras/sector
    add ax, STAGING_SEG
    mov [kdap + 6], ax
    mov eax, KERNEL_LBA_BASE
    movzx edi, bx
    add eax, edi
    mov [kdap + 8], eax
    mov dword [kdap + 12], 0            ; high dword of LBA
    mov si, kdap
    mov ah, 0x42
    mov dl, 0x80
    int 0x13
    jc disk_fail
    add bx, CHUNK_SECTORS
    cmp bx, STAGING_SECTORS
    jb read_kernel

    jmp STAGE2_LOAD_SEG:STAGE2_ENTRY_OFF

magic_fail:
    mov si, str_magic_fail
    call serial_puts
    jmp hang

disk_fail:
    mov si, str_fail
    call serial_puts
hang:
    cli
    hlt
    jmp hang

; ---------------- serial helpers ----------------

serial_init:
    push dx
    push ax
    mov dx, SERIAL_PORT + 1
    xor al, al
    out dx, al                  ; disable interrupts
    mov dx, SERIAL_PORT + 3
    mov al, 0x80
    out dx, al                  ; enable DLAB
    mov dx, SERIAL_PORT
    mov al, 0x01                ; 115200 / 115200 = 1 (low)
    out dx, al
    mov dx, SERIAL_PORT + 1
    mov al, 0x00                ; high
    out dx, al
    mov dx, SERIAL_PORT + 3
    xor al, al
    out dx, al                  ; disable DLAB, 8-N-1
    mov dx, SERIAL_PORT + 2
    mov al, 0xc7                ; enable FIFO
    out dx, al
    mov dx, SERIAL_PORT + 4
    mov al, 0x0b                ; DTR + RTS
    out dx, al
    pop ax
    pop dx
    ret

serial_putc:
    push dx
    push ax
    mov dx, SERIAL_PORT + 5
.wait:
    in al, dx
    test al, 0x20
    jz .wait
    mov dx, SERIAL_PORT
    pop ax
    out dx, al
    push ax
.wait2:
    mov dx, SERIAL_PORT + 5
    in al, dx
    test al, 0x20
    jz .wait2
    pop ax
    pop dx
    ret

serial_puts:
    cld
.loop:
    lodsb
    test al, al
    jz .done
    call serial_putc
    jmp .loop
.done:
    ret

str_boot: db "Stage1 boot", 0x0d, 0x0a, 0
str_fail: db "Stage1: disk read failed", 0x0d, 0x0a, 0
str_magic_fail: db "Stage1: stage2 magic mismatch", 0x0d, 0x0a, 0

align 2
dap:
    db 0x10, 0
    dw STAGE2_SECTORS
    dw 0
    dw STAGE2_LOAD_SEG
    dq 1

align 2
kdap:
    db 0x10, 0
    dw CHUNK_SECTORS
    dw 0
    dw STAGING_SEG
    dq 0

times 510 - ($ - $$) db 0
dw 0xaa55
