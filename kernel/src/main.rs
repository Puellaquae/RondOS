#![no_std]
#![no_main]
#![feature(lang_items, naked_functions, asm_const, asm_sym)]
#![allow(dead_code)]

use core::arch::asm;
use core::panic::PanicInfo;

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
    println!("Hello World! From ReOS");
    kernel_info();
    print!("Init the interrupts");
    intr_init();
    println!(" Ok!");
    println!("Try Interrupt!");

    unsafe {
        asm!("int 3");
        *(0xdeadbeaf as *mut u32) = 42;
    }
    println!("Try Over!");
    loop {}
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

use lazy_static::lazy_static;
use x86::{
    interrupt::{ExceptionStackFrame, Idt, Interrupts, PageFaultErrorCode},
    reg::get_cr2,
};

lazy_static! {
    static ref IDT: Idt = {
        let mut idt = Idt::new();

        idt.set_handler(
            Interrupts::BreakpointException,
            handler!(breakpoint_handler),
        );
        idt.set_handler(
            Interrupts::PageFaultException,
            handler_with_code!(page_fault_handler),
        );
        idt.set_handler(
            Interrupts::DoubleFaultException,
            handler_with_code!(double_handler),
        );
        idt
    };
}

extern "C" fn breakpoint_handler(f: &ExceptionStackFrame) {
    println!("EXCEPTION: BreakPoint At 0x{:x}", f.instruction_pointer);
}

extern "C" fn page_fault_handler(f: &ExceptionStackFrame) {
    println!(
        "EXCEPTION: Page Fault {:?} When Access 0x{:x}",
        PageFaultErrorCode::from_bits(f.error_code).unwrap(),
        get_cr2()
    );
}

extern "C" fn double_handler(f: &ExceptionStackFrame) {
    println!("EXCEPTION: Double Fault");
    println!("{:#?}", f);
}

fn intr_init() {
    IDT.load();
}
