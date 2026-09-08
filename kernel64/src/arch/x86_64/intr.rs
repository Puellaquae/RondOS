//! 64-bit IDT, canonical trap frame and exception dispatch — M0.3/M0.4.
//!
//! Long mode has no `pusha`, and a privilege change pushes two extra words
//! (`SS`, `RSP`), so the old i686 44-byte frame is replaced by one explicit
//! layout used by *both* entry paths (ring0 and ring3):
//!
//! ```text
//!   low address
//!     r15 r14 r13 r12 r11 r10 r9 r8        <- stub pushes (last pushed = lowest)
//!     rdi rsi rbp rdx rcx rbx rax
//!     vector error                         <- stub normalizes (fake error = 0)
//!     rip cs rflags                        <- CPU
//!     rsp ss                               <- CPU, only on a privilege change
//!   high address
//! ```
//!
//! The stub pushes a fake `error` for vectors that do not have one, so the
//! layout is identical everywhere and `iretq` at the end works for both
//! directions.

use core::cell::UnsafeCell;

use super::gdt::{KERNEL_CODE, KERNEL_DATA};
use super::DescriptorTablePointer;

/// `int 0x80` — the stable syscall gate (DPL=3).
pub const VECTOR_SYSCALL: usize = 0x80;
pub const VECTOR_GENERAL_PROTECTION: usize = 13;
pub const VECTOR_PAGE_FAULT: usize = 14;
pub const VECTOR_BREAKPOINT: usize = 3;

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

// --------------------------------------------------------------- stubs
//
// One stub per vector: normalize the frame (push a fake error code when the
// CPU did not) and jump to the common path.

macro_rules! stub {
    ($name:ident, $vec:literal, $has_err:literal) => {
        core::arch::global_asm!(
            concat!(
                ".global ", stringify!($name), "\n",
                ".type ", stringify!($name), ", @function\n",
                stringify!($name), ":\n",
                ".if ", $has_err, " == 0\n",
                "push 0\n",
                ".endif\n",
                "push ", $vec, "\n",
                "jmp isr_common\n",
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
stub!(isr_128, 128, 0); // int 0x80

core::arch::global_asm!(
    ".global isr_common",
    ".type isr_common, @function",
    "isr_common:",
    "push rax",
    "push rbx",
    "push rcx",
    "push rdx",
    "push rbp",
    "push rsi",
    "push rdi",
    "push r8",
    "push r9",
    "push r10",
    "push r11",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov ax, {kdata}",
    "mov ds, ax",
    "mov es, ax",
    // The CPU loads a *null* SS when entering ring0 from ring3 in long mode.
    // Without this, `iretq` back to ring0 (same privilege level, so SS is not
    // popped) raises #GP.
    "mov ss, ax",
    "mov rdi, rsp",
    "call {dispatch}",
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
);

// ------------------------------------------------------------ dispatch

pub type Handler = fn(&mut TrapFrame);

struct HandlerTable(UnsafeCell<[Option<Handler>; 256]>);

unsafe impl Sync for HandlerTable {}

static HANDLERS: HandlerTable = HandlerTable(UnsafeCell::new([None; 256]));

pub fn set_handler(vector: usize, handler: Handler) {
    assert!(vector < 256);
    unsafe {
        (*HANDLERS.0.get())[vector] = Some(handler);
    }
}

fn handler_for(vector: usize) -> Option<Handler> {
    unsafe { (*HANDLERS.0.get())[vector] }
}

fn stub_addr(f: unsafe extern "C" fn()) -> usize {
    f as usize
}

fn install(idt: *mut [IdtEntry; 256], vector: usize, stub: unsafe extern "C" fn(), dpl: u8) {
    unsafe {
        (*idt)[vector] = IdtEntry::new(stub_addr(stub), KERNEL_CODE, dpl, 0);
    }
}

/// Build and load the IDT.  Only the vectors we can actually handle get a real
/// stub; everything else stays non-present so a stray interrupt is loud.
pub fn init() {
    let idt = IDT.0.get();
    unsafe {
        for e in (*idt).iter_mut() {
            *e = IdtEntry::missing();
        }
    }

    install(idt, 0, isr_0, 0);
    install(idt, 1, isr_1, 0);
    install(idt, 2, isr_2, 0);
    install(idt, 3, isr_3, 3); // int3 from ring3 must be allowed
    install(idt, 4, isr_4, 0);
    install(idt, 5, isr_5, 0);
    install(idt, 6, isr_6, 0);
    install(idt, 7, isr_7, 0);
    install(idt, 8, isr_8, 0);
    install(idt, 10, isr_10, 0);
    install(idt, 11, isr_11, 0);
    install(idt, 12, isr_12, 0);
    install(idt, 13, isr_13, 0);
    install(idt, 14, isr_14, 0);
    install(idt, 16, isr_16, 0);
    install(idt, 17, isr_17, 0);
    install(idt, 18, isr_18, 0);
    install(idt, 19, isr_19, 0);
    install(idt, 21, isr_21, 0);
    install(idt, VECTOR_SYSCALL, isr_128, 3);

    let dtr = DescriptorTablePointer {
        limit: (256 * 16 - 1) as u16,
        base: idt as u64,
    };
    super::lidt(&dtr);
}

#[no_mangle]
extern "C" fn isr_dispatch(frame: *mut TrapFrame) {
    let f = unsafe { &mut *frame };
    let vector = f.vector as usize;
    match handler_for(vector) {
        Some(h) => h(f),
        None => unhandled(f),
    }
}

fn unhandled(f: &mut TrapFrame) {
    crate::serial_println!(
        "unhandled vector {} from {} at rip {:#x} err {:#x}",
        f.vector,
        if f.from_user() { "user" } else { "kernel" },
        f.rip,
        f.error
    );
    crate::serial_println!("frame: {:#x?}", f);
    loop {
        super::hlt();
    }
}
