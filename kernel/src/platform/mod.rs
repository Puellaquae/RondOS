#[no_mangle]
pub unsafe extern "C" fn memcpy(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    assert!(!dest.is_null());
    assert!(!src.is_null());
    for i in 0..n {
        *dest.add(i) = *src.add(i)
    }
    dest
}

#[no_mangle]
pub unsafe extern "C" fn memset(s: *mut u8, c: u8, n: usize) -> *mut u8 {
    assert!(!s.is_null());
    for i in 0..n {
        *s.add(i) = c;
    }
    s
}

#[no_mangle]
pub unsafe extern "C" fn memcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    let mut i = 0;
    while i < n {
        let a = *s1.add(i);
        let b = *s2.add(i);
        if a != b {
            return a as i32 - b as i32;
        }
        i += 1;
    }
    0
}

#[no_mangle]
static _fltused: i32 = 0;

#[naked]
#[no_mangle]
pub unsafe extern "C" fn _chkstk() {
    core::arch::asm!(
        "push   %ecx",
        "cmp    $0x1000,%eax",
        "lea    8(%esp),%ecx", // esp before calling this routine -> ecx
        "jb     1f",
        "2:",
        "sub    $0x1000,%ecx",
        "test   %ecx,(%ecx)",
        "sub    $0x1000,%eax",
        "cmp    $0x1000,%eax",
        "ja     2b",
        "1:",
        "sub    %eax,%ecx",
        "test   %ecx,(%ecx)",
        "lea    4(%esp),%eax",  // load pointer to the return address into eax
        "mov    %ecx,%esp",     // install the new top of stack pointer into esp
        "mov    -4(%eax),%ecx", // restore ecx
        "push   (%eax)",        // push return address onto the stack
        "sub    %esp,%eax",     // restore the original value in eax
        "ret",
        options(noreturn, att_syntax)
    );
}

// from https://github.com/MauriceKayser/rs-windows-builtins/blob/master/no_std/builtins/src/x86.rs

use core::arch::asm;

// More info:
// - https://skanthak.homepage.t-online.de/integer.html
// - https://skanthak.homepage.t-online.de/llvm.html
// - https://skanthak.homepage.t-online.de/nomsvcrt.html

/// Returns the quotient in eax:edx.
/// stdcall, but rust will emit `NAME@16`.
#[naked]
#[no_mangle]
unsafe extern fn _alldiv(dividend: i64, divisor: i64) -> i64 {
    asm!(
        "
        push edi
        push esi
        push ebx
        xor  edi, edi
        mov  eax, dword ptr [esp + 0x14]
        or   eax, eax
        jge  2f
        inc  edi
        mov  edx, dword ptr [esp + 0x10]
        neg  eax
        neg  edx
        sbb  eax, 0
        mov  dword ptr [esp + 0x14], eax
        mov  dword ptr [esp + 0x10], edx
        2:
        mov  eax, dword ptr [esp + 0x1C]
        or   eax, eax
        jge  3f
        inc  edi
        mov  edx, dword ptr [esp + 0x18]
        neg  eax
        neg  edx
        sbb  eax, 0
        mov  dword ptr [esp + 0x1C], eax
        mov  dword ptr [esp + 0x18], edx
        3:
        or   eax, eax
        jnz  4f
        mov  ecx, dword ptr [esp + 0x18]
        mov  eax, dword ptr [esp + 0x14]
        xor  edx, edx
        div  ecx
        mov  ebx, eax
        mov  eax, dword ptr [esp + 0x10]
        div  ecx
        mov  edx, ebx
        jmp  8f
        4:
        mov  ebx, eax
        mov  ecx, dword ptr [esp + 0x18]
        mov  edx, dword ptr [esp + 0x14]
        mov  eax, dword ptr [esp + 0x10]
        5:
        shr  ebx, 1
        rcr  ecx, 1
        shr  edx, 1
        rcr  eax, 1
        or   ebx, ebx
        jnz  5b
        div  ecx
        mov  esi, eax
        mul  dword ptr [esp + 0x1C]
        mov  ecx, eax
        mov  eax, dword ptr [esp + 0x18]
        mul  esi
        add  edx, ecx
        jb   6f
        cmp  edx, dword ptr [esp + 0x14]
        ja   6f
        jb   7f
        cmp  eax, dword ptr [esp + 0x10]
        jbe  7f
        6:
        dec  esi
        7:
        xor  edx, edx
        mov  eax, esi
        8:
        dec  edi
        jnz  9f
        neg  edx
        neg  eax
        sbb  edx, 0
        9:
        pop  ebx
        pop  esi
        pop  edi
        ret  2*8
        ",

        options(noreturn)
    );
}

