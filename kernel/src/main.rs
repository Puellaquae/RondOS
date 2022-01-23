#![no_std]
#![no_main]
#![feature(lang_items, naked_functions, asm_const, asm_sym)]
#![allow(dead_code)]

use core::arch::asm;
use core::panic::PanicInfo;

mod interrupt;
mod loader;
mod platform;
mod vga_buffer;
mod x86;

#[lang = "panic_impl"]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);
    loop {}
}

#[no_mangle]
pub extern "C" fn main() -> ! {
    enable_sse();
    println!("Hello World! From ReOS");
    kernel_info();
    print!("Init the interrupts");
    intr_init();
    println!(" Ok!");
    println!("Try Interrupt!");
    unsafe {
        asm!("int 3");
    }
    println!("Try Over!");
    loop {}
}

fn enable_sse() {
    unsafe {
        asm!(
            "mov eax, cr0",
            "and ax, 0xFFFB",
            "or ax, 0x2",
            "mov cr0, eax",
            "mov eax, cr4",
            "or ax, {flag}",
            "mov cr4, eax",
            flag = const 3 << 9
        );
    }
}

fn kernel_info() {
    let kernel_size = loader::get_kernel_size();
    println!("Kernel Size: 0x{:x}B", kernel_size);
    println!("Kernel Base in vaddr: 0x{:x}", loader::KERNEL_VADDR_BASE);
    println!("Kernel Stack in paddr: 0x{:x}", loader::KERNEL_STACK_PADDR);
    println!(
        "Kernel Stack in vaddr: 0x{:x}",
        loader::KERNEL_VADDR_BASE + loader::KERNEL_STACK_PADDR
    );
    println!(
        "Kernel Place in paddr: 0x{:x} - 0x{:x}",
        loader::KERNEL_PLACE_BEGIN_PADDR,
        loader::KERNEL_PLACE_BEGIN_PADDR + kernel_size
    );
    println!(
        "Kernel Place in vaddr: 0x{:x} - 0x{:x}",
        loader::KERNEL_VADDR_BASE + loader::KERNEL_PLACE_BEGIN_PADDR,
        loader::KERNEL_VADDR_BASE + loader::KERNEL_PLACE_BEGIN_PADDR + kernel_size
    );
}

use crate::interrupt::{ExceptionStackFrame, Idt, Interrupts };
use lazy_static::lazy_static;

lazy_static! {
    static ref IDT: Idt = {
        let mut idt = Idt::new();

        idt.set_handler(Interrupts::BreakpointException, handler!(breakpoint_handler));

        idt
    };
}

extern "C" fn breakpoint_handler(f: &ExceptionStackFrame) -> ! {
    println!("EXCEPTION: BreakPoint");
    println!("{:#?}", f);
    loop {}
}

fn intr_init() {
    IDT.load();
}
