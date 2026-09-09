//! Kernel-mode test programs and their harness.
//!
//! The smoke suite used to live inline in `main.rs`, which made the boot path
//! and the tests the same 600-line function.  Tests are now independent
//! **kernel-mode programs** in their own modules:
//!
//! * each program is a `fn() -> Verdict`, listed once in its module's `CASES`
//!   table, and reports only its verdict — the harness prints the name;
//! * programs that produce several results (the user-process group) return
//!   [`Verdict::Reported`] and call [`report`] themselves;
//! * the ring3 probes are the only asynchronous ones: `iretq` never returns to
//!   the caller, so `ring3::start()` hands control to a continuation that
//!   re-enters the harness through [`boot_ready`].
//!
//! Execution order matters and is explicit:
//!
//! 1. [`run_early`] — mm/elf programs, interrupts still off (they must not be
//!    preempted while holding kernel tables);
//! 2. boot code initialises GDT/IDT/PIC/PIT and the scheduler;
//! 3. [`ring3::start`] — ring3 probes, then [`boot_ready`];
//! 4. [`boot_ready`] — scheduler + user programs, then the summary.

#![allow(dead_code)]

use core::sync::atomic::{AtomicUsize, Ordering};

pub mod elf;
pub mod mm;
pub mod ring3;
pub mod sched;
pub mod user;

use crate::arch::x86_64::{halt_loop, pic};
use crate::thread;

/// PIT frequency used by the scheduler tests.
pub const TIMER_HZ: u32 = 200;

static REPORTS: AtomicUsize = AtomicUsize::new(0);
static FAILS: AtomicUsize = AtomicUsize::new(0);

/// Verdict of one test program.
pub enum Verdict {
    Pass,
    Fail,
    /// The program already called [`report`] for its sub-results.
    Reported,
}

/// One kernel-mode test program.
pub struct Case {
    pub name: &'static str,
    pub run: fn() -> Verdict,
}

/// Print and count one result.  `make test` greps these lines.
pub fn report(name: &str, ok: bool) {
    REPORTS.fetch_add(1, Ordering::Relaxed);
    if !ok {
        FAILS.fetch_add(1, Ordering::Relaxed);
    }
    crate::serial_println!("[{}] {}", if ok { " ok " } else { "FAIL" }, name);
}

/// Convenience for a program that wants to bail out early with a message.
pub fn fail(name: &str, why: &str) -> bool {
    crate::serial_println!("[FAIL] {}: {}", name, why);
    false
}

/// Run a table of synchronous programs in order.
pub fn run_cases(cases: &[Case]) {
    for case in cases {
        match (case.run)() {
            Verdict::Pass => report(case.name, true),
            Verdict::Fail => report(case.name, false),
            Verdict::Reported => {}
        }
    }
}

/// Programs that must run before interrupts are enabled.
pub fn run_early() {
    run_cases(mm::CASES);
    run_cases(elf::CASES);
}

/// Bring up the scheduler and run the remaining programs.
///
/// Called by the ring3 continuation, i.e. after the probes have come back to
/// ring0.  It is `-> !` because it ends with the summary and `halt_loop`.
pub fn boot_ready() -> ! {
    pic::init();
    pic::configure_pit(0, 2, TIMER_HZ);
    thread::init();
    crate::arch::x86_64::sti();

    run_cases(sched::CASES);
    run_cases(user::CASES);

    summary()
}

fn summary() -> ! {
    crate::serial_println!("----------------------------------------------");
    let total = REPORTS.load(Ordering::Relaxed);
    let failed = FAILS.load(Ordering::Relaxed);
    if failed == 0 {
        crate::serial_println!("smoke: ALL PASS ({}/{})", total, total);
    } else {
        crate::serial_println!("smoke: {} FAILURE(S) of {}", failed, total);
    }
    halt_loop()
}
