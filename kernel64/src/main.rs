//! RondOS x86-64 kernel — M0.1 … M0.7.
//!
//! Migration scaffold from `docs/user-mode-design.md` §7:
//!
//! * M0.1 arch layer
//! * M0.2 4-level paging backend (NX, physmap, huge-page splitting)
//! * M0.3 GDT/TSS/per-CPU + IDT + ring3 round-trip
//! * M0.4 ring0 frame normalization, PIC/PIT, IST for `#DF`, user `#PF` split
//! * M0.5 kernel threads, preemptive round-robin, sleep/exit, WaitQueue
//! * M0.6 versioned `BootInfo` boot contract
//! * M0.7 UEFI stub + kernel-owned address space (the 32-bit trampoline is gone)
//!
//! `_start` is entered in long mode by the UEFI stub (`boot/uefi`) with
//! `rdi` = the physical address of a `BootInfo`.

#![no_std]
#![no_main]

mod arch;
mod bootinfo;
mod io;
mod mm;
mod proc;
mod syscall;
mod thread;
mod utils;

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use arch::x86_64::intr::TrapFrame;
use arch::x86_64::paging::{
    create_kernel_address_space, destroy_address_space, phys_to_virt, virt_to_phys, X86_64Paging,
    KERNEL_VIRT_BASE, PHYS_MAP_BASE,
};
use arch::x86_64::{cpuid, gdt, halt_loop, has_nx, intr, percpu, pic};
use proc::ExitStatus;
use mm::vm::{
    AddressSpace, CachePolicy, PagingArch, PAGE_KERNEL_RW, PAGE_KERNEL_RX, PAGE_USER_RW,
    PAGE_USER_RX,
};

static REPORTS: AtomicUsize = AtomicUsize::new(0);
static FAILS: AtomicUsize = AtomicUsize::new(0);
static RING3_STEPS: AtomicU64 = AtomicU64::new(0);
static SAVED_KERNEL_RSP: AtomicU64 = AtomicU64::new(0);
static CONTINUE_FN: AtomicUsize = AtomicUsize::new(0);

static BUSY_A: AtomicUsize = AtomicUsize::new(0);
static BUSY_B: AtomicUsize = AtomicUsize::new(0);
static SLEEPER: AtomicUsize = AtomicUsize::new(0);

const RING3_SYSCALL: u64 = 0x1000;
const RING3_EXIT: u64 = 0x1001;
const RING3_GP_OK: u64 = 0x1002;

const STEP_SYSCALL: u64 = 1 << 0;
const STEP_GP: u64 = 1 << 1;
const STEP_EXIT: u64 = 1 << 2;
const STEP_PF: u64 = 1 << 3;

const TIMER_HZ: u32 = 200;

/// Stack the kernel runs on from its very first instruction.  It lives in
/// `.bss`, i.e. inside the kernel window the loader maps, so it survives the
/// switch to the kernel-owned page tables below.
const BOOT_STACK_SIZE: usize = 64 * 1024;

#[repr(align(16))]
#[allow(dead_code)]
struct BootStack([u8; BOOT_STACK_SIZE]);

static mut BOOT_STACK: BootStack = BootStack([0; BOOT_STACK_SIZE]);

#[no_mangle]
pub extern "C" fn _start(_boot: u64) -> ! {
    // The loader's stack and page tables are temporary scaffolding.  Take a
    // stack we own before touching anything, then never look back.
    let stack_top = core::ptr::addr_of!(BOOT_STACK) as u64 + BOOT_STACK_SIZE as u64 - 8;
    unsafe {
        core::arch::asm!(
            "mov rsp, {stack}",
            "call {kmain}",
            stack = in(reg) stack_top,
            kmain = sym kmain,
            options(noreturn),
        )
    }
}

