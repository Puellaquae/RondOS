; Stage2 — loaded by Stage1 at 0x0100:0000 (= 0x1000), 16 KiB max.
; Stage1 already read the first STAGING bytes of the kernel ELF into
; STAGING_ADDR. Stage2 only parses that buffer, copies PT_LOAD segments to
; their p_paddr, enables paging and jumps into the kernel. No BIOS disk
; calls happen here.

%define KERNEL_BASE     0xc0000000
%define STAGING_ADDR    0x00010000                 ; kernel ELF staged here
%define PDE_TABLE_ADDR  0x0000f000                 ; 4 KiB page-directory

CR0_PE equ 0x00000001
CR0_PG equ 0x80000000
CR0_WP equ 0x00010000

[bits 16]
[org 0x1000]

    ; "STG2" magic at offset 0 so Stage1 can verify the load.
    db "STG2"

stage2_start:
    cli
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax
    mov esp, 0x6000                  ; stack above Stage2 load region
    sti

    mov si, str_jump_in
    call puts

; --- 1. e820 detect ---
    ; entries at physical 0x9304 (u32 count at 0x9300), mirrored at
    ; 0xc0009300 by the identity+KERNEL_BASE mapping.
    xor ax, ax
    mov es, ax
    mov di, 0x9304
    xor ebx, ebx
    xor bp, bp
.detect_loop:
    mov eax, 0xe820
    mov ecx, 20
    mov edx, 0x0534D4150
    int 0x15
    jc .detect_done
    inc bp
    add di, 20
    cmp bp, 4
    jae .detect_done
    test ebx, ebx
    jnz .detect_loop
.detect_done:
    mov dword [0x9300], ebp         ; memlayout count (u32) at 0x9300

; --- 2. validate ELF header in staging ---
    mov eax, STAGING_ADDR
    cmp dword [eax], 0x464c457f
    jne elf_fail
    cmp byte [eax + 4], 1
    jne elf_fail
    cmp word [eax + 18], 3
    jne elf_fail

; --- 3. walk program headers, memcpy PT_LOAD to p_paddr ---
    movzx ecx, word [eax + 44]                       ; e_phnum
    test ecx, ecx
    jz elf_fail
    movzx ebx, word [eax + 42]                       ; e_phentsize
    movzx edx, word [eax + 28]                       ; e_phoff
    add edx, eax                                     ; edx = first phdr

.phdr_loop:
    push edx
    push ecx

    mov eax, [edx]                                   ; p_type
    cmp eax, 1                                       ; PT_LOAD?
    jne .phdr_next
    mov eax, [edx + 16]                              ; p_filesz
    test eax, eax
    jz .phdr_next

    mov esi, [edx + 4]                               ; p_offset
    add esi, STAGING_ADDR
    mov [memcpy_src], esi
    mov esi, [edx + 12]                              ; p_paddr
    mov [memcpy_dst], esi
    mov [memcpy_len], eax
    call memcpy

.phdr_next:
    pop ecx
    pop edx
    add edx, ebx
    dec ecx
    jnz .phdr_loop

; --- 4. build PDEs (4 MiB pages, identity + KERNEL_BASE mirror) ---
    call build_pde_4m

; --- 5. protected mode jump ---
    cli
    lgdt [gdtdesc]

    mov eax, cr0
    or eax, CR0_PE | CR0_PG | CR0_WP
    mov cr0, eax

    jmp dword SELECTOR_CODE_SEG:protect_entry

[BITS 32]
protect_entry:
    mov ax, SELECTOR_DATA_SEG
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    mov eax, [STAGING_ADDR + 24]                ; e_entry
    add eax, KERNEL_BASE
    jmp eax
[BITS 16]

elf_fail:
    mov si, str_elf_fail
    call puts
.hang:
    hlt
    jmp .hang

; ---------------- helpers ----------------

puts:
    cld
.loop:
    lodsb
    test al, al
    jz .done
    mov ah, 0x0e
    int 0x10
    jmp .loop
.done:
    ret

