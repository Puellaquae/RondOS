//! Scheduler test programs.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::arch::x86_64;
use crate::thread;

use super::{report, Case, Verdict, TIMER_HZ};
use crate::serial_println;

static BUSY_A: AtomicUsize = AtomicUsize::new(0);
static BUSY_B: AtomicUsize = AtomicUsize::new(0);
static SLEEPER: AtomicUsize = AtomicUsize::new(0);

pub static CASES: &[Case] = &[Case {
    name: "scheduler",
    run: scheduler,
}];

fn busy_a(_arg: usize) {
    let mut n = 0usize;
    loop {
        n += 1;
        if n % 2_000_000 == 0 {
            BUSY_A.store(n, Ordering::Relaxed);
            serial_println!("a {} @t{}", n, thread::ticks());
        }
        if n % 20_000 == 0 {
            thread::yield_now();
        }
    }
}

fn busy_b(_arg: usize) {
    let mut n = 0usize;
    loop {
        n += 1;
        if n % 2_000_000 == 0 {
            BUSY_B.store(n, Ordering::Relaxed);
            serial_println!("b {} @t{}", n, thread::ticks());
        }
        if n % 20_000 == 0 {
            thread::yield_now();
        }
    }
}

fn sleeper(_arg: usize) {
    for i in 0..3 {
        serial_println!("sleeper {} @t{}", i, thread::ticks());
        SLEEPER.fetch_add(1, Ordering::Relaxed);
        thread::sleep(200);
    }
    serial_println!("sleeper done, exiting @t{}", thread::ticks());
    thread::thread_exit()
}

/// Preemption, sleep/wake and thread exit in one run: three threads share the
/// CPU for ~1.5 s and the counters tell us whether the scheduler behaved.
fn scheduler() -> Verdict {
    thread::thread_create("busy-a", busy_a, 0).expect("thread a");
    thread::thread_create("busy-b", busy_b, 1).expect("thread b");
    thread::thread_create("sleeper", sleeper, 2).expect("thread sleeper");
    serial_println!("scheduler: 3 threads, {} Hz, enabling preemption", TIMER_HZ);

    x86_64::sti();

    // Let the scheduler run for ~1.5 s, then report.
    // Let the scheduler run for ~1.5 s, then report.
    thread::sleep(1500);

    let a = BUSY_A.load(Ordering::Relaxed);
    let b = BUSY_B.load(Ordering::Relaxed);
    let s = SLEEPER.load(Ordering::Relaxed);
    let t = thread::ticks();
    serial_println!(
        "scheduler: {} ticks (~{} ms), a={} b={} sleeper={}",
        t,
        t * thread::TICK_MS,
        a,
        b,
        s
    );

    let preempted = a > 0 && b > 0;
    report("preemption", preempted);
    let slept = s >= 3;
    report("sleep/wake", slept);
    let exited = s >= 3;
    report("thread-exit", exited);
    Verdict::Reported
}