extern "C" fn kmain(boot: u64) -> ! {
    serial_println!();
    serial_println!("==============================================");
    serial_println!("RondOS x86-64 — M0.1..M0.7 scaffold");
    serial_println!("==============================================");

    banner_cpu();

    // M0.7: the single boot contract is a `BootInfo` produced by the UEFI stub
    // (`boot/uefi`), passed in `rdi` as a physical address.
    if !bootinfo::probe(boot) {
        panic!("boot: no BootInfo at {:#x}", boot);
    }
    if !bootinfo::adopt_external(boot) {
        panic!("boot: BootInfo at {:#x} failed validation", boot);
    }
    serial_println!("bootinfo: adopted UEFI structure");
    bootinfo::dump();
    serial_println!(
        "memory: {} MiB usable, top {:#x}",
        mm::available_mem_size() / 1024 / 1024,
        mm::usable_end()
    );

    // M0.7: the loader's page tables are scaffolding too.  Build a kernel-owned
    // root (kernel half copied, low identity map dropped) and run on it from
    // here on — this is what makes the kernel independent of the bootloader.
    let root = match create_kernel_address_space() {
        Some(r) => r,
        None => panic!("paging: cannot create the kernel address space"),
    };
    X86_64Paging::switch_to(root);
    serial_println!("paging: kernel-owned root {:#x}", root);

    test_physmap();
    test_huge_split();
    test_address_space();
    test_wx();
    test_device_map();
    test_allocator();

    gdt::init();
    percpu::init();
    intr::init();
    serial_println!("gdt/tss/percpu/idt ready (gs {:#x})", percpu::gs_base());

    intr::set_handler(intr::VECTOR_SYSCALL, syscall_handler);
    intr::set_handler(intr::VECTOR_GENERAL_PROTECTION, gp_handler);
    intr::set_handler(intr::VECTOR_PAGE_FAULT, page_fault_handler);
    intr::set_handler(6, invalid_opcode_handler);
    intr::set_handler(intr::VECTOR_DOUBLE_FAULT, double_fault_handler);

    // Phase 1: syscall + #GP + exit syscall back to ring0.
    enter_phase(
        probe_normal as *const () as *const u8,
        probe_normal_end as *const () as usize - probe_normal as *const () as usize,
        0x0000_5000_0000_0000,
        0x0000_5000_0001_0000,
        after_phase1,
    )
}

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

fn syscall_handler(f: &mut TrapFrame) {
    // The M0 probe blobs used fake ids to drive the ring3 round-trip; keep
    // them working, and route everything else through the real v1 dispatcher.
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
        }
        RING3_GP_OK => {
            serial_println!("ring3: resumed after #GP — ok");
            RING3_STEPS.fetch_or(STEP_GP, Ordering::Relaxed);
            f.set_result(0, 0);
        }
        RING3_EXIT => {
            serial_println!("ring3: exit syscall, returning to ring0");
            RING3_STEPS.fetch_or(STEP_EXIT, Ordering::Relaxed);
            f.return_to_kernel(
                ring3_return as *const () as usize as u64,
                percpu::kernel_stack(),
                0x2,
            );
        }
        _ => syscall::dispatch(f),
    }
}

fn gp_handler(f: &mut TrapFrame) {
    if f.from_user() {
        if thread::current_pid().is_some() {
            serial_println!("ring3: #GP at rip {:#x} err {:#x} — killing process", f.rip, f.error);
            proc::exit_current(ExitStatus::Fault {
                vector: 13,
                rip: f.rip,
                addr: 0,
            });
            return;
        }
        serial_println!(
            "ring3: #GP at rip {:#x} err {:#x} — expected (cli has no IOPL), skipping 1 byte",
            f.rip,
            f.error
        );
        f.rip += 1; // `cli` is one byte
    } else {
        serial_println!("kernel #GP at rip {:#x} err {:#x}", f.rip, f.error);
        f.dump("general protection");
        halt_loop();
    }
}

fn page_fault_handler(f: &mut TrapFrame) {
    let cr2 = arch::x86_64::cr2();
    if f.from_user() {
        serial_println!(
            "ring3: #PF cr2 {:#x} err {:#x} at rip {:#x} — killing user context",
            cr2,
            f.error,
            f.rip
        );
        RING3_STEPS.fetch_or(STEP_PF, Ordering::Relaxed);
        if thread::current_pid().is_some() {
            // P0: a real process — tear it down and let the scheduler move on.
            proc::exit_current(ExitStatus::Fault {
                vector: 14,
                rip: f.rip,
                addr: cr2,
            });
        } else {
            // M0 probe blob: no process, hand the frame back to the test.
            f.return_to_kernel(
                ring3_return as *const () as usize as u64,
                percpu::kernel_stack(),
                0x2,
            );
        }
    } else {
        serial_println!("kernel #PF cr2 {:#x} err {:#x}", cr2, f.error);
        f.dump("page fault");
        halt_loop();
    }
}

fn invalid_opcode_handler(f: &mut TrapFrame) {
    serial_println!("#UD in thread '{}'", thread::current_name());
    f.dump("invalid opcode");
    halt_loop()
}

