//! Kernel threads + round-robin scheduler — M0.5.
//!
//! Ported from the i686 scheduler to the canonical 64-bit [`TrapFrame`].  The
//! design is unchanged: *all* context switches happen on an interrupt-style
//! entry, where the full register state is already saved in a fixed layout, so
//! there is no separate cooperative switch primitive.  `sleep`, `thread_exit`
//! and `WaitQueue::wait` simply mark the thread's state and raise `int 0x81`
//! (vector [`VECTOR_YIELD`]), which re-enters the same `schedule` path.
//!
//! Differences from i686:
//!
//! * a thread's saved state is a [`TrapFrame`] pointer, not a raw stack slot;
//! * `iretq` pops `RSP`/`SS`, so a brand-new thread's frame carries its stack
//!   top and the kernel data selector explicitly;
//! * the per-CPU block tracks the current thread and mirrors the thread's
//!   kernel stack into `TSS.RSP0`, which is what ring3 traps switch to.

#![allow(dead_code)]

use core::array;
use core::mem::size_of;

use crate::arch::x86_64::gdt::{KERNEL_CODE, KERNEL_DATA};
use crate::arch::x86_64::intr::{TrapFrame, VECTOR_TIMER};
use crate::arch::x86_64::{hlt, percpu};
use crate::mm;
use crate::utils::singleton::Singleton;

/// PIT is configured to 200 Hz, i.e. one tick every 5 ms.
pub const TICK_MS: u64 = 5;

pub const MAX_THREADS: usize = 64;

const NONE: u32 = u32::MAX;
const MAIN_ID: usize = 0;
const IDLE_ID: usize = 1;

/// Kernel stack size of every created thread, in 4 KiB pages.
const STACK_PAGES: usize = 4;

/// `RFLAGS.IF`.
const EFLAGS_IF: u64 = 0x200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Saved [`TrapFrame`] while the thread is off-CPU.
    frame: usize,
    /// Kernel stack base (allocated pages), or null for the boot stack.
    stack: *mut u8,
    stack_pages: usize,
    /// Top of this thread's kernel stack; mirrored into `TSS.RSP0`.
    kstack_top: usize,
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
            kstack_top: 0,
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
    /// Thread whose stack must be reclaimed on the next tick (it is still in
    /// use by the interrupt that noticed it dying).
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

    fn reclaim(&mut self, idx: usize) {
        let (stack, pages) = (self.threads[idx].stack, self.threads[idx].stack_pages);
        if pages != 0 && !stack.is_null() {
            mm::page_alloc().free_page(stack, pages);
        }
        self.threads[idx] = Thread::new();
    }
}

static SCHED: Singleton<Scheduler> = Singleton::UNINIT;

fn sched() -> &'static mut Scheduler {
    SCHED.get_mut()
}

/// Build the initial frame for a brand-new kernel thread.  `iretq` pops `rsp`
/// and `ss`, so both must be filled with real values.
unsafe fn build_initial_frame(stack_top: usize, entry: usize) -> usize {
    let base = stack_top - size_of::<TrapFrame>();
    core::ptr::write_bytes(base as *mut u8, 0, size_of::<TrapFrame>());
    let f = &mut *(base as *mut TrapFrame);
    f.rip = entry as u64;
    f.cs = KERNEL_CODE as u64;
    f.rflags = EFLAGS_IF;
    // SysV: rsp % 16 == 8 at a function entry (the CPU pushed no return
    // address for us).  `stack_top` is page aligned, so drop 8 bytes.
    f.rsp = (stack_top - 8) as u64;
    f.ss = KERNEL_DATA as u64;
    base
}

/// Where every freshly created thread starts.
extern "C" fn thread_entry_trampoline() -> ! {
    let ptr = percpu::current_thread() as *const Thread;
    let (entry, arg) = unsafe { ((*ptr).entry, (*ptr).arg) };
    if let Some(f) = entry {
        f(arg);
    }
    thread_exit()
}

