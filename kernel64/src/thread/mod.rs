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

use core::cell::UnsafeCell;
use core::mem::size_of;

use crate::arch::x86_64::gdt::{KERNEL_CODE, KERNEL_DATA, USER_CODE, USER_DATA};
use crate::arch::x86_64::intr::{self, TrapFrame, VECTOR_TIMER};
use crate::arch::x86_64::paging::X86_64Paging;
use crate::arch::x86_64::{hlt, percpu};
use crate::mm;
use crate::mm::vm::PagingArch;

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

/// Per-thread FPU/SSE state (design §7.3).  512 bytes is the `FXSAVE` image
/// (x87 + MXCSR + XMM0..15); the kernel saves it eagerly on every context
/// switch, because user programs are built for SSE2 while the kernel itself is
/// soft-float and never touches the registers.
#[repr(C, align(16))]
pub struct FpuState([u8; 512]);

impl FpuState {
    const fn new() -> Self {
        Self([0; 512])
    }
}

/// `fxsave`/`fxrstor` require a 16-byte aligned 512-byte buffer; a misaligned
/// operand raises `#GP(0)` (which is how this was found).
#[inline]
unsafe fn fxsave(area: *mut u8) {
    debug_assert_eq!(area as usize % 16, 0, "fxsave needs a 16-byte aligned area");
    core::arch::asm!("fxsave [{}]", in(reg) area, options(nostack, preserves_flags));
}

#[inline]
unsafe fn fxrstor(area: *const u8) {
    debug_assert_eq!(area as usize % 16, 0, "fxrstor needs a 16-byte aligned area");
    core::arch::asm!("fxrstor [{}]", in(reg) area, options(nostack, preserves_flags));
}

/// A 512-byte FXSAVE image with a guaranteed 16-byte aligned start, even when
/// it lives on the stack (the compiler only aligns locals to `align_of::<T>()`,
/// which is not enough here because the requirement is hidden inside inline
/// asm).
struct AlignedFpu([u64; 66]);

impl AlignedFpu {
    fn new() -> Self {
        Self([0; 66])
    }

    fn ptr(&mut self) -> *mut u8 {
        ((self.0.as_mut_ptr() as usize + 15) & !15) as *mut u8
    }
}

/// Give a brand-new thread a clean FPU state: x87 reset (`fninit`) and
/// `MXCSR = 0x1F80` (all exceptions masked, round-to-nearest).
///
/// The current thread's live state is saved and restored around this, because
/// creating a thread can happen inside a syscall of a user thread that may be
/// using the registers.
unsafe fn init_fpu(area: &mut FpuState) {
    let mut saved = AlignedFpu::new();
    let saved_ptr = saved.ptr();
    fxsave(saved_ptr);
    core::arch::asm!("fninit", options(nostack, preserves_flags));
    static MXCSR_DEFAULT: u32 = 0x1F80;
    core::arch::asm!(
        "ldmxcsr [{}]",
        in(reg) core::ptr::addr_of!(MXCSR_DEFAULT),
        options(nostack, preserves_flags)
    );
    fxsave(area.0.as_mut_ptr());
    fxrstor(saved_ptr);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Ready,
    Running,
    Blocked,
    Dying,
}

/// What a thread *is*.  User threads own a process address space that must be
/// active while they run; kernel threads run on the kernel root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadKind {
    Kernel,
    User { pid: u32 },
}

pub struct Thread {
    used: bool,
    is_idle: bool,
    id: u32,
    name: &'static str,
    kind: ThreadKind,
    /// Page-table root to activate while this thread runs.
    root: usize,
    /// FPU/SSE state, swapped by `schedule`.
    fpu: FpuState,
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
            kind: ThreadKind::Kernel,
            root: 0,
            fpu: FpuState::new(),
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
}

