//! Kernel threads on a single CPU with preemptive round-robin scheduling.
//!
//! Design (see the design notes): every context switch happens inside the
//! IRQ0 (PIT timer) interrupt entry.  A thread's suspended state is always a
//! full 32-bit trap frame kept at the top of its own kernel stack:
//!
//! ```text
//!   low address
//!     [edi][esi][ebp][oesp][ebx][edx][ecx][eax]   <- saved by `pusha`
//!     [eip][cs][eflags]                           <- saved by the CPU
//!   high address
//! ```
//!
//! The IRQ0 entry stub `pusha`s the interrupted thread's registers, calls
//! [`sched_tick`], which returns the frame of the thread that must run next,
//! then executes `mov esp, frame; popa; iret`.  Because the stack pointer is
//! switched by the stub, threads never need a separate cooperative context
//! switch primitive: *all* switching goes through the timer tick.
//!
//! `sleep`/`thread_exit` therefore only change the thread's state and then
//! wait (with `hlt`) until the next tick actually takes the thread off the
//! CPU.  Voluntary blocking is merely a hint to the tick-driven scheduler.

#![allow(dead_code)]

use core::array;

use crate::arch::x86;
use crate::arch::x86::intr::end_of_interrupt;
use crate::loader;
use crate::mm;
use crate::utils::singleton::Singleton;

/// PIT is configured to 200 Hz, i.e. one tick every 5 ms.
pub const TICK_MS: u64 = 5;

/// Upper bound on simultaneously alive threads (TCB pool is static in BSS).
pub const MAX_THREADS: usize = 64;

/// Sentinel for the intrusive ready-queue links.
const NONE: u32 = u32::MAX;

/// Reserved TCB slots.
const MAIN_ID: usize = 0;
const IDLE_ID: usize = 1;

/// Stack size of every created thread, in 4 KiB pages.
const STACK_PAGES: usize = 2;

/// Size of one saved trap frame (8 GPRs + eip/cs/eflags).
const TRAP_FRAME_SIZE: usize = 44;

/// `RFLAGS.IF` set.
const EFLAGS_IF: u32 = 0x200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ThreadState {
    Ready,
    Running,
    Blocked,
    Dying,
}

pub struct Thread {
    used: bool,
    is_idle: bool,
    id: u32,
    name: &'static str,
    state: ThreadState,
    entry: Option<fn(usize)>,
    arg: usize,
    /// Base of the saved trap frame on this thread's stack; `mov esp, frame`
    /// followed by `popa; iret` resumes the thread.
    frame: usize,
    /// Kernel stack base (allocated pages), or null for the boot/idle stacks.
    stack: *mut u8,
    stack_pages: usize,
    sleep_until: u64,
    next: u32,
}

impl Thread {
    const fn new() -> Thread {
        Thread {
            used: false,
            is_idle: false,
            id: 0,
            name: "",
            state: ThreadState::Ready,
            entry: None,
            arg: 0,
            frame: 0,
            stack: core::ptr::null_mut(),
            stack_pages: 0,
            sleep_until: 0,
            next: NONE,
        }
    }
}

pub struct Scheduler {
    threads: [Thread; MAX_THREADS],
    ready_head: u32,
    ready_tail: u32,
    current: usize,
    idle: usize,
    ticks: u64,
    /// Slot of a thread whose stack must be reclaimed on the next tick.
    /// Reclamation is deferred because the tick that notices a `Dying` thread
    /// is still running on that very stack; one tick later the CPU is on a
    /// different thread's stack and the pages can be freed safely.
    pending_reap: usize,
}

impl Default for Scheduler {
    fn default() -> Self {
        Scheduler {
            threads: array::from_fn(|_| Thread::new()),
            ready_head: NONE,
            ready_tail: NONE,
            current: MAIN_ID,
            idle: IDLE_ID,
            ticks: 0,
            pending_reap: NONE as usize,
        }
    }
}

impl Scheduler {
    fn enqueue(&mut self, idx: usize) {
        debug_assert!(self.threads[idx].next == NONE);
        if self.ready_head == NONE {
            self.ready_head = idx as u32;
        } else {
            self.threads[self.ready_tail as usize].next = idx as u32;
        }
        self.threads[idx].next = NONE;
        self.ready_tail = idx as u32;
    }

    fn dequeue(&mut self) -> Option<usize> {
        if self.ready_head == NONE {
            return None;
        }
        let head = self.ready_head as usize;
        self.ready_head = self.threads[head].next;
        if self.ready_head == NONE {
            self.ready_tail = NONE;
        }
        self.threads[head].next = NONE;
        Some(head)
    }

