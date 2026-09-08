//! Per-CPU data + `swapgs` — M0.3.
//!
//! Long mode has no `FSBASE`/`GSBASE` MSRs on i686, but on x86-64 the `GS` base
//! is a full 64-bit register we can point at per-CPU state and reach with
//! `gs:[offset]`.  Two MSRs cooperate:
//!
//! * `IA32_GS_BASE`         — the base in effect right now
//! * `IA32_KERNEL_GS_BASE`  — the base `swapgs` exchanges it with
//!
//! Convention (same as Linux):
//!
//! ```text
//!   in kernel:  GS.base = &CPU_LOCAL      KERNEL_GS_BASE = user's GS base
//!   in user:    GS.base = user TLS base   KERNEL_GS_BASE = &CPU_LOCAL
//! ```
//!
//! So the entry path does one `swapgs` to get at per-CPU state, and the exit
//! path does one more to give the user its base back.  Because the kernel is
//! entered from ring3 through an interrupt gate, the kernel stack comes from
//! `TSS.RSP0`; per-CPU state is what the scheduler will use to find the current
//! thread without touching user-controllable memory.

use core::cell::UnsafeCell;

use super::{read_msr, write_msr, MSR_GS_BASE, MSR_KERNEL_GS_BASE};

#[repr(C)]
pub struct CpuLocal {
    /// Points at itself; makes the struct findable from a raw `gs` base.
    pub self_ptr: u64,
    /// Top of the kernel stack for ring3 entry (mirrors `TSS.RSP0`).
    pub kernel_stack_top: u64,
    /// Scratch slot used by the `syscall` fast path later (M0.4+).
    pub user_rsp: u64,
    /// Opaque pointer to the current `Thread` (set by the scheduler, M0.5).
    pub current_thread: u64,
    pub preempt_count: u32,
    pub need_resched: u32,
}

impl CpuLocal {
    const fn new() -> Self {
        Self {
            self_ptr: 0,
            kernel_stack_top: 0,
            user_rsp: 0,
            current_thread: 0,
            preempt_count: 0,
            need_resched: 0,
        }
    }
}

#[repr(align(64))]
struct CpuLocalCell(UnsafeCell<CpuLocal>);

unsafe impl Sync for CpuLocalCell {}

static CPU_LOCAL: CpuLocalCell = CpuLocalCell(UnsafeCell::new(CpuLocal::new()));

pub fn local() -> *mut CpuLocal {
    CPU_LOCAL.0.get()
}

/// Make `gs:[0]` point at the per-CPU struct for the kernel.  Must run before
/// any `swapgs` (i.e. before ring3 entry).
pub fn init() {
    let p = local();
    unsafe {
        (*p).self_ptr = p as u64;
    }
    // Kernel-side base is the struct; the "user" side starts at 0.
    write_msr(MSR_GS_BASE, p as u64);
    write_msr(MSR_KERNEL_GS_BASE, 0);
}

/// Read a field of the current per-CPU struct through `gs:`.
#[macro_export]
macro_rules! cpu_local {
    ($field:ident) => {{
        let v: u64;
        unsafe {
            core::arch::asm!(
                "mov {}, gs:[{off}]",
                out(reg) v,
                off = const core::mem::offset_of!(
                    $crate::arch::x86_64::percpu::CpuLocal,
                    $field
                ),
                options(nomem, nostack, preserves_flags),
            );
        }
        v
    }};
}

pub fn set_kernel_stack(top: u64) {
    unsafe {
        (*local()).kernel_stack_top = top;
    }
    super::tss::set_rsp0(top);
}

pub fn kernel_stack() -> u64 {
    unsafe { (*local()).kernel_stack_top }
}

pub fn set_current_thread(t: u64) {
    unsafe {
        (*local()).current_thread = t;
    }
}

pub fn current_thread() -> u64 {
    unsafe { (*local()).current_thread }
}

/// `swapgs` — exchange `GS.base` and `IA32_KERNEL_GS_BASE`.
#[inline]
pub fn swapgs() {
    unsafe { core::arch::asm!("swapgs", options(nomem, nostack, preserves_flags)) }
}

/// Read the raw GS base MSR (diagnostics).
pub fn gs_base() -> u64 {
    read_msr(MSR_GS_BASE)
}