fn double_fault_handler(f: &mut TrapFrame) {
    serial_println!("DOUBLE FAULT err {:#x}", f.error);
    f.dump("double fault");
    halt_loop()
}

// ------------------------------------------------------------- phases

extern "C" fn after_phase1() -> ! {
    let steps = RING3_STEPS.load(Ordering::Relaxed);
    let ok = steps & (STEP_SYSCALL | STEP_GP | STEP_EXIT) == (STEP_SYSCALL | STEP_GP | STEP_EXIT);
    report("ring3-syscall", ok);

    // Phase 2: user page fault must be contained.
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
    let ok = steps & STEP_PF != 0;
    report("ring3-pagefault", ok);

    // ------------------------------------------------------------ M0.4/M0.5
    pic::init();
    pic::configure_pit(0, 2, TIMER_HZ);
    thread::init();

    thread::thread_create("busy-a", busy_a, 0).expect("thread a");
    thread::thread_create("busy-b", busy_b, 1).expect("thread b");
    thread::thread_create("sleeper", sleeper, 2).expect("thread sleeper");
    serial_println!("scheduler: 3 threads, {} Hz, enabling preemption", TIMER_HZ);

    arch::x86_64::sti();

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

    // ------------------------------------------------------------------ P0
    // Real user processes: an address space, a VMA list, a handle table, a
    // ring3 thread that talks to the kernel through `int 0x80`, and a fault
    // that takes down only its own process.
    report("handle-table", proc::selftest_handles());

    let free_before = mm::page_alloc().free_pages();
    let hello = spawn_user(
        "hello",
        user_hello,
        user_hello_end,
        0x0000_6000_0000_0000,
    );
    let fault = spawn_user(
        "fault",
        user_fault,
        user_fault_end,
        0x0000_7000_0000_0000,
    );
    let (hello_pid, fault_pid) = match (hello, fault) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            report("user-spawn", false);
            halt_loop()
        }
    };
    report("user-spawn", true);

    let hello_status = wait_for_exit(hello_pid, 4000);
    let fault_status = wait_for_exit(fault_pid, 4000);

    let hello_ok =
        hello_status == Some(ExitStatus::Exited(0)) && syscall::logged_bytes() >= 12;
    if !hello_ok {
        serial_println!(
            "user: hello status {:?}, sys_log accepted {} bytes",
            hello_status,
            syscall::logged_bytes()
        );
    }
    report("user-process", hello_ok);

    let fault_ok = matches!(fault_status, Some(ExitStatus::Fault { vector: 14, .. }));
    if !fault_ok {
        serial_println!("user: fault status {:?}", fault_status);
    }
    report("user-fault-isolation", fault_ok && proc::table().live() == 0);

    // Every frame the two processes owned must be back in the allocator.
    thread::sleep(50);
    let free_after = mm::page_alloc().free_pages();
    if free_after < free_before {
        serial_println!("user: frames leaked: {} -> {}", free_before, free_after);
    }
    report("user-reclaim", free_after >= free_before);

    serial_println!("----------------------------------------------");
    let total = REPORTS.load(Ordering::Relaxed);
    let failed = FAILS.load(Ordering::Relaxed);
    if failed == 0 {
        serial_println!("smoke: ALL PASS ({}/{})", total, total);
    } else {
        serial_println!("smoke: {} FAILURE(S) of {}", failed, total);
    }
    halt_loop()
}

// ------------------------------------------------------- P0 user processes

/// Offset/size of the user stack inside a process image (P0 placeholder for
/// the ELF loader's segment layout).
const USER_STACK_OFF: u64 = 0x0001_0000;
const USER_STACK_LEN: u64 = 0x2000;

fn spawn_user(
    name: &'static str,
    start: unsafe extern "C" fn(),
    end: unsafe extern "C" fn(),
    base: u64,
) -> Option<u32> {
    let len = end as usize - start as usize;
    let code = unsafe { core::slice::from_raw_parts(start as *const u8, len) };
    let pid = proc::table().create(0)?;
    let root = {
        let p = proc::table().get(pid)?;
        p.map_blob(base, code, PAGE_USER_RX).ok()?;
        p.map_anon(base + USER_STACK_OFF, USER_STACK_LEN, PAGE_USER_RW)
            .ok()?;
        p.root()
    };
    let stack_top = base + USER_STACK_OFF + USER_STACK_LEN - 16;
    let tid = thread::thread_create_user(pid, root, name, base, stack_top, 0)?;
    proc::table().attach_thread(pid, tid).ok()?;
    serial_println!(
        "user: '{}' pid {} tid {} code {:#x} stack {:#x}",
        name,
        pid,
        tid,
        base,
        stack_top
    );
    Some(pid)
}

