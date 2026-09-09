//! x86-64 architecture layer — M0.1.
//!
//! Only what the migration scaffold needs: port I/O, control registers, MSRs,
//! CPUID, descriptor-table loading, TLB invalidation and the usual interrupt
//! flag helpers.  Everything here is the x86-64 replacement for
//! `kernel/src/arch/x86/mod.rs`.

#![allow(dead_code)]

use core::arch::asm;

pub mod gdt;
pub mod intr;
pub mod paging;
pub mod pic;
pub mod percpu;
pub mod tss;

// ---------------------------------------------------------------- port I/O

#[inline]
pub fn inb(port: u16) -> u8 {
    let data: u8;
    unsafe {
        asm!("in al, dx", in("dx") port, out("al") data, options(nomem, nostack, preserves_flags));
    }
    data
}

#[inline]
pub fn outb(port: u16, data: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") data, options(nomem, nostack, preserves_flags));
    }
}

#[inline]
pub fn inw(port: u16) -> u16 {
    let data: u16;
    unsafe {
        asm!("in ax, dx", in("dx") port, out("ax") data, options(nomem, nostack, preserves_flags));
    }
    data
}

#[inline]
pub fn outw(port: u16, data: u16) {
    unsafe {
        asm!("out dx, ax", in("dx") port, in("ax") data, options(nomem, nostack, preserves_flags));
    }
}

#[inline]
pub fn inl(port: u16) -> u32 {
    let data: u32;
    unsafe {
        asm!("in eax, dx", in("dx") port, out("eax") data, options(nomem, nostack, preserves_flags));
    }
    data
}

#[inline]
pub fn outl(port: u16, data: u32) {
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") data, options(nomem, nostack, preserves_flags));
    }
}

// ------------------------------------------------------ control registers

macro_rules! cr_read {
    ($name:ident, $cr:literal) => {
        #[inline]
        pub fn $name() -> u64 {
            let v: u64;
            unsafe { asm!(concat!("mov {}, ", $cr), out(reg) v, options(nomem, nostack, preserves_flags)) };
            v
        }
    };
}

macro_rules! cr_write {
    ($name:ident, $cr:literal) => {
        #[inline]
        pub fn $name(v: u64) {
            unsafe { asm!(concat!("mov ", $cr, ", {}"), in(reg) v, options(nomem, nostack, preserves_flags)) };
        }
    };
}

cr_read!(cr0, "cr0");
cr_read!(cr2, "cr2");
cr_read!(cr3, "cr3");
cr_read!(cr4, "cr4");
cr_write!(set_cr0, "cr0");
cr_write!(set_cr3, "cr3");
cr_write!(set_cr4, "cr4");

/// `CR0.WP` — kernel writes to read-only pages fault (needed for COW later).
pub const CR0_WP: u64 = 1 << 16;
/// `CR0.PG` — paging enable.
pub const CR0_PG: u64 = 1 << 31;
/// `CR4.PAE` — physical address extension (mandatory for long mode).
pub const CR4_PAE: u64 = 1 << 5;
/// `CR4.PGE` — global pages.
pub const CR4_PGE: u64 = 1 << 7;
/// `CR4.OSFXSR` / `CR4.OSXMMEXCPT` — SSE enable (M0.3+, see design §7.3).
pub const CR4_OSFXSR: u64 = 1 << 9;
pub const CR4_OSXMMEXCPT: u64 = 1 << 10;
/// `CR0.EM` — no FPU (must be clear) / `CR0.TS` — lazy FPU switch (must be clear
/// while the kernel runs `fxsave`/`fxrstor` eagerly).
pub const CR0_EM: u64 = 1 << 2;
pub const CR0_TS: u64 = 1 << 3;

