//! Interrupt enable/disable helpers and a scoped guard that restores the
//! previous interrupt state on drop.
//!
//! This is the single arch-specific hook the arch-neutral kernel code (heap,
//! scheduler, ...) needs to make critical sections preemption-safe on a
//! uniprocessor.

use core::arch::asm;

/// Current `eflags` value.
pub fn eflags() -> u32 {
    let f: u32;
    unsafe {
        asm!("pushfd", "pop {0}", out(reg) f, options(preserves_flags));
    }
    f
}

/// Does the current `eflags` have the interrupt flag set?
#[inline]
pub fn interrupts_enabled() -> bool {
    eflags() & 0x200 != 0
}

/// Disables interrupts and restores the previous state on drop.
pub struct InterruptGuard {
    reenable: bool,
}

impl InterruptGuard {
    /// Enter a critical section: disable interrupts (if enabled) and return a
    /// guard that restores the prior state when dropped.
    #[must_use]
    pub fn new() -> InterruptGuard {
        let reenable = interrupts_enabled();
        if reenable {
            unsafe {
                asm!("cli", options(nomem, nostack, preserves_flags));
            }
        }
        InterruptGuard { reenable }
    }
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        if self.reenable {
            unsafe {
                asm!("sti", options(nomem, nostack, preserves_flags));
            }
        }
    }
}