impl Scheduler {
    /// `const`-constructed straight into `.bss`.  A `Default` impl would build
    /// a ~40 KiB temporary on the kernel stack (64 threads × FPU state), which
    /// overflows a 16 KiB thread stack — and it is reached from the first
    /// `current_pid()` call, long before `init()` runs.
    pub const fn new() -> Self {
        Scheduler {
            threads: [const { Thread::new() }; MAX_THREADS],
            ready_head: NONE,
            ready_tail: NONE,
            current: MAIN_ID,
            idle: IDLE_ID,
            ticks: 0,
        }
    }

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
        let kind = self.threads[idx].kind;
        // A dead user thread takes its process down with it: free the user
        // frames and the user-half page tables.  This is only safe because the
        // scheduler already switched CR3 to the next thread's root.
        if let ThreadKind::User { pid } = kind {
            debug_assert_ne!(self.threads[self.current].root, self.threads[idx].root);
            crate::proc::table().reap(pid);
        }
        if pages != 0 && !stack.is_null() {
            mm::page_alloc().free_page(stack, pages);
        }
        self.threads[idx] = Thread::new();
    }

    /// Reclaim every thread that died before this tick.
    ///
    /// A single "pending" slot was not enough: two threads can die within one
    /// tick (a process exits while another faults), and the second one would
    /// silently leak its kernel stack and its whole process.  Scanning 64
    /// entries per tick is free.
    fn reap_dying(&mut self) {
        for i in 0..MAX_THREADS {
            if i != self.current
                && self.threads[i].used
                && self.threads[i].state == ThreadState::Dying
            {
                self.reclaim(i);
            }
        }
    }
}

#[repr(transparent)]
struct SchedCell(UnsafeCell<Scheduler>);

unsafe impl Sync for SchedCell {}

static SCHED: SchedCell = SchedCell(UnsafeCell::new(Scheduler::new()));

fn sched() -> &'static mut Scheduler {
    unsafe { &mut *SCHED.0.get() }
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

/// Build the first frame of a **user** thread.  `iretq` lands in ring3 with
/// `rdi = arg` (the `StartupBlock` pointer), `rflags.IF` set so the timer can
/// preempt it, and the user stack top in `rsp`.
unsafe fn build_user_frame(
    kstack_top: usize,
    entry: u64,
    user_stack_top: u64,
    arg: u64,
) -> usize {
    let base = kstack_top - size_of::<TrapFrame>();
    core::ptr::write_bytes(base as *mut u8, 0, size_of::<TrapFrame>());
    let f = &mut *(base as *mut TrapFrame);
    f.rip = entry;
    f.cs = USER_CODE as u64;
    f.rflags = EFLAGS_IF;
    f.rsp = user_stack_top;
    f.ss = USER_DATA as u64;
    f.rdi = arg;
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
    main_t.kind = ThreadKind::Kernel;
    main_t.root = X86_64Paging::active_root();
    main_t.state = ThreadState::Running;
    // The boot thread runs on the trampoline stack; keep it for now.
    main_t.kstack_top = (crate::arch::x86_64::read_rsp() & !0xf) as usize;

    let idle_t = &mut s.threads[IDLE_ID];
    *idle_t = Thread::new();
    idle_t.used = true;
    idle_t.is_idle = true;
    idle_t.id = IDLE_ID as u32;
    idle_t.name = "idle";
    idle_t.kind = ThreadKind::Kernel;
    idle_t.root = X86_64Paging::active_root();
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
    unsafe { init_fpu(&mut idle_t.fpu) };

    s.current = MAIN_ID;
    s.idle = IDLE_ID;

    unsafe { init_fpu(&mut s.threads[MAIN_ID].fpu) };
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
            let kernel_root = s.threads[MAIN_ID].root;
            let t = &mut s.threads[slot];
            *t = Thread::new();
            t.used = true;
            t.id = slot as u32;
            t.name = name;
            t.kind = ThreadKind::Kernel;
            t.root = kernel_root;
            t.state = ThreadState::Ready;
            t.entry = Some(entry);
            t.arg = arg;
            t.stack = stack;
            t.stack_pages = STACK_PAGES;
            t.kstack_top = top;
            t.frame = unsafe { build_initial_frame(top, thread_entry_trampoline as *const () as usize) };
            unsafe { init_fpu(&mut t.fpu) };
            s.enqueue(slot);
            Some(slot as u32)
        })()
    };
    crate::arch::x86_64::sti();
    result
}

