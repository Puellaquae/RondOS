//! RondOS x86-64 kernel — boot and trap entry.
//!
//! The boot sequence lives here and nowhere else:
//!
//! 1. turn on SSE, print the CPU banner;
//! 2. adopt the `BootInfo` the UEFI stub handed over (`boot/uefi`);
//! 3. switch to a kernel-owned address space (the loader's tables are scaffolding);
//! 4. build GDT/TSS/percpu/IDT and register the trap handlers;
//! 5. hand over to the test harness ([`tests`]), which runs the kernel-mode
//!    test programs and ends with the summary.
//!
//! `_start` is entered in long mode with `rdi` = the physical address of a
//! `BootInfo`.

#![no_std]
#![no_main]

mod acpi;
mod arch;
mod boot;
mod bootinfo;
mod bootscreen;
mod exec;
mod fs;
mod io;
mod mm;
mod obj;
mod proc;
mod syscall;
mod tests;
mod thread;
mod utils;

use arch::x86_64::intr::TrapFrame;
use arch::x86_64::paging::{
    create_kernel_address_space, X86_64Paging, KERNEL_VIRT_BASE, PHYS_MAP_BASE,
};
use arch::x86_64::{cpuid, gdt, halt_loop, has_nx, intr, percpu};
use mm::vm::PagingArch;
use proc::ExitStatus;

/// Stack the kernel runs on from its very first instruction.  It lives in
/// `.bss`, i.e. inside the kernel window the loader maps, so it survives the
/// switch to the kernel-owned page tables below.
const BOOT_STACK_SIZE: usize = 64 * 1024;

#[repr(align(16))]
#[allow(dead_code)]
struct BootStack([u8; BOOT_STACK_SIZE]);

static mut BOOT_STACK: BootStack = BootStack([0; BOOT_STACK_SIZE]);

/// Breadcrumb written to CMOS 0x3A at each early step.  The loader reads the
/// last value that survived and prints it, so a machine that dies before it can
/// paint anything still says exactly how far it got.
///
/// The register index must be `0x3a` (`0x80 | 0x3a` = `0xba` on port 0x70).
/// This used to write `0x8a`, i.e. RTC register **0x0A** — the loader's read of
/// 0x3A therefore always came back 0, and the write disturbed the RTC divider.
fn raw_breadcrumb(value: u8) {
    crate::arch::x86_64::outb(0x70, 0xba);
    crate::arch::x86_64::outb(0x71, value);
    crate::arch::x86_64::outb(0x70, 0x00);
}

#[no_mangle]
pub extern "C" fn _start(_boot: u64) -> ! {
    // Split deliberately: the proof-of-life paint runs on the loader's stack
    // (pushing rbx) so it needs no kernel stack, and every register it touches
    // is declared as a clobber, so the compiler keeps the hand-off pointer in
    // `rdi` intact for `kmain`.
    //
    // Red on screen means the CPU executed the kernel.  No red means it did
    // not -- and that answer depends on nothing that can lie.
    unsafe {
        core::arch::asm!(
            "cli",                        // own the interrupt state from instruction 0
            "push rbx",                   // spare rbx while painting
            "mov r10, {mb}",
            "mov rbx, [r10 + 8]",         // fb phys
            "mov r8, [r10 + 16]",         // width
            "mov r9, [r10 + 24]",         // height
            "mov r11, [r10 + 32]",        // pitch
            "test rbx, rbx",
            "jz 9f",
            "cmp r8, 4096",
            "ja 9f",                      // implausible width: do not touch memory
            "mov eax, 0x000000ff",        // pure red (BGRx word order)
            "xor rcx, rcx",               // y
            "2:",
            "cmp rcx, r9",
            "jae 9f",
            "mov r10, rcx",
            "imul r10, r11",
            "add r10, rbx",
            "xor rdx, rdx",               // x
            "3:",
            "cmp rdx, r8",
            "jae 4f",
            "mov [r10 + rdx*4], eax",
            "inc rdx",
            "jmp 3b",
            "4:",
            "inc rcx",
            "jmp 2b",
            "9:",
            "pop rbx",
            mb = const 0x6000u64,
            out("rax") _, out("rcx") _, out("rdx") _,
            out("r8") _, out("r9") _, out("r10") _, out("r11") _,
            options(nostack),
        );
    }

    // Then the normal entry: stack + kmain, with the arithmetic kept out of
    // Rust so no runtime overflow check can be emitted here.
    crate::arch::x86_64::outb(0x60, 0xed);
    crate::arch::x86_64::outb(0x60, 0x07);
    raw_breadcrumb(0xe1);
    unsafe {
        core::arch::asm!(
            "call {addr}",
            "add rax, {size}",
            // The SysV AMD64 ABI wants `%rsp % 16 == 8` at a function's first
            // instruction, i.e. `%rsp % 16 == 0` immediately before `call`.
            // `BOOT_STACK` is 16-aligned and 64 KiB long, so its top is already
            // 16-aligned; the `call` below then lands `kmain` on a correctly
            // aligned frame.  (Subtracting 8 here would skip a 16-byte slot per
            // frame and break any aligned SSE spill in `kmain`.)
            "mov rsp, rax",
            "call {kmain}",
            "ud2",
            size = const BOOT_STACK_SIZE,
            addr = sym boot_stack_addr,
            kmain = sym kmain,
            in("rdi") _boot,
            options(noreturn),
        )
    }
}

