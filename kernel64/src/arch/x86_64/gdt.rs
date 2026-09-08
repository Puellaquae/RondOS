//! 64-bit GDT — M0.3.
//!
//! In long mode segmentation is mostly vestigial: only `CS`/`SS` matter (for
//! CPL) and the base/limit of the others are ignored.  We still need:
//!
//! | selector | index | purpose |
//! | --- | --- | --- |
//! | `0x08` | 1 | kernel code, DPL0, L=1 |
//! | `0x10` | 2 | kernel data, DPL0 |
//! | `0x1b` | 3 | **user code**, DPL3, L=1 |
//! | `0x23` | 4 | **user data**, DPL3 |
//! | `0x28` | 5 | TSS, 16-byte system descriptor |
//!
//! The TSS is what makes ring3 → ring0 work: on an interrupt/`int n` from CPL3
//! the CPU loads `RSP0` from it, i.e. the kernel stack.  Its I/O bitmap base is
//! set past the limit, so *any* `in`/`out` from ring3 raises `#GP`.

use core::cell::UnsafeCell;
use core::mem::size_of;

use super::DescriptorTablePointer;

pub const KERNEL_CODE: u16 = 0x08;
pub const KERNEL_DATA: u16 = 0x10;
pub const USER_CODE: u16 = 0x1b;
pub const USER_DATA: u16 = 0x23;
pub const TSS_SELECTOR: u16 = 0x28;

/// null, kcode, kdata, ucode, udata, tss lo, tss hi
const GDT_ENTRIES: usize = 7;

#[repr(C, align(16))]
struct GdtTable(UnsafeCell<[u64; GDT_ENTRIES]>);

unsafe impl Sync for GdtTable {}

static GDT: GdtTable = GdtTable(UnsafeCell::new([0; GDT_ENTRIES]));

/// Code segment: P=1, S=1, type=0xA (exec/read), G=1, L=1 (64-bit), limit=4 GiB.
const fn code_segment(dpl: u16) -> u64 {
    let access = 0x9A | ((dpl as u64) << 5);
    let flags = 0xAF; // G=1, D=0, L=1, limit[19:16]=0xF
    0x0000_FFFF | (access << 40) | (flags << 48)
}

/// Data segment: P=1, S=1, type=0x2 (read/write), G=1, D=1.
const fn data_segment(dpl: u16) -> u64 {
    let access = 0x92 | ((dpl as u64) << 5);
    let flags = 0xCF;
    0x0000_FFFF | (access << 40) | (flags << 48)
}

/// Low half of a 64-bit TSS descriptor (type 0x9 = available 64-bit TSS).
///
/// The base is split across three non-adjacent fields — bits 0..23 go to
/// bits 16..39 of the descriptor, bits 24..31 to bits 56..63, and bits 32..63
/// to the *high* half of the 16-byte descriptor.  Forgetting the 24..31 piece
/// silently corrupts the TSS base (a higher-half address like
/// `0xFFFF_FFFF_8021_1F00` becomes `0xFFFF_FFFF_0021_1F00`), so the CPU reads
/// `RSP0` from unmapped memory and the first ring3 trap triple-faults.
const fn tss_descriptor_low(base: u64, limit: u64) -> u64 {
    (limit & 0xffff)
        | ((base & 0xff_ffff) << 16)
        | (0x9u64 << 40) // type, S=0, DPL=0
        | (1u64 << 47) // present
        | (((limit >> 16) & 0xf) << 48)
        | (((base >> 24) & 0xff) << 56)
}

/// Reload CS through the (new) GDT with a far return.
fn reload_cs() {
    unsafe {
        core::arch::asm!(
            "push {sel}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            sel = in(reg) KERNEL_CODE as u64,
            tmp = out(reg) _,
            options(preserves_flags),
        );
    }
}

/// Install the kernel GDT and load the task register.  Must be called once,
/// before any ring3 entry.
pub fn init() {
    let base = super::tss::base() as u64;
    let limit = (size_of::<super::tss::TaskStateSegment>() - 1) as u64;

    unsafe {
        let gdt = GDT.0.get();
        (*gdt)[0] = 0;
        (*gdt)[1] = code_segment(0);
        (*gdt)[2] = data_segment(0);
        (*gdt)[3] = code_segment(3);
        (*gdt)[4] = data_segment(3);
        (*gdt)[5] = tss_descriptor_low(base, limit);
        (*gdt)[6] = base >> 32;

        let dtr = DescriptorTablePointer {
            limit: (GDT_ENTRIES * 8 - 1) as u16,
            base: gdt as u64,
        };
        super::lgdt(&dtr);

        reload_cs();

        core::arch::asm!(
            "mov ax, {sel:x}",
            "mov ds, ax",
            "mov es, ax",
            "mov ss, ax",
            "mov fs, ax",
            "mov gs, ax",
            sel = in(reg) KERNEL_DATA,
            options(preserves_flags),
        );
    }

    super::tss::init();

    unsafe {
        core::arch::asm!("ltr {sel:x}", sel = in(reg) TSS_SELECTOR, options(preserves_flags));
    }
}