/// Create a user thread for `pid`, whose address space is `root`.
///
/// P0 keeps one thread per process: the caller (`proc`) records the tid so the
/// process can be reaped when it dies.
pub fn thread_create_user(
    pid: u32,
    root: usize,
    name: &'static str,
    entry: u64,
    user_stack_top: u64,
    arg: u64,
) -> Option<u32> {
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
            t.kind = ThreadKind::User { pid };
            t.root = root;
            t.state = ThreadState::Ready;
            t.stack = stack;
            t.stack_pages = STACK_PAGES;
            t.kstack_top = top;
            t.frame = unsafe { build_user_frame(top, entry, user_stack_top, arg) };
            unsafe { init_fpu(&mut t.fpu) };
            s.enqueue(slot);
            Some(slot as u32)
        })()
    };
    crate::arch::x86_64::sti();
    result
}

/// The process of the running thread, when it is a user thread.
pub fn current_pid() -> Option<u32> {
    let s = sched();
    match s.threads[s.current].kind {
        ThreadKind::User { pid } => Some(pid),
        ThreadKind::Kernel => None,
    }
}

/// Page-table root of the running thread.
pub fn current_root() -> usize {
    let s = sched();
    s.threads[s.current].root
}

/// Mark the running thread dying and ask the dispatcher for a reschedule.
///
/// Unlike [`thread_exit`] this is callable from *inside* an interrupt handler
/// (a syscall or a fault): it does not raise `int 0x81`, it sets a flag that
/// `isr_dispatch` checks before returning to the frame, so the dying frame is
/// never resumed.
pub fn kill_current() {
    {
        let s = sched();
        s.threads[s.current].state = ThreadState::Dying;
    }
    intr::request_resched();
}

/// Kill the user thread of `pid` from another thread.
///
/// The target may be running, ready or blocked; `schedule` skips Dying threads
/// in the ready queue and the reaper frees its stack and process next tick.
pub fn kill_pid(pid: u32) -> bool {
    crate::arch::x86_64::cli();
    let s = sched();
    for i in 0..MAX_THREADS {
        if !s.threads[i].used {
            continue;
        }
        if s.threads[i].kind == (ThreadKind::User { pid }) {
            s.threads[i].state = ThreadState::Dying;
            if i == s.current {
                intr::request_resched();
            }
            crate::arch::x86_64::sti();
            return true;
        }
    }
    crate::arch::x86_64::sti();
    false
}

/// `fn(frame, vector) -> next_frame` installed as the scheduler hook.
fn sched_entry(frame: usize, vector: usize) -> usize {
    if vector == VECTOR_TIMER {
        let s = sched();
        s.ticks += 1;

        // Reclaim every thread that died before this tick: its stack is free
        // because this tick runs on a different stack.
        s.reap_dying();

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

    // Eager per-thread FPU/SSE state (design §7.3): the kernel is soft-float,
    // so nothing else would preserve it.
    unsafe { fxsave(core::ptr::addr_of_mut!(s.threads[cur].fpu) as *mut u8) };

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

    // A thread can be marked Dying while it sits in the ready queue (another
    // process called sys_kill).  Skip it: it must never run again.
    let next = loop {
        match s.dequeue() {
            Some(h) if s.threads[h].state == ThreadState::Dying => continue,
            Some(h) => break h,
            None => {
                break if cur_is_idle { cur } else { s.idle };
            }
        }
    };

    s.threads[next].state = ThreadState::Running;
    let next_frame = if next == cur {
        cur_frame
    } else {
        s.threads[next].frame
    };

    // Address spaces follow the thread: a user thread runs on its process
    // root, a kernel thread on the kernel root (identical kernel half, so the
    // switch never invalidates kernel mappings).
    let next_root = s.threads[next].root;
    if next_root != 0 && next_root != X86_64Paging::active_root() {
        X86_64Paging::switch_to(next_root);
    }

    unsafe { fxrstor(core::ptr::addr_of!(s.threads[next].fpu) as *const u8) };

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