; memcpy(dst, src, len): 32-bit moves, handles 64 KiB segment crossing.
memcpy:
    push bx
    push cx
    push si
    push di
    push ds
    push es

    mov bx, ds
    mov [ss:memcpy_save_ds], bx
    mov bx, es
    mov [ss:memcpy_save_es], bx

.memcpy_loop:
    mov ecx, [ss:memcpy_len]
    test ecx, ecx
    jz .memcpy_done

    ; src window: ds = (src & ~0xffff) >> 4, si = src & 0xffff
    mov eax, [ss:memcpy_src]
    mov si, ax
    and eax, 0xffff0000
    shr eax, 4
    mov ds, ax

    ; dst window: es = (dst & ~0xffff) >> 4, di = dst & 0xffff
    mov eax, [ss:memcpy_dst]
    mov di, ax
    and eax, 0xffff0000
    shr eax, 4
    mov es, ax

    ; chunk = min(len, 0x10000-(src&0xffff), 0x10000-(dst&0xffff))
    mov eax, [ss:memcpy_src]
    and eax, 0xffff
    neg eax
    add eax, 0x10000
    mov edx, eax
    mov eax, [ss:memcpy_dst]
    and eax, 0xffff
    neg eax
    add eax, 0x10000
    cmp edx, eax
    jbe .src_ok
    mov edx, eax
.src_ok:
    cmp edx, ecx
    jbe .len_ok
    mov edx, ecx
.len_ok:

    push edx
    mov ecx, edx
    shr ecx, 2
    jz .byte_tail
    cld
    rep movsd
.byte_tail:
    mov ecx, edx
    and ecx, 3
    jz .advance
    rep movsb
.advance:
    pop edx
    add [ss:memcpy_src], edx
    add [ss:memcpy_dst], edx
    sub [ss:memcpy_len], edx
    jmp .memcpy_loop

.memcpy_done:
    mov bx, [ss:memcpy_save_ds]
    mov ds, bx
    mov bx, [ss:memcpy_save_es]
    mov es, bx

    pop es
    pop ds
    pop di
    pop si
    pop cx
    pop bx
    ret

build_pde_4m:
    push es
    push di
    xor eax, eax
    mov es, ax
    mov di, PDE_TABLE_ADDR
    mov cx, 1024
.clear:
    mov [es:di], eax
    add di, 4
    loop .clear

    ; write PDE #0..15: identity map low 64 MiB (16 × 4 MiB)
    mov di, PDE_TABLE_ADDR
    mov eax, 0x00000087
    mov cx, 16
.write:
    mov [es:di], eax
    add eax, 0x00400000
    add di, 4
    loop .write

    ; write PDE #768..783: mirror low 64 MiB at KERNEL_BASE
    mov di, PDE_TABLE_ADDR + (768 * 4)
    mov eax, 0x00000087
    mov cx, 16
.write_mirror:
    mov [es:di], eax
    add eax, 0x00400000
    add di, 4
    loop .write_mirror

    mov eax, cr4
    or eax, 0x10                   ; enable PSE (4 MiB pages)
    mov cr4, eax

    mov eax, PDE_TABLE_ADDR
    mov cr3, eax

    pop di
    pop es
    ret

; ---------------- data ----------------

str_jump_in:  db "Stage2", 0x0d, 0x0a, 0
str_elf_fail: db "ELF Fail", 0x0d, 0x0a, 0

memcpy_dst: dd 0
memcpy_src: dd 0
memcpy_len: dd 0
memcpy_save_ds: dw 0
memcpy_save_es: dw 0

align 16
gdt:
gdt_null:   dq 0
SELECTOR_NULL     equ gdt_null - gdt
gdt_kcseg:  dq 0x00cf9a000000ffff
SELECTOR_CODE_SEG equ gdt_kcseg - gdt
gdt_kdseg:  dq 0x00cf92000000ffff
SELECTOR_DATA_SEG equ gdt_kdseg - gdt
gdtdesc:
    dw gdtdesc - gdt - 1
    dd gdt