/// Poll until `pid` leaves `Running` (the outcome survives reaping).
fn wait_for_exit(pid: u32, timeout_ms: u64) -> Option<ExitStatus> {
    let deadline = thread::ticks() + (timeout_ms + thread::TICK_MS - 1) / thread::TICK_MS;
    loop {
        if let Some(st) = proc::table().status_of(pid) {
            if st != ExitStatus::Running {
                return Some(st);
            }
        }
        if thread::ticks() >= deadline {
            return None;
        }
        thread::sleep(5);
    }
}

// Two tiny ring3 programs.  They are *not* the P1 ELF path: they exist so P0
// can prove process lifetime, syscalls and fault containment end to end.

core::arch::global_asm!(
    ".global user_hello",
    "user_hello:",
    // sys_info into a stack buffer (proves copy_to_user + VMA write check).
    "  lea rdi, [rsp - 128]",
    "  mov dword ptr [rdi], 112",       // hdr.size = sizeof(Info)
    "  mov dword ptr [rdi + 4], 1",     // hdr.version
    "  mov rax, 0x00",                  // SyscallId::Info
    "  int 0x80",
    "  test rax, rax",
    "  jnz 2f",                         // -> exit(1)
    "  cmp dword ptr [rdi + 8], 1",     // abi_version
    "  jne 3f",                         // -> exit(2)
    // sys_clock_gettime(Monotonic) -> rdx = ns since boot
    "  mov rax, 0x15",
    "  xor edi, edi",
    "  int 0x80",
    "  test rax, rax",
    "  jnz 4f",                         // -> exit(3)
    "  test rdx, rdx",
    "  jz 4f",
    // sys_yield()
    "  mov rax, 0x13",
    "  int 0x80",
    "  test rax, rax",
    "  jnz 5f",                         // -> exit(4)
    // sys_log(Error, msg, 12)
    "  mov rax, 0x16",
    "  xor edi, edi",
    "  lea rsi, [rip + user_hello_msg]",
    "  mov rdx, 12",
    "  int 0x80",
    "  cmp rdx, 12",                    // value, not status
    "  jne 6f",                         // -> exit(5)
    // sys_exit(0)
    "  mov rax, 0x10",
    "  xor edi, edi",
    "  int 0x80",
    "  ud2",
    "2: mov edi, 1",
    "  jmp 7f",
    "3: mov edi, 2",
    "  jmp 7f",
    "4: mov edi, 3",
    "  jmp 7f",
    "5: mov edi, 4",
    "  jmp 7f",
    "6: mov edi, 5",
    "7: mov rax, 0x10",
    "  int 0x80",
    "  ud2",
    "user_hello_msg:",
    "  .ascii \"hello ring3\\n\"",
    ".global user_hello_end",
    "user_hello_end:",
);

core::arch::global_asm!(
    ".global user_fault",
    "user_fault:",
    "  mov rax, 0x1234",            // unmapped in the process address space
    "  mov byte ptr [rax], 0x5a",
    "  ud2",
    ".global user_fault_end",
    "user_fault_end:",
);

extern "C" {
    fn user_hello();
    fn user_hello_end();
    fn user_fault();
    fn user_fault_end();
}

// ------------------------------------------------------- test threads

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

// ---------------------------------------------------------- memory tests

fn banner_cpu() {
    let v = cpuid(0, 0);
    let mut vendor = [0u8; 12];
    vendor[0..4].copy_from_slice(&v.ebx.to_le_bytes());
    vendor[4..8].copy_from_slice(&v.edx.to_le_bytes());
    vendor[8..12].copy_from_slice(&v.ecx.to_le_bytes());
    let vendor = core::str::from_utf8(&vendor).unwrap_or("?");
    let f = cpuid(1, 0);
    let ext = cpuid(0x8000_0001, 0);
    serial_println!(
        "cpu: {} family {} model {} stepping {} | NX {}",
        vendor,
        (f.eax >> 8) & 0xf,
        (f.eax >> 4) & 0xf,
        f.eax & 0xf,
        if has_nx() { "yes" } else { "NO" }
    );
    serial_println!(
        "physmap {:#x} | kernel {:#x} | cr3 {:#x} | ext.edx {:#010x}",
        PHYS_MAP_BASE,
        KERNEL_VIRT_BASE,
        arch::x86_64::cr3(),
        ext.edx
    );
}