/// Returns the quotient in edx:eax, and the remainder in ebx:ecx.
/// stdcall, but rust will emit `NAME@16`.
#[allow(improper_ctypes_definitions)]
#[naked]
#[no_mangle]
unsafe extern fn _alldvrm(dividend: i64, divisor: i64) -> (i64, i64) {
    asm!(
        "
        push edi
        push esi
        push ebp
        xor  edi, edi
        xor  ebp, ebp
        mov  eax, dword ptr [esp + 0x14]
        or   eax, eax
        jge  2f
        inc  edi
        inc  ebp
        mov  edx, dword ptr [esp + 0x10]
        neg  eax
        neg  edx
        sbb  eax, 0
        mov  dword ptr [esp + 0x14], eax
        mov  dword ptr [esp + 0x10], edx
        2:
        mov  eax, dword ptr [esp + 0x1C]
        or   eax, eax
        jge  3f
        inc  edi
        mov  edx, dword ptr [esp + 0x18]
        neg  eax
        neg  edx
        sbb  eax, 0
        mov  dword ptr [esp + 0x1C], eax
        mov  dword ptr [esp + 0x18], edx
        3:
        or   eax, eax
        jnz  4f
        mov  ecx, dword ptr [esp + 0x18]
        mov  eax, dword ptr [esp + 0x14]
        xor  edx, edx
        div  ecx
        mov  ebx, eax
        mov  eax, dword ptr [esp + 0x10]
        div  ecx
        mov  esi, eax
        mov  eax, ebx
        mul  dword ptr [esp + 0x18]
        mov  ecx, eax
        mov  eax, esi
        mul  dword ptr [esp + 0x18]
        add  edx, ecx
        jmp  8f
        4:
        mov  ebx, eax
        mov  ecx, dword ptr [esp + 0x18]
        mov  edx, dword ptr [esp + 0x14]
        mov  eax, dword ptr [esp + 0x10]
        5:
        shr  ebx, 1
        rcr  ecx, 1
        shr  edx, 1
        rcr  eax, 1
        or   ebx, ebx
        jnz  5b
        div  ecx
        mov  esi, eax
        mul  dword ptr [esp + 0x1C]
        mov  ecx, eax
        mov  eax, dword ptr [esp + 0x18]
        mul  esi
        add  edx, ecx
        jb   6f
        cmp  edx, dword ptr [esp + 0x14]
        ja   6f
        jb   7f
        cmp  eax, dword ptr [esp + 0x10]
        jbe  7f
        6:
        dec  esi
        sub  eax, dword ptr [esp + 0x18]
        sbb  edx, dword ptr [esp + 0x1C]
        7:
        xor  ebx, ebx
        8:
        sub  eax, dword ptr [esp + 0x10]
        sbb  edx, dword ptr [esp + 0x14]
        dec  ebp
        jns  9f
        neg  edx
        neg  eax
        sbb  edx, 0
        9:
        mov  ecx, edx
        mov  edx, ebx
        mov  ebx, ecx
        mov  ecx, eax
        mov  eax, esi
        dec  edi
        jnz  12f
        neg  edx
        neg  eax
        sbb  edx, 0
        12:
        pop  ebp
        pop  esi
        pop  edi
        ret  2*8
        ",

        options(noreturn)
    );
}

/// Returns the product in edx:eax.
/// stdcall, but rust will emit `NAME@16`.
#[naked]
#[no_mangle]
unsafe extern fn _allmul(multiplier: i64, multiplicand: i64) -> i64 {
    asm!(
        "
        mov  eax, dword ptr [esp + 0x8]
        mov  ecx, dword ptr [esp + 0x10]
        or   ecx, eax
        mov  ecx, dword ptr [esp + 0xC]
        jnz  2f
        mov  eax, dword ptr [esp + 0x4]
        mul  ecx
        ret  2*8
        2:
        push ebx
        mul  ecx
        mov  ebx, eax
        mov  eax, dword ptr [esp + 0x8]
        mul  dword ptr [esp + 0x14]
        add  ebx, eax
        mov  eax, dword ptr [esp + 0x8]
        mul  ecx
        add  edx, ebx
        pop  ebx
        ret  2*8
        ",

        options(noreturn)
    );
}