/// Returns the address of the boot stack without any arithmetic the compiler
/// could turn into a checked operation.  The static is referenced through a
/// real (black-boxed) read so it cannot be optimised away.
fn boot_stack_addr() -> usize {
    core::hint::black_box(core::ptr::addr_of!(BOOT_STACK) as usize)
}

extern "C" fn kmain(boot: u64) -> ! {
    raw_breadcrumb(0xe3); // the call into kmain returned
    // Read the marker *before* overwriting it with this boot's first stage.
    let previous = boot::previous_stage();
    // Fresh stage mask for this boot, before the first bit is set.
    boot::cmos_begin_run();
    boot::stage(boot::STAGE_ENTERED);
    serial_println!();
    serial_println!("==============================================");
    serial_println!("RondOS x86-64 — M0.1..M0.7 scaffold");
    serial_println!("==============================================");
    if previous != 0 {
        serial_println!(
            "boot: previous boot reached stage {:#04x} (0xff = clean shutdown)",
            previous
        );
    }

    // M0.3 (finished in P2): user programs are built for SSE2, so the kernel
    // must turn the FPU/SSE on and save it per thread.
    arch::x86_64::enable_sse();
    banner_cpu();

    // M0.7: the single boot contract is a `BootInfo` produced by the UEFI stub
    // (`boot/uefi`), passed in `rdi` as a physical address.
    if !bootinfo::probe(boot) {
        raw_breadcrumb(0xef);
        panic!("boot: no BootInfo at {:#x}", boot);
    }
    raw_breadcrumb(0xe4); // BootInfo magic found
    if !bootinfo::adopt_external(boot) {
        raw_breadcrumb(0xee);
        panic!("boot: BootInfo at {:#x} failed validation", boot);
    }
    raw_breadcrumb(0xe5); // BootInfo adopted
    boot::stage(boot::STAGE_BOOTINFO);
    serial_println!("bootinfo: adopted UEFI structure");
    // Paint the progress strip as soon as the framebuffer parameters are known
    // (before the text console exists, so nothing can clear it away).
    bootscreen::init();
    bootinfo::dump();
    serial_println!(
        "memory: {} MiB usable, top {:#x}",
        mm::available_mem_size() / 1024 / 1024,
        mm::usable_end()
    );

    // Identity first: is this the image the loader verified?
    boot::verify_build_tag();
    // Resolve the self-evidence pause before the first paint.
    boot::init_delay();

    // --- traps BEFORE paging ------------------------------------------------
    // Install the kernel's own GDT/TSS/IDT *before* the first CR3 switch.  The
    // IDT and the handlers live in the kernel window, which the loader maps and
    // the kernel-owned root copies, so they stay reachable across the switch.
    // A fault in that switch, or in a page walk the loader's tables get wrong,
    // is now a catchable trap with a report instead of a triple fault and a
    // silent reset — `isr_dispatch` writes the vector to CMOS before it touches
    // any memory a broken physmap could break.
    gdt::init();
    percpu::init();
    intr::init();
    boot::stage(boot::STAGE_TRAPS);
    serial_println!("gdt/tss/percpu/idt ready (gs {:#x})", percpu::gs_base());
    intr::set_handler(intr::VECTOR_SYSCALL, syscall_handler);
    intr::set_handler(intr::VECTOR_GENERAL_PROTECTION, gp_handler);
    intr::set_handler(intr::VECTOR_PAGE_FAULT, page_fault_handler);
    intr::set_handler(6, invalid_opcode_handler);
    intr::set_handler(intr::VECTOR_DOUBLE_FAULT, double_fault_handler);
    // P3: PS/2 keyboard (IRQ1 is unmasked later, by `pic::init`).
    intr::set_handler(intr::VECTOR_KEYBOARD, keyboard_handler);

    // --- self-evidence: are we running at all, and with which mappings? -----
    // The very first thing after `BootInfo` is on screen, so a photo settles
    // "did the kernel start" without depending on logs, CMOS or the stick.
    // (Uses the loader's identity map; the framebuffer physical address is
    // reachable as-is only before the CR3 switch below.)
    let painted_phys = unsafe { boot::paint_phys(0x00_20_40) };
    serial_println!("paint(before cr3): {}", painted_phys);
    boot::delay_loops(boot::DELAY_DEFAULT);
    boot::stage(0x23); // survived the pre-CR3 delay

    // The loader's page tables are scaffolding too.  Build a kernel-owned root
    // (kernel half copied, low identity map dropped) and run on it from here on.
    boot::stage(0x20); // about to build the kernel-owned address space
    let root = match create_kernel_address_space() {
        Some(r) => r,
        None => panic!("paging: cannot create the kernel address space"),
    };
    boot::stage(0x21); // root built, still on the loader's tables
    X86_64Paging::switch_to(root);
    // `stage` itself needs the physmap (for the RAM record), so simply landing
    // here proves the copied kernel half works.
    boot::stage(0x22); // CR3 switched, physmap usable
    boot::stage(boot::STAGE_PAGING);
    serial_println!("paging: kernel-owned root {:#x}", root);

    // Same test again, now on the kernel's own tables: a different colour so the
    // photo says which side of the CR3 switch the machine died on.
    let painted_dev = unsafe { boot::paint_dev(0x00_40_00) };
    serial_println!("paint(after cr3): {}", painted_dev);
    boot::delay_loops(boot::DELAY_DEFAULT);
    boot::stage(0x24); // survived the post-CR3 delay

    // P3: the framebuffer console is the only output channel on the target
    // machine, so bring it up before anything else can fail.
    boot::init_fb_console();
    boot::stage(boot::STAGE_CONSOLE);

    // Necessary invariants: a broken physmap/kernel window/allocator makes the
    // kernel unusable, so fail loudly here in every boot mode.
    boot::stage(boot::STAGE_SELFCHECK);
    boot::self_check();

    // The UEFI stub handed over the ACPI RSDP; parse just what `sys_shutdown`
    // needs (RSDT/XSDT -> FADT -> DSDT `_S5_`).  Reading is best-effort: a
    // machine without ACPI still boots, it just cannot power itself off.
    acpi::init();

    // The test programs must not be preempted while they hold kernel tables,
    // so the mm/elf group runs with interrupts still off (only `sti` in
    // `bring_up_scheduler` turns them on; the IDT was installed before CR3).
    #[cfg(feature = "kernel-tests")]
    tests::run_early();

    io::input::init();

    #[cfg(feature = "kernel-tests")]
    {
        // Ring3 probes never return; their continuation resumes the harness.
        tests::ring3::start()
    }

    #[cfg(not(feature = "kernel-tests"))]
    boot::normal_boot()
}