    /// Returns the thread that must run next, switching `cur` back onto the
    /// ready queue first if it is still runnable.
    fn pick_next(&mut self, cur: usize, cur_frame: usize) -> usize {
        let (cur_state, cur_is_idle) = {
            let t = &self.threads[cur];
            (t.state, t.is_idle)
        };

        match cur_state {
            ThreadState::Running => {
                // Store this thread's interrupt frame so it can be resumed.
                self.threads[cur].frame = cur_frame;
                self.threads[cur].state = ThreadState::Ready;
                if !cur_is_idle {
                    self.enqueue(cur);
                }
            }
            ThreadState::Blocked | ThreadState::Dying | ThreadState::Ready => {
                // Blocked: sleeping, stays out of the ready queue until woken.
                // Dying:   leaves the CPU forever; reclaimed below.
                // Ready:   can only happen if a Blocked thread was just woken
                //          while still running its sleep wait-loop; it was
                //          already enqueued by the wake scan.
                self.threads[cur].frame = cur_frame;
            }
        }

        let chosen = match self.dequeue() {
            Some(head) => head,
            None => {
                if cur_is_idle {
                    cur
                } else {
                    self.idle
                }
            }
        };

        self.threads[chosen].state = ThreadState::Running;
        let next_frame = if chosen == cur {
            cur_frame
        } else {
            self.threads[chosen].frame
        };

        if cur_state == ThreadState::Dying && chosen != cur {
            // Do not free the stack here: this tick's IRQ is still running on
            // it. The TCB is left marked `Dying` (it is never re-enqueued and
            // never chosen again) and is reclaimed at the start of the next
            // tick, which runs on some other thread's stack.
            self.pending_reap = cur;
        }

        self.current = chosen;
        next_frame
    }

    fn reclaim(&mut self, idx: usize) {
        let (stack, stack_pages) = {
            let t = &self.threads[idx];
            (t.stack, t.stack_pages)
        };
        if stack_pages != 0 && !stack.is_null() {
            mm::page_alloc().free_page(stack, stack_pages);
        }
        self.threads[idx] = Thread::new();
    }
}

static SCHED: Singleton<Scheduler> = Singleton::UNINIT;

/// Build the initial fake trap frame for a brand-new thread.  `stack_top` is
/// the highest address of the thread's kernel stack; the frame is written just
/// below it so the first `popa; iret` starts the thread with `esp == stack_top`.
///
/// Returns the frame base to store in the TCB.
unsafe fn build_initial_frame(stack_top: usize, entry: usize) -> usize {
    let base = stack_top - TRAP_FRAME_SIZE;
    core::ptr::write_bytes(base as *mut u8, 0, TRAP_FRAME_SIZE);
    let q = base as *mut u32;
    q.add(8).write(entry as u32); // eip
    q.add(9).write(loader::SEGMENT_KERNEL_CODE as u32); // cs
    q.add(10).write(EFLAGS_IF); // eflags (interrupts on)
    base
}

/// Where every freshly created thread starts.  Reads the thread's entry point
/// from the scheduler (the tick path sets `current` before resuming it).
unsafe extern "C" fn thread_entry_trampoline() -> ! {
    x86::sti();
    let (entry, arg) = {
        let s = SCHED.get_mut();
        let me = s.current;
        let t = &s.threads[me];
        (t.entry, t.arg)
    };
    if let Some(f) = entry {
        f(arg);
    }
    thread_exit();
}

fn idle_loop(_arg: usize) {
    loop {
        x86::hlt();
    }
}

/// Initialize the scheduler: reserve the boot/main thread and the idle thread.
/// Must be called once, before interrupts are enabled.
pub fn init() {
    unsafe {
        x86::cli();
        let s = SCHED.get_mut();

        let main_t = &mut s.threads[MAIN_ID];
        *main_t = Thread::new();
        main_t.used = true;
        main_t.id = MAIN_ID as u32;
        main_t.name = "main";
        main_t.state = ThreadState::Running;

        // The idle thread is scheduled only when nothing else is runnable and
        // is never put on the ready queue.
        let idle_t = &mut s.threads[IDLE_ID];
        *idle_t = Thread::new();
        idle_t.used = true;
        idle_t.is_idle = true;
        idle_t.id = IDLE_ID as u32;
        idle_t.name = "idle";
        idle_t.state = ThreadState::Running;
        idle_t.entry = Some(idle_loop);
        idle_t.stack = mm::page_alloc().get_page(STACK_PAGES).expect("no idle stack");
        idle_t.stack_pages = STACK_PAGES;
        let top = idle_t.stack as usize + STACK_PAGES * 4096;
        idle_t.frame = build_initial_frame(top, thread_entry_trampoline as unsafe extern "C" fn() -> ! as usize);

        s.current = MAIN_ID;
        s.idle = IDLE_ID;
    }
}