/// Returns the remainder in edx:eax.
/// stdcall, but rust will emit `NAME@16`.
#[naked]
#[no_mangle]
unsafe extern fn _allrem(dividend: i64, divisor: i64) -> i64 {
    asm!(
        "
        push ebx
        push edi
        xor  edi, edi
        mov  eax, dword ptr [esp + 0x10]
        or   eax, eax
        jge  2f
        inc  edi
        mov  edx, dword ptr [esp + 0xC]
        neg  eax
        neg  edx
        sbb  eax, 0
        mov  dword ptr [esp + 0x10], eax
        mov  dword ptr [esp + 0xC], edx
        2:
        mov  eax, dword ptr [esp + 0x18]
        or   eax, eax
        jge  3f
        mov  edx, dword ptr [esp + 0x14]
        neg  eax
        neg  edx
        sbb  eax, 0
        mov  dword ptr [esp + 0x18], eax
        mov  dword ptr [esp + 0x14], edx
        3:
        or   eax, eax
        jnz  4f
        mov  ecx, dword ptr [esp + 0x14]
        mov  eax, dword ptr [esp + 0x10]
        xor  edx, edx
        div  ecx
        mov  eax, dword ptr [esp + 0xC]
        div  ecx
        mov  eax, edx
        xor  edx, edx
        dec  edi
        jns  8f
        jmp  9f
        4:
        mov  ebx, eax
        mov  ecx, dword ptr [esp + 0x14]
        mov  edx, dword ptr [esp + 0x10]
        mov  eax, dword ptr [esp + 0xC]
        5:
        shr  ebx, 1
        rcr  ecx, 1
        shr  edx, 1
        rcr  eax, 1
        or   ebx, ebx
        jnz  5b
        div  ecx
        mov  ecx, eax
        mul  dword ptr [esp + 0x18]
        xchg eax, ecx
        mul  dword ptr [esp + 0x14]
        add  edx, ecx
        jb   6f
        cmp  edx, dword ptr [esp + 0x10]
        ja   6f
        jb   7f
        cmp  eax, dword ptr [esp + 0xC]
        jbe  7f
        6:
        sub  eax, dword ptr [esp + 0x14]
        sbb  edx, dword ptr [esp + 0x18]
        7:
        sub  eax, dword ptr [esp + 0xC]
        sbb  edx, dword ptr [esp + 0x10]
        dec  edi
        jns  9f
        8:
        neg  edx
        neg  eax
        sbb  edx, 0
        9:
        pop  edi
        pop  ebx
        ret  2*8
        ",

        options(noreturn)
    );
}

/// Receives `value` in edx:eax and `positions` in cl.
/// Returns the shifted value in edx:eax.
/// borland fastcall, not available in rust.
#[naked]
#[no_mangle]
unsafe extern fn _allshl(value: i64, positions: i64) -> i64 {
    asm!(
        "
        cmp  cl,  64
        jnb  3f
        cmp  cl,  32
        jnb  2f
        shld edx, eax, cl
        shl  eax, cl
        ret
        2:
        mov  edx, eax
        xor  eax, eax
        and  cl,  32-1
        shl  edx, cl
        ret
        3:
        xor  eax, eax
        xor  edx, edx
        ret
        ",

        options(noreturn)
    );
}

/// Receives `value` in edx:eax and `positions` in cl.
/// Returns the shifted value in edx:eax.
/// borland fastcall, not available in rust.
#[naked]
#[no_mangle]
unsafe extern fn _allshr(value: i64, positions: i64) -> i64 {
    asm!(
        "
        cmp  cl,  64
        jnb  3f
        cmp  cl,  32
        jnb  2f
        shrd eax, edx, cl
        sar  edx, cl
        ret
        2:
        mov  eax, edx
        sar  edx, 32-1
        and  cl,  32-1
        sar  eax, cl
        ret
        3:
        sar  edx, 0x1F
        mov  eax, edx
        ret
        ",

        options(noreturn)
    );
}

/// Returns the quotient in edx:eax.
/// stdcall, but rust will emit `NAME@16`.
#[naked]
#[no_mangle]
unsafe extern fn _aulldiv(dividend: u64, divisor: u64) -> u64 {
    asm!(
        "
        push ebx
        push esi
        mov  eax, dword ptr [esp + 0x18]
        or   eax, eax
        jnz  2f
        mov  ecx, dword ptr [esp + 0x14]
        mov  eax, dword ptr [esp + 0x10]
        xor  edx, edx
        div  ecx
        mov  ebx, eax
        mov  eax, dword ptr [esp + 0xC]
        div  ecx
        mov  edx, ebx
        jmp  6f
        2:
        mov  ecx, eax
        mov  ebx, dword ptr [esp + 0x14]
        mov  edx, dword ptr [esp + 0x10]
        mov  eax, dword ptr [esp + 0xC]
        3:
        shr  ecx, 1
        rcr  ebx, 1
        shr  edx, 1
        rcr  eax, 1
        or   ecx, ecx
        jnz  3b
        div  ebx
        mov  esi, eax
        mul  dword ptr [esp + 0x18]
        mov  ecx, eax
        mov  eax, dword ptr [esp + 0x14]
        mul  esi
        add  edx, ecx
        jb   4f
        cmp  edx, dword ptr [esp + 0x10]
        ja   4f
        jb   5f
        cmp  eax, dword ptr [esp + 0xC]
        jbe  5f
        4:
        dec  esi
        5:
        xor  edx, edx
        mov  eax, esi
        6:
        pop  esi
        pop  ebx
        ret  2*8
        ",

        options(noreturn)
    );
}

