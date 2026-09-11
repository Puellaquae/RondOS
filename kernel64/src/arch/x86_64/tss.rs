//! Task State Segment — M0.3.
//!
//! One static TSS for the single CPU.  Its job in long mode is narrow but
//! essential:
//!
//! * `RSP0` is the stack the CPU switches to when ring3 enters ring0
//!   (interrupt, exception or `int 0x80`).  The scheduler updates it on every
//!   context switch so each thread gets its own kernel stack.
//! * `IST1..7` give dedicated stacks for exceptions that must not run on a
//!   possibly-corrupt stack (`#DF`, `#MC`, NMI).  Unused for now.
//! * `iomap_base` is set past the limit, so **ring3 has no I/O port access at
//!   all** — any `in`/`out` raises `#GP` (design §9).

use core::cell::UnsafeCell;
use core::mem::size_of;

/// 64-bit TSS, 104 bytes (Intel SDM 7.7).
#[repr(C, packed)]
pub struct TaskStateSegment {
    reserved0: u32,
    pub rsp: [u64; 3],
    reserved1: u64,
    pub ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    pub iomap_base: u16,
}

impl TaskStateSegment {
    const fn new() -> Self {
        Self {
            reserved0: 0,
            rsp: [0; 3],
            reserved1: 0,
            ist: [0; 7],
            reserved2: 0,
            reserved3: 0,
            iomap_base: 0,
        }
    }
}

/// Dedicated stack for `#DF` (and NMI), so a fault that happens while the normal
/// kernel stack is broken still produces a readable report instead of a triple
/// fault and a silent reset.  Lives in `.bss`, i.e. in the kernel image the
/// loader maps read/write.
#[repr(align(16))]
struct IstStack(UnsafeCell<[u8; 8192]>);

unsafe impl Sync for IstStack {}

static DF_STACK: IstStack = IstStack(UnsafeCell::new([0; 8192]));

/// Top of the `#DF` stack (stacks grow down).
pub fn df_stack_top() -> u64 {
    DF_STACK.0.get() as u64 + 8192
}

#[repr(align(16))]
struct TssCell(UnsafeCell<TaskStateSegment>);

unsafe impl Sync for TssCell {}

static TSS: TssCell = TssCell(UnsafeCell::new(TaskStateSegment::new()));

#[inline]
fn tss() -> *mut TaskStateSegment {
    TSS.0.get()
}

/// Physical/virtual address of the TSS, for the GDT descriptor.
pub fn base() -> usize {
    tss() as usize
}

pub fn init() {
    unsafe {
        (*tss()).iomap_base = size_of::<TaskStateSegment>() as u16;
        // #DF runs on its own stack: by definition the normal one is suspect.
        (*tss()).ist[0] = df_stack_top();
    }
}

/// Kernel stack top used when entering ring0 from ring3.
pub fn set_rsp0(rsp0: u64) {
    unsafe {
        (*tss()).rsp[0] = rsp0;
    }
}

pub fn rsp0() -> u64 {
    unsafe { (*tss()).rsp[0] }
}

pub fn ist(index: usize) -> u64 {
    assert!(index < 7);
    unsafe { (*tss()).ist[index] }
}

pub fn set_ist(index: usize, value: u64) {
    assert!(index < 7);
    unsafe {
        (*tss()).ist[index] = value;
    }
}