fn create_slot(s: &mut Scheduler) -> Option<usize> {
    for i in 2..MAX_THREADS {
        if !s.threads[i].used {
            return Some(i);
        }
    }
    None
}

/// Create a new kernel thread with `STACK_PAGES` pages of kernel stack.
/// Returns the thread id (slot index) on success.
pub fn thread_create(name: &'static str, entry: fn(usize), arg: usize) -> Option<u32> {
    unsafe {
        x86::cli();
        let result = (|| {
            let s = SCHED.get_mut();
            let slot = create_slot(s)?;
            let stack = mm::page_alloc().get_page(STACK_PAGES)?;
            let top = stack as usize + STACK_PAGES * 4096;
            let t = &mut s.threads[slot];
            *t = Thread::new();
            t.used = true;
            t.id = slot as u32;
            t.name = name;
            t.state = ThreadState::Ready;
            t.entry = Some(entry);
            t.arg = arg;
            t.stack = stack;
            t.stack_pages = STACK_PAGES;
            t.frame = build_initial_frame(top, thread_entry_trampoline as unsafe extern "C" fn() -> ! as usize);
            s.enqueue(slot);
            Some(slot as u32)
        })();
        x86::sti();
        result
    }
}

/// Sleep for at least `ms` milliseconds.  The actual deschedule happens on the
/// next timer tick; until then the thread spins in an interruptible `hlt`
/// wait-loop.
pub fn sleep(ms: u64) {
    x86::cli();
    {
        let s = SCHED.get_mut();
        let me = s.current;
        let t = &mut s.threads[me];
        t.state = ThreadState::Blocked;
        t.sleep_until = s.ticks + (ms + TICK_MS - 1) / TICK_MS;
    }
    loop {
        let running = {
            let s = SCHED.get_mut();
            let me = s.current;
            s.threads[me].used && s.threads[me].state == ThreadState::Running
        };
        if running {
            break;
        }
        x86::sti();
        x86::hlt();
        x86::cli();
    }
    x86::sti();
}

/// Terminate the calling thread.  The thread marks itself `Dying`, waits for
/// the next tick to take it off the CPU, and is never resumed; its kernel
/// stack is freed by the scheduler at that point.
pub fn thread_exit() -> ! {
    x86::cli();
    let s = SCHED.get_mut();
    let me = s.current;
    s.threads[me].state = ThreadState::Dying;
    loop {
        x86::sti();
        x86::hlt();
        x86::cli();
    }
}

/// Current scheduler tick count (each tick is [`TICK_MS`] ms).
pub fn ticks() -> u64 {
    // The ISR updates `ticks` non-atomically; read it with interrupts off so
    // the 32-bit halves cannot be torn by a tick arriving mid-read.
    x86::cli();
    let t = SCHED.get_mut().ticks;
    x86::sti();
    t
}

/// IRQ0 (timer) handler, installed in the IDT at vector 0x20.
///
/// `frame` is the address of the interrupted thread's saved trap frame (the
/// stack pointer right after the entry stub executed `pusha`).  Returns the
/// trap frame of the thread that should resume.
#[no_mangle]
unsafe extern "C" fn sched_tick(cur_frame: usize) -> usize {
    end_of_interrupt();

    let s = SCHED.get_mut();
    s.ticks += 1;

    // Reclaim a thread that died last tick. Its stack is now unused because
    // this tick is running on some other thread's stack.
    if s.pending_reap != NONE as usize {
        s.reclaim(s.pending_reap);
        s.pending_reap = NONE as usize;
    }

    // Wake sleeping threads whose deadline has passed.
    for i in 0..MAX_THREADS {
        if s.threads[i].used
            && s.threads[i].state == ThreadState::Blocked
            && s.threads[i].sleep_until <= s.ticks
        {
            s.threads[i].state = ThreadState::Ready;
            s.enqueue(i);
        }
    }

    let cur = s.current;
    s.pick_next(cur, cur_frame)
}

/// Naked assembly entry point for the timer interrupt.
///
/// The CPU (interrupt gate) has already pushed `eip`, `cs`, `eflags` and
/// cleared IF.  We save all GP registers, call the scheduler, then jump to the
/// chosen thread's saved frame and return from interrupt.
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn irq0_stub() {
    core::arch::naked_asm!(
        "pusha",
        "mov eax, esp",
        "push eax",
        "call {tick}",
        "mov esp, eax",
        "popa",
        "iretd",
        tick = sym sched_tick,
    );
}