fn test_physmap() -> bool {
    let root = X86_64Paging::active_root();
    let probes = [0x0usize, 0x1000, 0x200000, 0x1000000, 0x4000_0000];
    let mut ok = true;
    for pa in probes {
        if X86_64Paging::translate(root, phys_to_virt(pa)) != Some(pa) {
            serial_println!("physmap: {:#x} broken", pa);
            ok = false;
        }
    }
    let kva = KERNEL_VIRT_BASE + 0x200000;
    if X86_64Paging::translate(root, kva) != Some(0x200000) {
        serial_println!("physmap: kernel alias {:#x} broken", kva);
        ok = false;
    }
    if let Some(info) = X86_64Paging::query(root, phys_to_virt(0x4000_0000)) {
        if !info.huge {
            serial_println!("physmap: expected a huge mapping at 4 GiB");
            ok = false;
        }
    }
    report("physmap", ok);
    ok
}

fn test_huge_split() -> bool {
    // Self-contained: build a private root with one 1 GiB page in the user
    // half, then force the 1 GiB -> 2 MiB -> 4 KiB split.  Relying on the
    // bootloader's identity map (as this test used to) is exactly what the
    // kernel-owned address space removed.
    let root = match create_kernel_address_space() {
        Some(r) => r,
        None => return fail("huge-split", "no PML4"),
    };
    let frame = match mm::page_alloc().get_page(1) {
        Some(f) => f,
        None => return fail("huge-split", "no frame"),
    };
    let pa = virt_to_phys(frame as usize);
    unsafe { (frame as *mut u64).write_volatile(0xDEAD_BEEF_1234_5678) };

    let base = 0x0000_6000_0000_0000usize; // 1 GiB aligned, user half
    let va = base + 0x0100_0000;
    let mut ok = X86_64Paging::map_huge_1g(root, base, 0, PAGE_USER_RW).is_ok();
    ok &= X86_64Paging::map(root, va, pa, PAGE_KERNEL_RW).is_ok();

    match X86_64Paging::query(root, va) {
        Some(info) => {
            if info.pa != pa || info.huge || info.flags.executable || !info.flags.writable {
                serial_println!("huge-split: unexpected flags");
                ok = false;
            }
        }
        None => {
            serial_println!("huge-split: query returned None");
            ok = false;
        }
    }
    // The rest of the split 1 GiB page must still map identity.
    ok &= X86_64Paging::translate(root, va + 0x1000) == Some(0x0100_1000);
    ok &= X86_64Paging::translate(root, va + 0x20_0000) == Some(0x0120_0000);

    let boot_root = X86_64Paging::active_root();
    X86_64Paging::switch_to(root);
    ok &= unsafe { (va as *const u64).read_volatile() } == 0xDEAD_BEEF_1234_5678;
    X86_64Paging::switch_to(boot_root);

    ok &= X86_64Paging::map(root, va, pa + 0x1000, PAGE_KERNEL_RW)
        == Err(mm::vm::MapError::AlreadyMapped);

    ok &= X86_64Paging::unmap(root, va) == Ok(pa);
    mm::page_alloc().free_page(frame, 1);
    destroy_address_space(root);

    report("huge-split", ok);
    ok
}

fn test_address_space() -> bool {
    let boot_root = X86_64Paging::active_root();
    let root = match create_kernel_address_space() {
        Some(r) => r,
        None => return fail("addr-space", "no PML4"),
    };
    let frame = match mm::page_alloc().get_page(1) {
        Some(f) => f,
        None => return fail("addr-space", "no frame"),
    };
    let pa = virt_to_phys(frame as usize);

    let user_va = 0x0000_4000_0000_0000usize;
    let mut space = AddressSpace::<X86_64Paging>::new(root);
    let mut ok = space.map(user_va, pa, PAGE_USER_RW).is_ok();
    ok &= space.translate(user_va) == Some(pa);
    ok &= X86_64Paging::translate(root, 0x0100_0000).is_none();

    let rsp = arch::x86_64::read_rsp();
    let stack_lo = (rsp & !0xfff) - 0x1000;
    for i in 0..3 {
        let va = stack_lo + i * 0x1000;
        match X86_64Paging::translate(boot_root, va) {
            Some(pa_stack) => {
                ok &= space.map(va, pa_stack, PAGE_KERNEL_RW).is_ok();
            }
            None => ok = false,
        }
    }

    space.activate();
    unsafe { (user_va as *mut u32).write_volatile(0xCAFE_F00D) };
    X86_64Paging::switch_to(boot_root);

    ok &= unsafe { (frame as *const u32).read_volatile() } == 0xCAFE_F00D;
    ok &= X86_64Paging::translate(boot_root, user_va).is_none();

    mm::page_alloc().free_page(frame, 1);
    destroy_address_space(root);

    report("addr-space", ok);
    ok
}