// ------------------------------------------------------------ trap handlers

fn syscall_handler(f: &mut TrapFrame) {
    // Probe blobs use fake ids to drive the ring3 round-trip; everything else
    // is the real v1 dispatcher.
    if tests::ring3::probe_syscall(f) {
        return;
    }
    syscall::dispatch(f);
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
        if tests::ring3::probe_gp(f) {
            return;
        }
    }
    serial_println!("kernel #GP at rip {:#x} err {:#x}", f.rip, f.error);
    f.dump("general protection");
    halt_loop();
}

fn page_fault_handler(f: &mut TrapFrame) {
    let cr2 = arch::x86_64::cr2();
    if f.from_user() {
        if thread::current_pid().is_some() {
            // A real process: tear it down, the scheduler moves on.
            serial_println!(
                "ring3: #PF cr2 {:#x} err {:#x} at rip {:#x} — killing user context",
                cr2,
                f.error,
                f.rip
            );
            proc::exit_current(ExitStatus::Fault {
                vector: 14,
                rip: f.rip,
                addr: cr2,
            });
            return;
        }
        if tests::ring3::probe_pf(f, cr2) {
            return;
        }
    }
    serial_println!("kernel #PF cr2 {:#x} err {:#x}", cr2, f.error);
    f.dump("page fault");
    halt_loop();
}

/// IRQ1: one PS/2 scancode -> the input queue.
fn keyboard_handler(_f: &mut TrapFrame) {
    let status = arch::x86_64::inb(0x64);
    if status & 1 != 0 {
        io::input::handle_scancode(arch::x86_64::inb(0x60));
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

// ------------------------------------------------------------------- banner

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

#[panic_handler]
pub fn panic(info: &core::panic::PanicInfo) -> ! {
    serial_println!("PANIC: {}", info);
    halt_loop()
}
