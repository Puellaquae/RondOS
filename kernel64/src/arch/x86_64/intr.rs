//! 64-bit IDT, canonical trap frame, exception/IRQ dispatch — M0.3/M0.4.
//!
//! Long mode has no `pusha`, and the CPU pushes a *variable* number of words
//! depending on how it entered the kernel:
//!
//! ```text
//!   ring3 -> ring0 :  [rip][cs][rflags][rsp][ss]     (privilege change)
//!   ring0 -> ring0 :  [rip][cs][rflags]              (no stack switch)
//!   + error code   :  the CPU pushes it just below rip
//! ```
//!
//! Because `iretq` in 64-bit mode **always** pops `rsp`/`ss`, every entry must
//! end up with the same five-word tail.  The per-vector stub therefore tests
//! the saved `CS` and, for ring0 entries, inserts two words after `rflags`
//! (shifting the CPU frame down by 16 bytes and writing the original `rsp`
//! plus the kernel data selector).  After that the frame is always:
//!
//! ```text
//!   low address
//!     r15 r14 r13 r12 r11 r10 r9 r8        <- stub pushes
//!     rdi rsi rbp rdx rcx rbx rax
//!     vector error
//!     rip cs rflags rsp ss
//!   high address
//! ```

use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::gdt::{KERNEL_CODE, KERNEL_DATA};
use super::DescriptorTablePointer;

// --------------------------------------------------------- fault breadcrumb
//
// CMOS NVRAM bytes the UEFI loader reads and prints on the *next* boot (see
// `boot/uefi/src/main.rs::report_previous_stage`).  They are written with port
// I/O only: when the fault is a bad page walk, every memory access — including
// the physmap the normal `boot::stage` path uses — is suspect, and this is the
// one channel that still works while the machine is falling over.

const CMOS_FAULT_VEC: u8 = 0x3b;
const CMOS_FAULT_ERR: u8 = 0x3c;

fn cmos_read(reg: u8) -> u8 {
    let v: u8;
    unsafe {
        asm!("out dx, al", in("dx") 0x70u16, in("al") 0x80 | reg, options(nomem, nostack));
        asm!("in al, dx", in("dx") 0x71u16, out("al") v, options(nomem, nostack));
        asm!("out dx, al", in("dx") 0x70u16, in("al") 0u8, options(nomem, nostack));
    }
    v
}

fn cmos_write(reg: u8, value: u8) {
    unsafe {
        asm!("out dx, al", in("dx") 0x70u16, in("al") 0x80 | reg, options(nomem, nostack));
        asm!("out dx, al", in("dx") 0x71u16, in("al") value, options(nomem, nostack));
        asm!("out dx, al", in("dx") 0x70u16, in("al") 0u8, options(nomem, nostack));
    }
}

/// Record a CPU exception.  The *first* one wins: a handler that faults again
/// (or a #DF after a #PF) must not overwrite the root cause.
fn fault_record(vector: u8, error: u8) {
    if cmos_read(CMOS_FAULT_VEC) != 0 {
        return;
    }
    cmos_write(CMOS_FAULT_VEC, vector);
    cmos_write(CMOS_FAULT_ERR, error);
}

/// `int 0x80` — the stable syscall gate (DPL=3).
pub const VECTOR_SYSCALL: usize = 0x80;
/// `int 0x81` — voluntary reschedule (yield / exit / sleep).
pub const VECTOR_YIELD: usize = 0x81;
pub const VECTOR_TIMER: usize = 0x20;
pub const VECTOR_KEYBOARD: usize = 0x21;

pub const VECTOR_BREAKPOINT: usize = 3;
pub const VECTOR_DOUBLE_FAULT: usize = 8;
pub const VECTOR_GENERAL_PROTECTION: usize = 13;
pub const VECTOR_PAGE_FAULT: usize = 14;

