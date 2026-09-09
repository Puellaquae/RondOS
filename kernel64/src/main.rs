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

mod arch;
mod bootinfo;
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

    // M0.3 (finished in P2): user programs are built for SSE2, so the kernel
    // must turn the FPU/SSE on and save it per thread.
    arch::x86_64::enable_sse();
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

    // The loader's page tables are scaffolding too.  Build a kernel-owned root
    // (kernel half copied, low identity map dropped) and run on it from here on.
    let root = match create_kernel_address_space() {
        Some(r) => r,
        None => panic!("paging: cannot create the kernel address space"),
    };
    X86_64Paging::switch_to(root);
    serial_println!("paging: kernel-owned root {:#x}", root);

    // Programs that must not be preempted: mm/elf checks run with IF clear.
    tests::run_early();

    gdt::init();
    percpu::init();
    intr::init();
    serial_println!("gdt/tss/percpu/idt ready (gs {:#x})", percpu::gs_base());

    intr::set_handler(intr::VECTOR_SYSCALL, syscall_handler);
    intr::set_handler(intr::VECTOR_GENERAL_PROTECTION, gp_handler);
    intr::set_handler(intr::VECTOR_PAGE_FAULT, page_fault_handler);
    intr::set_handler(6, invalid_opcode_handler);
    intr::set_handler(intr::VECTOR_DOUBLE_FAULT, double_fault_handler);

    // Ring3 probes never return; their continuation resumes the harness.
    tests::ring3::start()
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