/// Returns the quotient in edx:eax, and the remainder in ebx:ecx.
/// stdcall, but rust will emit `NAME@16`.
#[naked]
#[no_mangle]
unsafe extern fn _aulldvrm(dividend: u64, divisor: u64) -> u64 {
    asm!(
        "
        push esi
        mov  eax, dword ptr [esp + 0x14]
        or   eax, eax
        jnz  2f
        mov  ecx, dword ptr [esp + 0x10]
        mov  eax, dword ptr [esp + 0xC]
        xor  edx, edx
        div  ecx
        mov  ebx, eax
        mov  eax, dword ptr [esp + 0x8]
        div  ecx
        mov  esi, eax
        mov  eax, ebx
        mul  dword ptr [esp + 0x10]
        mov  ecx, eax
        mov  eax, esi
        mul  dword ptr [esp + 0x10]
        add  edx, ecx
        jmp  6f
        2:
        mov  ecx, eax
        mov  ebx, dword ptr [esp + 0x10]
        mov  edx, dword ptr [esp + 0xC]
        mov  eax, dword ptr [esp + 0x8]
        3:
        shr  ecx, 1
        rcr  ebx, 1
        shr  edx, 1
        rcr  eax, 1
        or   ecx, ecx
        jnz  3b
        div  ebx
        mov  esi, eax
        mul  dword ptr [esp + 0x14]
        mov  ecx, eax
        mov  eax, dword ptr [esp + 0x10]
        mul  esi
        add  edx, ecx
        jb   4f
        cmp  edx, dword ptr [esp + 0xC]
        ja   4f
        jb   5f
        cmp  eax, dword ptr [esp + 0x8]
        jbe  5f
        4:
        dec  esi
        sub  eax, dword ptr [esp + 0x10]
        sbb  edx, dword ptr [esp + 0x14]
        5:
        xor  ebx, ebx
        6:
        sub  eax, dword ptr [esp + 0x8]
        sbb  edx, dword ptr [esp + 0xC]
        neg  edx
        neg  eax
        sbb  edx, 0
        mov  ecx, edx
        mov  edx, ebx
        mov  ebx, ecx
        mov  ecx, eax
        mov  eax, esi
        pop  esi
        ret  2*8
        ",

        options(noreturn)
    );
}

/// Returns the remainder in edx:eax.
/// stdcall, but rust will emit `NAME@16`.
#[naked]
#[no_mangle]
unsafe extern fn _aullrem(dividend: u64, divisor: u64) -> u64 {
    asm!(
        "
        push ebx
        mov  eax, dword ptr [esp + 0x14]
        or   eax, eax
        jnz  2f
        mov  ecx, dword ptr [esp + 0x10]
        mov  eax, dword ptr [esp + 0xC]
        xor  edx, edx
        div  ecx
        mov  eax, dword ptr [esp + 0x8]
        div  ecx
        mov  eax, edx
        xor  edx, edx
        jmp  6f
        2:
        mov  ecx, eax
        mov  ebx, dword ptr [esp + 0x10]
        mov  edx, dword ptr [esp + 0xC]
        mov  eax, dword ptr [esp + 0x8]
        3:
        shr  ecx, 1
        rcr  ebx, 1
        shr  edx, 1
        rcr  eax, 1
        or   ecx, ecx
        jnz  3b
        div  ebx
        mov  ecx, eax
        mul  dword ptr [esp + 0x14]
        xchg eax, ecx
        mul  dword ptr [esp + 0x10]
        add  edx, ecx
        jb   4f
        cmp  edx, dword ptr [esp + 0xC]
        ja   4f
        jb   5f
        cmp  eax, dword ptr [esp + 0x8]
        jbe  5f
        4:
        sub  eax, dword ptr [esp + 0x10]
        sbb  edx, dword ptr [esp + 0x14]
        5:
        sub  eax, dword ptr [esp + 0x8]
        sbb  edx, dword ptr [esp + 0xC]
        neg  edx
        neg  eax
        sbb  edx, 0
        6:
        pop  ebx
        ret  2*8
        ",

        options(noreturn)
    );
}

/// Returns the shifted value in edx:eax.
/// borland fastcall, not available in rust.
#[naked]
#[no_mangle]
unsafe extern fn _aullshr(value: u64, positions: u64) -> u64 {
    asm!(
        "
        cmp  cl,  64
        jnb  3f
        cmp  cl,  32
        jnb  2f
        shrd eax, edx, cl
        shr  edx, cl
        ret
        2:
        mov  eax, edx
        xor  edx, edx
        and  cl,  32-1
        shr  eax, cl
        ret
        3:
        xor  eax, eax
        xor  edx, edx
        ret
        ",

        options(noreturn)
    );
}