pub const IRQ_BASE: usize = 0x20;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl TrapFrame {
    /// True when the interrupted context was ring3.
    #[inline]
    pub fn from_user(&self) -> bool {
        self.cs & 3 == 3
    }

    /// Return `(status, value)` to the caller (design §6.2).
    #[inline]
    pub fn set_result(&mut self, status: u64, value: u64) {
        self.rax = status;
        self.rdx = value;
    }

    /// Rewrite the frame so `iretq` resumes in ring0 at `rip` on `rsp`.
    ///
    /// **In 64-bit mode `iretq` always pops `RSP` and `SS`**, even when the
    /// return stays at CPL0 (the SDM only makes that conditional for 32-bit
    /// operand size; QEMU's `helper_ret_protected` shows it explicitly as
    /// `(HF_CS64_MASK && !is_iret)`).  So a kernel-return frame must carry a
    /// valid kernel `rsp` and the kernel data selector — leaving the user
    /// values there loads `SS=0x23` at CPL0 and raises `#GP(0x20)`.
    pub fn return_to_kernel(&mut self, rip: u64, rsp: u64, rflags: u64) {
        self.rip = rip;
        self.cs = KERNEL_CODE as u64;
        self.rsp = rsp;
        self.ss = KERNEL_DATA as u64;
        self.rflags = rflags;
    }

    pub fn dump(&self, what: &str) {
        crate::serial_println!(
            "{} vec {} err {:#x} from {} rip {:#x} cs {:#x} rflags {:#x} rsp {:#x} ss {:#x}",
            what,
            self.vector,
            self.error,
            if self.from_user() { "user" } else { "kernel" },
            self.rip,
            self.cs,
            self.rflags,
            self.rsp,
            self.ss
        );
        crate::serial_println!(
            "  rax {:#018x} rbx {:#018x} rcx {:#018x} rdx {:#018x}",
            self.rax,
            self.rbx,
            self.rcx,
            self.rdx
        );
        crate::serial_println!(
            "  rsi {:#018x} rdi {:#018x} rbp {:#018x} rsp {:#018x}",
            self.rsi,
            self.rdi,
            self.rbp,
            self.rsp
        );
        crate::serial_println!(
            "  r8  {:#018x} r9  {:#018x} r10 {:#018x} r11 {:#018x}",
            self.r8,
            self.r9,
            self.r10,
            self.r11
        );
        crate::serial_println!(
            "  r12 {:#018x} r13 {:#018x} r14 {:#018x} r15 {:#018x}",
            self.r12,
            self.r13,
            self.r14,
            self.r15
        );
    }
}

// ------------------------------------------------------------------ IDT

#[repr(C)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_lo: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_hi: u32,
    reserved: u32,
}

impl IdtEntry {
    const fn missing() -> Self {
        Self {
            offset_lo: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_hi: 0,
            reserved: 0,
        }
    }

