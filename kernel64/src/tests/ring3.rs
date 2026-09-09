//! Ring-3 probe programs.
//!
//! These are the only asynchronous tests: `enter_ring3` `iretq`s into user mode
//! and the CPU only comes back through `ring3_return`, which jumps to the
//! continuation installed by `enter_phase`.  The continuation reports the
//! result and hands control back to the harness (`super::boot_ready`).

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::arch::x86_64::paging::{phys_to_virt, virt_to_phys, X86_64Paging};
use crate::arch::x86_64::{gdt, percpu};
use crate::mm;
use crate::mm::vm::{PagingArch, PAGE_USER_RW, PAGE_USER_RX};

use super::{report, boot_ready};
use crate::serial_println;

/// Fake syscall ids the probe blobs use to drive the ring3 round trip.
const RING3_SYSCALL: u64 = 0x1000;
const RING3_EXIT: u64 = 0x1001;
const RING3_GP_OK: u64 = 0x1002;

const STEP_SYSCALL: u64 = 1 << 0;
const STEP_GP: u64 = 1 << 1;
const STEP_EXIT: u64 = 1 << 2;
const STEP_PF: u64 = 1 << 3;

static RING3_STEPS: AtomicU64 = AtomicU64::new(0);
static SAVED_KERNEL_RSP: AtomicU64 = AtomicU64::new(0);
static CONTINUE_FN: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------- probes

// Phase 1: talk to the kernel, trip a privileged instruction, then leave.
core::arch::global_asm!(
    ".global probe_normal",
    "probe_normal:",
    "mov eax, 0x1000", // RING3_SYSCALL
    "int 0x80",
    "cli",             // #GP: no IOPL at CPL3
    "mov eax, 0x1002", // RING3_GP_OK
    "int 0x80",
    "mov eax, 0x1001", // RING3_EXIT
    "int 0x80",
    "ud2",
    ".global probe_normal_end",
    "probe_normal_end:",
);

// Phase 2: touch an unmapped user address -> user #PF -> kernel kills it.
core::arch::global_asm!(
    ".global probe_fault",
    "probe_fault:",
    "mov rax, 0x0000000000001234",
    "mov byte ptr [rax], 0x5a",
    "ud2",
    ".global probe_fault_end",
    "probe_fault_end:",
);

extern "C" {
    fn probe_normal();
    fn probe_normal_end();
    fn probe_fault();
    fn probe_fault_end();
}


/// Map a probe blob + user stack, then `iretq` to ring3.  Never returns.
fn enter_phase(
    code: *const u8,
    len: usize,
    code_va: usize,
    stack_va: usize,
    cont: extern "C" fn() -> !,
) -> ! {
    let root = X86_64Paging::active_root();
    let code_frame = mm::page_alloc().get_page(1).expect("probe code page");
    let stack_frame = mm::page_alloc().get_page(1).expect("probe stack page");
    let kstack_frame = mm::page_alloc().get_page(2).expect("probe kernel stack");

    unsafe { core::ptr::copy_nonoverlapping(code, code_frame, len) };

    X86_64Paging::map(root, code_va, virt_to_phys(code_frame as usize), PAGE_USER_RX)
        .expect("map probe code");
    X86_64Paging::map(root, stack_va, virt_to_phys(stack_frame as usize), PAGE_USER_RW)
        .expect("map probe stack");

    let kstack_top = phys_to_virt(virt_to_phys(kstack_frame as usize)) + 2 * 4096;
    percpu::set_kernel_stack(kstack_top as u64);
    CONTINUE_FN.store(cont as *const () as usize, Ordering::Relaxed);

    serial_println!(
        "ring3: probe {:#x} ({} bytes) stack {:#x} kstack {:#x}",
        code_va,
        len,
        stack_va + 4096,
        kstack_top
    );
    unsafe { enter_ring3(code_va as u64, (stack_va + 4096 - 16) as u64) }
}