/// Let user code use SSE2 and let the kernel use `fxsave`/`fxrstor`.
///
/// Firmware usually enables this already (OVMF does), but the kernel must not
/// depend on the firmware for a feature it relies on: without `CR4.OSFXSR` an
/// SSE instruction in ring3 raises `#UD`, and with `CR0.TS` set `fxsave`
/// faults.  The kernel itself stays soft-float — only the save/restore of
/// *user* state touches the FPU.
pub fn enable_sse() {
    let cr0 = cr0();
    set_cr0((cr0 & !(CR0_EM | CR0_TS)) | CR0_WP);
    let cr4 = cr4();
    set_cr4(cr4 | CR4_OSFXSR | CR4_OSXMMEXCPT);
}

// ---------------------------------------------------------------- MSRs

pub const MSR_EFER: u32 = 0xC000_0080;
pub const MSR_GS_BASE: u32 = 0xC000_0101;
pub const MSR_STAR: u32 = 0xC000_0081;
pub const MSR_LSTAR: u32 = 0xC000_0082;
pub const MSR_FMASK: u32 = 0xC000_0084;
pub const MSR_KERNEL_GS_BASE: u32 = 0xC000_0102;

pub const EFER_SCE: u64 = 1 << 0;
pub const EFER_LME: u64 = 1 << 8;
pub const EFER_NXE: u64 = 1 << 11;

#[inline]
pub fn read_msr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags));
    }
    ((hi as u64) << 32) | lo as u64
}

#[inline]
pub fn write_msr(msr: u32, value: u64) {
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack, preserves_flags),
        );
    }
}

// ---------------------------------------------------------------- CPUID

pub use core::arch::x86_64::CpuidResult;

#[inline]
pub fn cpuid(leaf: u32, subleaf: u32) -> CpuidResult {
    core::arch::x86_64::__cpuid_count(leaf, subleaf)
}

/// CPUID leaf 0x8000_0001 EDX bit 20 — NX / XD support.
#[inline]
pub fn has_nx() -> bool {
    (cpuid(0x8000_0001, 0).edx & (1 << 20)) != 0
}

// ------------------------------------------------------ interrupt flags

#[inline]
pub fn sti() {
    unsafe { asm!("sti", options(nomem, nostack)) }
}

#[inline]
pub fn cli() {
    unsafe { asm!("cli", options(nomem, nostack)) }
}

#[inline]
pub fn hlt() {
    unsafe { asm!("hlt", options(nomem, nostack)) }
}

#[inline]
pub fn halt_loop() -> ! {
    loop {
        hlt();
    }
}

#[inline]
pub fn read_rflags() -> u64 {
    let f: u64;
    unsafe { asm!("pushfq; pop {}", out(reg) f, options(nomem, preserves_flags)) };
    f
}

#[inline]
pub fn read_rsp() -> usize {
    let sp: usize;
    unsafe { asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack, preserves_flags)) };
    sp
}

#[inline]
pub fn interrupts_enabled() -> bool {
    (read_rflags() & (1 << 9)) != 0
}

// ----------------------------------------------------- descriptor tables

#[repr(C, packed)]
pub struct DescriptorTablePointer {
    pub limit: u16,
    pub base: u64,
}

#[inline]
pub fn lgdt(dtr: &DescriptorTablePointer) {
    unsafe { asm!("lgdt [{}]", in(reg) dtr, options(readonly, nostack, preserves_flags)) }
}

#[inline]
pub fn lidt(dtr: &DescriptorTablePointer) {
    unsafe { asm!("lidt [{}]", in(reg) dtr, options(readonly, nostack, preserves_flags)) }
}

#[inline]
pub fn sidt(dtr: &mut DescriptorTablePointer) {
    unsafe { asm!("sidt [{}]", in(reg) dtr, options(nostack, preserves_flags)) }
}

// ------------------------------------------------------------------- TLB

#[inline]
pub fn invlpg(va: usize) {
    unsafe { asm!("invlpg [{}]", in(reg) va, options(nomem, nostack, preserves_flags)) }
}

/// Reload CR3, flushing the whole non-global TLB.
#[inline]
pub fn flush_tlb() {
    let cr3 = cr3();
    set_cr3(cr3);
}
