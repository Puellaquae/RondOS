; multiboot32.s — temporary 32-bit trampoline for the M0 x86-64 kernel.
;
; QEMU's multiboot loader wants a 32-bit image, and our kernel is a 64-bit
; higher-half ELF, so `qemu -kernel kernel64` cannot work.  Instead:
;
;   qemu -kernel trampoline.bin -device loader,file=kernel64
;
; `-kernel` boots this flat binary (multiboot "a.out kludge": the whole file is
; loaded at LOAD_ADDR and we are entered in 32-bit protected mode with
; eax = 0x2BADB002, ebx = multiboot info).  `-device loader` places the kernel's
; ELF64 segments at their p_paddr.  We then build the long-mode page tables,
; switch to 64-bit mode and jump to the kernel entry, whose address the Makefile
; passes in as -DENTRY_HI=<e_entry>.
;
; Deleted at M0.7 when the UEFI stub can hand `BootInfo` to the kernel.
;
; Scratch memory it owns (all below 1 MiB, all free after the jump):
;   0x10000  PML4
;   0x11000  PDPT  identity   (PML4[0])
;   0x12000  PDPT  physmap    (PML4[256])
;   0x13000  PDPT  kernel     (PML4[511])
;   0x14000  PD    kernel     (8 x 2 MiB)
;   0x9F000  temporary 32-bit stack

%define LOAD_ADDR 0x100000
%define PHYS_BASE 0x200000

org LOAD_ADDR
bits 32

; ---- multiboot 1 header (a.out kludge: explicit load/entry addresses) ----
mb_header:
    dd 0x1BADB002
    dd 0x00010003                 ; HAS_ADDR | MEM_INFO | MODS_ALIGN
    dd -(0x1BADB002 + 0x00010003)
    dd mb_header                  ; header_addr
    dd LOAD_ADDR                  ; load_addr
    dd 0                          ; load_end_addr (0 = whole file)
    dd 0                          ; bss_end_addr
    dd _start                     ; entry_addr

global _start
_start:
    cli
    mov esi, ebx                  ; multiboot info (physical)
    mov esp, 0x9F000

    mov ax, 0x10                  ; flat segments from the loader's GDT
    mov ds, ax
    mov es, ax
    mov ss, ax

    ; zero the page-table pages 0x10000..0x16000
    mov edi, 0x10000
    xor eax, eax
    mov ecx, 0x6000 / 4
    rep stosd

    ; PML4
    mov dword [0x10000], 0x11000 | 3
    mov dword [0x10800], 0x12000 | 3
    mov dword [0x10FF8], 0x13000 | 3

    ; identity: one 1 GiB page
    mov dword [0x11000], 0x83

    ; physmap: 4 x 1 GiB pages, NX
    xor ecx, ecx
.phys:
    mov eax, ecx
    shl eax, 30
    or eax, 0x83
    mov [0x12000 + ecx * 8], eax
    mov dword [0x12004 + ecx * 8], 0x80000000
    inc ecx
    cmp ecx, 4
    jne .phys

    ; kernel window: 8 x 2 MiB pages, VA = VIRT_BASE + PA (identity in the
    ; linear window), so PD[i] maps VIRT_BASE + i*2MiB -> phys i*2MiB.
    mov dword [0x13FF0], 0x14000 | 3
    xor ecx, ecx
.kern:
    mov eax, ecx
    shl eax, 21
    or eax, 0x83
    mov [0x14000 + ecx * 8], eax
    mov dword [0x14004 + ecx * 8], 0
    inc ecx
    cmp ecx, 8
    jne .kern

    ; CR4.PAE | CR4.PGE
    mov eax, cr4
    or eax, (1 << 5) | (1 << 7)
    mov cr4, eax

    ; EFER.LME | EFER.NXE
    mov ecx, 0xC0000080
    rdmsr
    or eax, (1 << 8) | (1 << 11)
    wrmsr

    mov eax, 0x10000
    mov cr3, eax

    ; CR0.WP | CR0.PG
    mov eax, cr0
    or eax, (1 << 31) | (1 << 16)
    mov cr0, eax

    lgdt [gdt_desc]
    jmp 0x08:long_entry

bits 64
long_entry:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov rsp, 0x9EFF8              ; SysV: rsp % 16 == 8 at function entry
    mov edi, esi                  ; rdi = multiboot info (zero-extended)
    mov rax, ENTRY_HI
    jmp rax

align 8
gdt:
    dq 0
    dq 0x00AF9A000000FFFF         ; 64-bit code, DPL0
    dq 0x00CF92000000FFFF         ; data, DPL0
gdt_end:
gdt_desc:
    dw gdt_end - gdt - 1
    dd gdt