#[unsafe(naked)]
unsafe extern "C" fn enter_ring3(entry: u64, user_stack: u64) -> ! {
    core::arch::naked_asm!(
        "mov qword ptr [rip + {saved}], rsp",
        "mov ax, {udata}",
        "mov ds, ax",
        "mov es, ax",
        "push {udata}", // ss
        "push rsi",     // user rsp
        "pushfq",
        "or qword ptr [rsp], 0x200",
        "push {ucode}", // cs
        "push rdi",     // rip
        "iretq",
        saved = sym SAVED_KERNEL_RSP,
        udata = const gdt::USER_DATA,
        ucode = const gdt::USER_CODE,
    )
}

/// Ring-0 continuation: restore the kernel stack, then jump to whatever
/// continuation the current phase installed.
#[unsafe(naked)]
unsafe extern "C" fn ring3_return() -> ! {
    core::arch::naked_asm!(
        "mov rsp, qword ptr [rip + {saved}]",
        "jmp qword ptr [rip + {cont}]",
        saved = sym SAVED_KERNEL_RSP,
        cont = sym CONTINUE_FN,
    )
}


/// Enter ring3 and never return: `after_phase2` resumes the harness.
pub fn start() -> ! {
    enter_phase(
        probe_normal as *const () as *const u8,
        probe_normal_end as *const () as usize - probe_normal as *const () as usize,
        0x0000_5000_0000_0000,
        0x0000_5000_0001_0000,
        after_phase1,
    )
}

/// Probe syscall gate: returns true when the frame was a probe call.
pub fn probe_syscall(f: &mut crate::arch::x86_64::intr::TrapFrame) -> bool {
    match f.rax {
        RING3_SYSCALL => {
            serial_println!(
                "ring3: int 0x80 from cs {:#x} ss {:#x} rsp {:#x} — ok",
                f.cs,
                f.ss,
                f.rsp
            );
            RING3_STEPS.fetch_or(STEP_SYSCALL, Ordering::Relaxed);
            f.set_result(0, 0);
            true
        }
        RING3_GP_OK => {
            serial_println!("ring3: resumed after #GP — ok");
            RING3_STEPS.fetch_or(STEP_GP, Ordering::Relaxed);
            f.set_result(0, 0);
            true
        }
        RING3_EXIT => {
            serial_println!("ring3: exit syscall, returning to ring0");
            RING3_STEPS.fetch_or(STEP_EXIT, Ordering::Relaxed);
            f.return_to_kernel(
                ring3_return as *const () as usize as u64,
                percpu::kernel_stack(),
                0x2,
            );
            true
        }
        _ => false,
    }
}

/// `cli` at CPL3 is the probe's expected #GP; skip the one-byte instruction.
pub fn probe_gp(f: &mut crate::arch::x86_64::intr::TrapFrame) -> bool {
    serial_println!(
        "ring3: #GP at rip {:#x} err {:#x} — expected (cli has no IOPL), skipping 1 byte",
        f.rip,
        f.error
    );
    f.rip += 1;
    true
}

/// The probe's deliberate user #PF: contain it and resume the kernel.
pub fn probe_pf(f: &mut crate::arch::x86_64::intr::TrapFrame, cr2: u64) -> bool {
    serial_println!(
        "ring3: #PF cr2 {:#x} err {:#x} at rip {:#x} — killing user context",
        cr2,
        f.error,
        f.rip
    );
    RING3_STEPS.fetch_or(STEP_PF, Ordering::Relaxed);
    f.return_to_kernel(
        ring3_return as *const () as usize as u64,
        percpu::kernel_stack(),
        0x2,
    );
    true
}

extern "C" fn after_phase1() -> ! {
    let steps = RING3_STEPS.load(Ordering::Relaxed);
    let ok = steps & (STEP_SYSCALL | STEP_GP | STEP_EXIT) == (STEP_SYSCALL | STEP_GP | STEP_EXIT);
    report("ring3-syscall", ok);

    // Phase 2: a user page fault must be contained.
    enter_phase(
        probe_fault as *const () as *const u8,
        probe_fault_end as *const () as usize - probe_fault as *const () as usize,
        0x0000_5000_0010_0000,
        0x0000_5000_0011_0000,
        after_phase2,
    )
}

extern "C" fn after_phase2() -> ! {
    let steps = RING3_STEPS.load(Ordering::Relaxed);
    report("ring3-pagefault", steps & STEP_PF != 0);
    // The probes are done; the harness takes over on the boot thread.
    boot_ready()
}