fn idle_loop(_arg: usize) {
    loop {
        hlt();
    }
}

/// Initialize the scheduler.  Must be called once, before interrupts are
/// enabled: the boot flow becomes the "main" thread and the idle thread's
/// stack is allocated here.
pub fn init() {
    crate::arch::x86_64::cli();
    let s = sched();

    let main_t = &mut s.threads[MAIN_ID];
    *main_t = Thread::new();
    main_t.used = true;
    main_t.id = MAIN_ID as u32;
    main_t.name = "main";
    main_t.state = ThreadState::Running;
    // The boot thread runs on the trampoline stack; keep it for now.
    main_t.kstack_top = (crate::arch::x86_64::read_rsp() & !0xf) as usize;

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
    idle_t.kstack_top = idle_t.stack as usize + STACK_PAGES * 4096;
    idle_t.frame = unsafe {
        build_initial_frame(
            idle_t.kstack_top,
            thread_entry_trampoline as *const () as usize,
        )
    };

    s.current = MAIN_ID;
    s.idle = IDLE_ID;

    let main_ptr = &mut s.threads[MAIN_ID] as *mut Thread as u64;
    let main_top = s.threads[MAIN_ID].kstack_top as u64;
    percpu::set_current_thread(main_ptr);
    percpu::set_kernel_stack(main_top);

    crate::arch::x86_64::intr::set_sched_hook(sched_entry);
}

fn create_slot(s: &mut Scheduler) -> Option<usize> {
    (2..MAX_THREADS).find(|&i| !s.threads[i].used)
}

/// Create a new kernel thread.  Returns the thread id on success.
pub fn thread_create(name: &'static str, entry: fn(usize), arg: usize) -> Option<u32> {
    crate::arch::x86_64::cli();
    let result = {
        let s = sched();
        (|| {
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
            t.kstack_top = top;
            t.frame = unsafe { build_initial_frame(top, thread_entry_trampoline as *const () as usize) };
            s.enqueue(slot);
            Some(slot as u32)
        })()
    };
    crate::arch::x86_64::sti();
    result
}

/// `fn(frame, vector) -> next_frame` installed as the scheduler hook.
fn sched_entry(frame: usize, vector: usize) -> usize {
    if vector == VECTOR_TIMER {
        let s = sched();
        s.ticks += 1;

        // Reclaim a thread that died on the previous tick: its stack is now
        // free because this tick runs on a different stack.
        if s.pending_reap != NONE as usize {
            let idx = s.pending_reap;
            s.reclaim(idx);
            s.pending_reap = NONE as usize;
        }

        // Wake sleepers.
        for i in 0..MAX_THREADS {
            if s.threads[i].used
                && s.threads[i].state == ThreadState::Blocked
                && s.threads[i].sleep_until <= s.ticks
            {
                s.threads[i].state = ThreadState::Ready;
                s.enqueue(i);
            }
        }
    }
    schedule(frame)
}

/// Core context switch.  `cur_frame` is the frame built by the entry stub of
/// the thread being switched out; returns the frame to resume.
pub fn schedule(cur_frame: usize) -> usize {
    let s = sched();
    let cur = s.current;
    let cur_state = s.threads[cur].state;
    let cur_is_idle = s.threads[cur].is_idle;

    match cur_state {
        ThreadState::Running => {
            s.threads[cur].frame = cur_frame;
            s.threads[cur].state = ThreadState::Ready;
            if !cur_is_idle {
                s.enqueue(cur);
            }
        }
        _ => {
            // Blocked / Dying / Ready: keep the frame, do not re-enqueue.
            s.threads[cur].frame = cur_frame;
        }
    }

    let next = match s.dequeue() {
        Some(h) => h,
        None => {
            if cur_is_idle {
                cur
            } else {
                s.idle
            }
        }
    };

    s.threads[next].state = ThreadState::Running;
    let next_frame = if next == cur {
        cur_frame
    } else {
        s.threads[next].frame
    };

    if cur_state == ThreadState::Dying && next != cur {
        s.pending_reap = cur;
    }

    s.current = next;
    let next_ptr = &mut s.threads[next] as *mut Thread as u64;
    let next_top = s.threads[next].kstack_top as u64;
    percpu::set_current_thread(next_ptr);
    percpu::set_kernel_stack(next_top);

    next_frame
}

