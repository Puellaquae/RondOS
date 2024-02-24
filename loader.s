CR0_PE equ 0x00000001      ; Protection Enable.
CR0_EM equ 0x00000004      ; (Floating-point) Emulation.
CR0_PG equ 0x80000000      ; Paging.
CR0_WP equ 0x00010000      ; Write-Protect enable in kernel mode.

PG_TABLE equ 0x00000001
PG_RW    equ 0x00000002
PG_US    equ 0x00000004
PG_PAGE  equ 0x00000001

LOADER_LOAD_ADDR equ 0x20000
LOADER_DISK_BLOCK_COUNT equ (LOADER_END + 255) / 256

PAGING_TABLE_ADDR equ 0xf000
PAGING_PAGE_ADDR equ 0x10000

KERNEL_BASE equ 0xc0000000

[BITS 16]

    mov ax, 0x07c0
    mov ds, ax
    mov es, ax
    xor ax, ax
    mov ss, ax
    mov fs, ax
    mov gs, ax
    mov sp, 0x7c00
    lea si, str_boot_start
    call puts

    jmp bios_read

; hda loader can't work in qemu
; hda_loader:
;     mov dx, 0x1f6
;     mov al, 0b11100000
;     out dx, al
; 
;     mov al, LOADER_DISK_BLOCK_COUNT
;     mov dx, 0x1f2
;     out dx, al
; 
;     xor al, al
;     mov dx, 0x1f3
;     out dx, al
;     
;     mov dx, 0x1f4
;     out dx, al
;     
;     mov dx, 0x1f5
;     out dx, al
;     
;     mov dx, 0x1f7
;     mov al, 0x20
;     out dx, al
; 
; waits:
;     in al, dx
;     test al, 0x08
;     jz waits
; 
; read_data:
;     mov dx, 0x1f0
;     mov cx, LOADER_DISK_BLOCK_COUNT * 256
;     mov ax, (LOADER_LOAD_ADDR >> 4)
;     mov es, ax
;     mov di, 0
;     rep insw

DAPACK:
    db 0x10
    db 0
blkcnt: dw LOADER_DISK_BLOCK_COUNT      ; int 13 resets this to # of blocks actually read/written
db_add: dw 0x0000  ; memory buffer destination address (0:7c00)
        dw 0x2000  ; in memory page zero
d_lba: dd 0 ; put the lba to read in this spot
       dd 0 ; more storage bytes only for big lba's ( > 4 bytes )

bios_read:
	mov si, DAPACK		; address of "disk address packet"
	mov ah, 0x42		; AL is unused
	mov dl, 0x80		; drive number 0 (OR the drive # with 0x80)
	int 0x13
	jc check_fail

check_read:
    mov ax, 0x2000
    mov es, ax
    mov si, 0x1000
    mov eax, [es: si]
    and eax, 0xffffff00
    cmp eax, 0x464c4500
    je jmp_rest
check_fail:
    lea si, str_load_fail
    call puts
fail_loop:
    jmp fail_loop

jmp_rest:
    jmp (LOADER_LOAD_ADDR >> 4):next_part

puts:
    cld
puts_loop:
    lodsb
    test al, al
    jz puts_fin
    call putc
    jmp puts_loop
puts_fin:
    ret

putc:
    mov ah, 0x0e
    int 0x10
    ret 

next_part:
    mov ax, LOADER_LOAD_ADDR >> 4
    mov es, ax
    mov ds, ax
    mov fs, ax
    mov gs, ax
    lea si, str_jump_in
    call puts

a20gate:
    in al, 0x92
    or al, 0000_0010B
    out 0x92, al

    xor ebx, ebx
    mov dword [memlayoutlen], 0
    mov di, memlayoutbuf
detectmem:
    mov eax, 0xe820
    mov ecx, 20
    mov edx, 0x0534D4150
    int 0x15
    jc detectmemfail
    inc dword [memlayoutlen]
    add di, 20
    test ebx, ebx
    jz detectmemfin
    jmp detectmem
detectmemfail:
    mov dword [memlayoutlen], 0xdead
detectmemfin:

; from 0xf000(60KB) to 0x10000(64KB) store the page table, total 1024 entries
; in low 64 MB direct to physical addr, in high 3GB, base 3GB map to base 0
pde_mem_clear:
    mov ax, PAGING_TABLE_ADDR >> 4
    mov es, ax
    xor eax, eax
    xor di, di
    mov cx, 0x400  ; 1024 * 4B = 4KB
    rep stosd

build_pde:
    mov eax, PAGING_PAGE_ADDR | PG_TABLE | PG_RW | PG_US
    mov cx, 0x10
    xor di, di
write_pde:
    mov [es:di], eax
    mov [es:di + (KERNEL_BASE >> 20)], eax
    add di, 4
    add eax, 0x1000
    loop write_pde

; from 0x10000(64KB) to 0x20000(128KB), total 1024 * 16 entries
build_pte:
    mov ax, PAGING_PAGE_ADDR >> 4
    mov es, ax
    mov eax, PG_PAGE | PG_RW | PG_US
    mov cx, 0x4000 ; 0x4000 * 4B = 64KB
    xor di, di
write_pte:
    mov [es:di], eax  
    add di, 4
    add eax, 0x1000
    loop write_pte

set_pde:
    mov eax, PAGING_TABLE_ADDR
    mov cr3, eax

    cli

    lgdt [gdtdesc]

    mov eax, cr0
    or eax, CR0_PE | CR0_PG | CR0_WP
    mov cr0, eax
    jmp dword SELECTOR_CODE_SEG: KERNEL_BASE + LOADER_LOAD_ADDR + protect_entry

[BITS 32]

protect_entry:
    mov ax, SELECTOR_DATA_SEG
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    add esp, KERNEL_BASE

    mov dword [LOADER_LOAD_ADDR + kernel_size], LOADER_END

enable_sse:
    mov eax, cr0
    and ax, 0xFFFB
    or ax, 0x2
    mov cr0, eax
    mov eax, cr4
    or ax, 3 << 9
    mov cr4, eax
    mov eax, [0x21018]
    sub eax, KERNEL_BASE
    jmp eax
    hlt

str_jump_in:  dd "Jump In", 0x0d, 0x0a, 0
str_boot_start: db "Booting", 0x0d, 0x0a, 0
str_load_loader: db "Load Disk", 0x0d, 0x0a, 0
str_load_fail: db "Load Fail", 0x0d, 0x0a, 0

align 8

gdt:
gdt_null:
	dq 0x0000000000000000	; Null segment.  Not used by CPU.
SELECTOR_NULL equ gdt_null - gdt
gdt_kcseg:
	dq 0x00cf9a000000ffff	; System code, base 0, limit 4 GB.
SELECTOR_CODE_SEG equ gdt_kcseg - gdt
gdt_kdseg:
	dq 0x00cf92000000ffff   ; System data, base 0, limit 4 GB.
SELECTOR_DATA_SEG equ gdt_kdseg - gdt
gdtdesc:
	dw	gdtdesc - gdt - 1	; Size of the GDT, minus 1 byte.
	dd	gdt	+ LOADER_LOAD_ADDR  ; Address of the GDT.

times 510-($-$$) db 0
dw 0xaa55

kernel_size: dw 0, 0
memlayoutlen: dw 0, 0
memlayoutbuf: dw 0, 0

times 0x1000-($-$$) db 0

incbin "kernel.bin"

LOADER_END equ $ - $$