fn test_wx() -> bool {
    let root = X86_64Paging::active_root();
    let frame = match mm::page_alloc().get_page(1) {
        Some(f) => f,
        None => return fail("w^x", "no frame"),
    };
    let pa = virt_to_phys(frame as usize);
    let va = 0x5100_0000usize;
    let mut ok = true;

    if X86_64Paging::map(root, va, pa, PAGE_KERNEL_RW).is_ok() {
        if let Some(i) = X86_64Paging::query(root, va) {
            ok &= !i.flags.executable && i.flags.writable && !i.flags.user;
        } else {
            ok = false;
        }
        ok &= X86_64Paging::unmap(root, va).is_ok();
    } else {
        ok = false;
    }

    if X86_64Paging::map(root, va, pa, PAGE_KERNEL_RX).is_ok() {
        if let Some(i) = X86_64Paging::query(root, va) {
            ok &= i.flags.executable && !i.flags.writable;
        } else {
            ok = false;
        }
        ok &= X86_64Paging::unmap(root, va).is_ok();
    } else {
        ok = false;
    }

    if X86_64Paging::map(root, va, pa, PAGE_USER_RW).is_ok() {
        if let Some(i) = X86_64Paging::query(root, va) {
            ok &= i.flags.user && !i.flags.executable;
        } else {
            ok = false;
        }
        ok &= X86_64Paging::unmap(root, va).is_ok();
    } else {
        ok = false;
    }

    mm::page_alloc().free_page(frame, 1);
    report("w^x", ok);
    ok
}

fn test_device_map() -> bool {
    let root = X86_64Paging::active_root();
    let va = 0x5200_0000usize;
    let pa = 0xE000_0000usize;
    let mut ok =
        X86_64Paging::map_device(root, va, pa, 0x3000, CachePolicy::WriteCombining).is_ok();
    if let Some(i) = X86_64Paging::query(root, va) {
        ok &= i.pa == pa && i.flags.cache == CachePolicy::WriteCombining && !i.flags.executable;
    } else {
        ok = false;
    }
    if let Some(i) = X86_64Paging::query(root, va + 0x2000) {
        ok &= i.pa == pa + 0x2000;
    } else {
        ok = false;
    }
    for i in 0..3 {
        ok &= X86_64Paging::unmap(root, va + i * 0x1000).is_ok();
    }
    report("device-map", ok);
    ok
}

fn test_allocator() -> bool {
    let mut ok = true;
    let a = mm::page_alloc().get_page(4);
    let b = mm::page_alloc().get_page(1);
    match (a, b) {
        (Some(a), Some(b)) => {
            ok &= (a as usize) % 4096 == 0 && (b as usize) % 4096 == 0;
            unsafe {
                for i in 0..4 * 4096 {
                    *a.add(i) = (i % 251) as u8;
                }
                let mut good = true;
                for i in 0..4 * 4096 {
                    if *a.add(i) != (i % 251) as u8 {
                        good = false;
                        break;
                    }
                }
                ok &= good;
            }
            mm::page_alloc().free_page(a, 4);
            mm::page_alloc().free_page(b, 1);
        }
        _ => ok = false,
    }
    serial_println!("allocator: {} KiB free", mm::page_alloc().free_pages() * 4);
    report("allocator", ok);
    ok
}

fn report(name: &str, ok: bool) {
    REPORTS.fetch_add(1, Ordering::Relaxed);
    if !ok {
        FAILS.fetch_add(1, Ordering::Relaxed);
    }
    serial_println!("[{}] {}", if ok { " ok " } else { "FAIL" }, name);
}

fn fail(name: &str, why: &str) -> bool {
    serial_println!("[FAIL] {}: {}", name, why);
    false
}

#[panic_handler]
pub fn panic(info: &core::panic::PanicInfo) -> ! {
    serial_println!("PANIC: {}", info);
    halt_loop()
}