    fn new(handler: usize, selector: u16, dpl: u8, ist: u8) -> Self {
        Self {
            offset_lo: handler as u16,
            selector,
            ist: ist & 0x7,
            // present, interrupt gate (IF cleared on entry), S=0
            type_attr: 0x8E | ((dpl & 3) << 5),
            offset_mid: (handler >> 16) as u16,
            offset_hi: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

#[repr(C, align(16))]
struct Idt(UnsafeCell<[IdtEntry; 256]>);

unsafe impl Sync for Idt {}

static IDT: Idt = Idt(UnsafeCell::new([IdtEntry::missing(); 256]));

// The `#DF` stack itself lives in `tss::df_stack_top()`; the gate below selects
// IST index 1 (TSS.ist[0]) so a double fault is always reported.

// --------------------------------------------------------------- stubs

// In **64-bit mode** interrupt delivery always pushes
//
//     [err?][rip][cs][rflags][rsp][ss]
//
// regardless of privilege level (unlike 32-bit protected mode, where SS:RSP
// are only pushed on a privilege change).  `iretq` likewise always pops all
// five.  So the frame needs *no* normalization at all: push a fake error code
// for vectors that lack one, push the vector, push the GPRs, done.
//
// Layout produced (low -> high), matching [`TrapFrame`]:
//
//     r15..r8 rdi rsi rbp rdx rcx rbx rax  vector error  rip cs rflags rsp ss
macro_rules! stub {
    ($name:ident, $vec:literal, $has_err:literal) => {
        core::arch::global_asm!(
            concat!(
                ".global ", stringify!($name), "\n",
                ".type ", stringify!($name), ", @function\n",
                stringify!($name), ":\n",
                ".if ", $has_err, " == 0\n",
                "  push 0\n", // fake error code
                ".endif\n",
                "  push ", $vec, "\n",
                "  push rax\n",
                "  push rbx\n",
                "  push rcx\n",
                "  push rdx\n",
                "  push rbp\n",
                "  push rsi\n",
                "  push rdi\n",
                "  push r8\n",
                "  push r9\n",
                "  push r10\n",
                "  push r11\n",
                "  push r12\n",
                "  push r13\n",
                "  push r14\n",
                "  push r15\n",
                "  jmp isr_common\n",
            )
        );
        extern "C" {
            fn $name();
        }
    };
}

stub!(isr_0, 0, 0); // #DE
stub!(isr_1, 1, 0); // #DB
stub!(isr_2, 2, 0); // NMI
stub!(isr_3, 3, 0); // #BP
stub!(isr_4, 4, 0); // #OF
stub!(isr_5, 5, 0); // #BR
stub!(isr_6, 6, 0); // #UD
stub!(isr_7, 7, 0); // #NM
stub!(isr_8, 8, 1); // #DF   (error code)
stub!(isr_9, 9, 0);
stub!(isr_10, 10, 1); // #TS
stub!(isr_11, 11, 1); // #NP
stub!(isr_12, 12, 1); // #SS
stub!(isr_13, 13, 1); // #GP
stub!(isr_14, 14, 1); // #PF
stub!(isr_15, 15, 0);
stub!(isr_16, 16, 0); // #MF
stub!(isr_17, 17, 1); // #AC
stub!(isr_18, 18, 0); // #MC
stub!(isr_19, 19, 0); // #XM
stub!(isr_20, 20, 0); // #VE
stub!(isr_21, 21, 1); // #CP
stub!(isr_22, 22, 0);
stub!(isr_23, 23, 0);
stub!(isr_24, 24, 0);
stub!(isr_25, 25, 0);
stub!(isr_26, 26, 0);
stub!(isr_27, 27, 0);
stub!(isr_28, 28, 0);
stub!(isr_29, 29, 0);
stub!(isr_30, 30, 0);
stub!(isr_31, 31, 0);

// IRQs (PIC remapped to 0x20..0x2F) and software scheduling.
stub!(isr_32, 32, 0); // IRQ0 timer
stub!(isr_33, 33, 0); // IRQ1 keyboard
stub!(isr_34, 34, 0);
stub!(isr_35, 35, 0);
stub!(isr_36, 36, 0);
stub!(isr_37, 37, 0);
stub!(isr_38, 38, 0);
stub!(isr_39, 39, 0);
stub!(isr_40, 40, 0);
stub!(isr_41, 41, 0);
stub!(isr_42, 42, 0);
stub!(isr_43, 43, 0);
stub!(isr_44, 44, 0);
stub!(isr_45, 45, 0);
stub!(isr_46, 46, 0);
stub!(isr_47, 47, 0);

stub!(isr_128, 128, 0); // int 0x80
stub!(isr_129, 129, 0); // int 0x81

core::arch::global_asm!(
    ".global isr_common",
    ".type isr_common, @function",
    "isr_common:",
    // The per-vector stub already saved the GPRs and normalized the frame.
    "mov ax, {kdata}",
    "mov ds, ax",
    "mov es, ax",
    // The CPU loads a *null* SS when entering ring0 from ring3 in long mode.
    "mov ss, ax",
    "mov rdi, rsp",
    "call {dispatch}",
    // isr_dispatch returns the frame to resume (the scheduler may switch it).
    "mov rsp, rax",
    // Returning to ring3 needs DPL3 data selectors in DS/ES: `iretq` restores
    // CS/SS from the frame but *not* DS/ES, and a DPL0 data selector is not
    // usable at CPL3.  CS lives at frame+0x90 (15 GPRs + vector + error + rip).
    // This must run *before* the pops: `mov ax, ...` would otherwise clobber
    // the syscall's return value in RAX.
    "test byte ptr [rsp + 0x90], 3",
    "jz 2f",
    "mov ax, {udata}",
    "mov ds, ax",
    "mov es, ax",
    "2:",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop r11",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rdi",
    "pop rsi",
    "pop rbp",
    "pop rdx",
    "pop rcx",
    "pop rbx",
    "pop rax",
    "add rsp, 16",
    "iretq",
    dispatch = sym isr_dispatch,
    kdata = const KERNEL_DATA,
    udata = const super::gdt::USER_DATA,
);

// ------------------------------------------------------------ dispatch

pub type Handler = fn(&mut TrapFrame);

/// `fn(frame, vector) -> next_frame`; used by the scheduler on the timer/yield
/// path.  The vector lets the scheduler distinguish a timer tick (advance time,
/// wake sleepers) from a voluntary `int 0x81`.
pub type SchedHook = fn(usize, usize) -> usize;

struct HandlerTable(UnsafeCell<[Option<Handler>; 256]>);

unsafe impl Sync for HandlerTable {}

static HANDLERS: HandlerTable = HandlerTable(UnsafeCell::new([None; 256]));

static SCHED_HOOK: AtomicUsize = AtomicUsize::new(0);

pub fn set_handler(vector: usize, handler: Handler) {
    assert!(vector < 256);
    unsafe {
        (*HANDLERS.0.get())[vector] = Some(handler);
    }
}

pub fn set_sched_hook(hook: SchedHook) {
    SCHED_HOOK.store(hook as usize, Ordering::Release);
}

/// Set by a handler that must not resume the interrupted frame (sys_exit, a
/// killed process, a user fault).  `isr_dispatch` consumes it and hands the
/// frame to the scheduler instead of returning it to `isr_common`.
static NEED_RESCHED: AtomicBool = AtomicBool::new(false);

pub fn request_resched() {
    NEED_RESCHED.store(true, Ordering::Release);
}

fn take_resched() -> bool {
    NEED_RESCHED.swap(false, Ordering::AcqRel)
}

fn handler_for(vector: usize) -> Option<Handler> {
    unsafe { (*HANDLERS.0.get())[vector] }
}

fn sched_hook() -> Option<SchedHook> {
    let p = SCHED_HOOK.load(Ordering::Acquire);
    if p == 0 {
        None
    } else {
        Some(unsafe { core::mem::transmute::<usize, SchedHook>(p) })
    }
}

fn stub_addr(f: unsafe extern "C" fn()) -> usize {
    f as usize
}

fn install(
    idt: *mut [IdtEntry; 256],
    vector: usize,
    stub: unsafe extern "C" fn(),
    dpl: u8,
    ist: u8,
) {
    unsafe {
        (*idt)[vector] = IdtEntry::new(stub_addr(stub), KERNEL_CODE, dpl, ist);
    }
}

/// Build and load the IDT.
pub fn init() {
    let idt = IDT.0.get();
    unsafe {
        for e in (*idt).iter_mut() {
            *e = IdtEntry::missing();
        }
    }

    // CPU exceptions.
    install(idt, 0, isr_0, 0, 0);
    install(idt, 1, isr_1, 0, 0);
    install(idt, 2, isr_2, 0, 0);
    install(idt, 3, isr_3, 3, 0); // int3 from ring3
    install(idt, 4, isr_4, 0, 0);
    install(idt, 5, isr_5, 0, 0);
    install(idt, 6, isr_6, 0, 0);
    install(idt, 7, isr_7, 0, 0);
    // #DF runs on its own IST stack (TSS.ist[0]) so a broken kernel stack still
    // reports instead of triple-faulting into a silent reset.
    super::tss::set_ist(0, super::tss::df_stack_top());
    install(idt, 8, isr_8, 0, 1);
    install(idt, 10, isr_10, 0, 0);
    install(idt, 11, isr_11, 0, 0);
    install(idt, 12, isr_12, 0, 0);
    install(idt, 13, isr_13, 0, 0);
    install(idt, 14, isr_14, 0, 0);
    install(idt, 16, isr_16, 0, 0);
    install(idt, 17, isr_17, 0, 0);
    install(idt, 18, isr_18, 0, 0);
    install(idt, 19, isr_19, 0, 0);
    install(idt, 21, isr_21, 0, 0);

    // IRQs.
    let irq = [
        isr_32, isr_33, isr_34, isr_35, isr_36, isr_37, isr_38, isr_39, isr_40, isr_41, isr_42,
        isr_43, isr_44, isr_45, isr_46, isr_47,
    ];
    for (i, s) in irq.iter().enumerate() {
        install(idt, IRQ_BASE + i, *s, 0, 0);
    }

    install(idt, VECTOR_SYSCALL, isr_128, 3, 0);
    install(idt, VECTOR_YIELD, isr_129, 0, 0);

    let dtr = DescriptorTablePointer {
        limit: (256 * 16 - 1) as u16,
        base: idt as u64,
    };
    super::lidt(&dtr);
}

#[no_mangle]
extern "C" fn isr_dispatch(frame: *mut TrapFrame) -> *mut TrapFrame {
    let vector = unsafe { (*frame).vector as usize };

    // CPU exception: breadcrumb it before anything that could depend on the
    // page tables (all of `serial_println!` and `f.dump` below does).
    if vector < 32 {
        fault_record(vector as u8, unsafe { (*frame).error } as u8);
        // A kernel-context exception is fatal (the handlers below halt), so make
        // it visible on a serial-less machine without waiting for a reset.  A
        // user fault is normal control flow (the process is killed) and must not
        // paint over the console.
        if !unsafe { (*frame).from_user() } {
            crate::boot::fault_signal();
        }
    }

    // Acknowledge the PIC before doing anything that may reschedule.
    if (IRQ_BASE..IRQ_BASE + 16).contains(&vector) {
        super::pic::end_of_interrupt((vector - IRQ_BASE) as u8);
    }

    // Timer and voluntary-reschedule vectors go straight to the scheduler.
    if vector == VECTOR_TIMER || vector == VECTOR_YIELD {
        if let Some(hook) = sched_hook() {
            return hook(frame as usize, vector) as *mut TrapFrame;
        }
    }

    match handler_for(vector) {
        Some(h) => h(unsafe { &mut *frame }),
        None => unhandled(unsafe { &mut *frame }),
    }

    // A syscall may have decided that this frame must never be resumed.
    if take_resched() {
        if let Some(hook) = sched_hook() {
            return hook(frame as usize, vector) as *mut TrapFrame;
        }
    }
    frame
}

/// A vector nobody claimed.  Halting here used to take the whole machine down
/// for what is usually a stray firmware/legacy vector, so: log it, mask the
/// corresponding PIC line if it is one, and carry on.  If it keeps firing, the
/// log says which vector and nothing else breaks.
fn unhandled(f: &mut TrapFrame) {
    let vector = f.vector as usize;
    crate::serial_println!(
        "unhandled vector {:#04x} from {} rip {:#x} — ignoring",
        vector,
        if f.from_user() { "user" } else { "kernel" },
        f.rip
    );
    if (IRQ_BASE..IRQ_BASE + 16).contains(&vector) {
        super::pic::mask((vector - IRQ_BASE) as u8);
        crate::serial_println!("intr: masked IRQ{}", vector - IRQ_BASE);
        return;
    }
    // A vector outside the PIC range is not ours to handle; report it once and
    // return, so the interrupted code continues.
}