pub fn current_id() -> u32 {
    let s = sched();
    s.threads[s.current].id
}

pub fn current_name() -> &'static str {
    let s = sched();
    s.threads[s.current].name
}

pub fn ticks() -> u64 {
    crate::arch::x86_64::cli();
    let t = sched().ticks;
    crate::arch::x86_64::sti();
    t
}

/// Block the current thread until the next tick takes it off the CPU.
fn block_current() {
    {
        let s = sched();
        s.threads[s.current].state = ThreadState::Blocked;
    }
    // Re-enter the scheduler through the yield gate; returns when woken.
    unsafe { core::arch::asm!("int 0x81", options(nomem, nostack)) };
}

/// Sleep for at least `ms` milliseconds.
pub fn sleep(ms: u64) {
    {
        let s = sched();
        let me = s.current;
        if s.threads[me].state == ThreadState::Dying {
            return;
        }
        s.threads[me].state = ThreadState::Blocked;
        s.threads[me].sleep_until = s.ticks + (ms + TICK_MS - 1) / TICK_MS;
    }
    unsafe { core::arch::asm!("int 0x81") };
}

/// Give up the CPU for one round.
pub fn yield_now() {
    // The handler saves and restores every register, but it *does* touch
    // memory (scheduler state), so no `nomem`/`nostack` options here.
    unsafe { core::arch::asm!("int 0x81") };
}

/// Terminate the calling thread.  Its kernel stack is reclaimed one tick later.
pub fn thread_exit() -> ! {
    {
        let s = sched();
        let me = s.current;
        s.threads[me].state = ThreadState::Dying;
    }
    unsafe { core::arch::asm!("int 0x81") };
    // Never resumed: the scheduler does not re-enqueue a Dying thread.
    loop {
        hlt();
    }
}

pub fn wake(id: u32) {
    if id == NONE {
        return;
    }
    let s = sched();
    let idx = id as usize;
    if s.threads[idx].used && s.threads[idx].state == ThreadState::Blocked {
        s.threads[idx].state = ThreadState::Ready;
        s.enqueue(idx);
    }
}

/// A minimal FIFO wait queue.  `wait` blocks the current thread and is woken by
/// `wake_one`/`wake_all`; the object that owns the queue decides the condition.
pub struct WaitQueue {
    waiters: [u32; MAX_THREADS],
    len: usize,
}

impl WaitQueue {
    pub const fn new() -> Self {
        Self {
            waiters: [NONE; MAX_THREADS],
            len: 0,
        }
    }

    /// Block the current thread on this queue.  Callers must have re-checked
    /// their condition while holding whatever lock protects it (no lost
    /// wakeups on a single CPU with interrupts off).
    pub fn wait(&mut self) {
        crate::arch::x86_64::cli();
        let me = current_id();
        if self.len < MAX_THREADS {
            self.waiters[self.len] = me;
            self.len += 1;
        }
        block_current();
    }

    pub fn wake_one(&mut self) -> bool {
        crate::arch::x86_64::cli();
        if self.len == 0 {
            return false;
        }
        let id = self.waiters[0];
        for i in 1..self.len {
            self.waiters[i - 1] = self.waiters[i];
        }
        self.len -= 1;
        wake(id);
        true
    }

    pub fn wake_all(&mut self) {
        crate::arch::x86_64::cli();
        for i in 0..self.len {
            wake(self.waiters[i]);
        }
        self.len = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}
