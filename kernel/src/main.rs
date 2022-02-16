#![no_std]
#![no_main]
#![feature(lang_items, naked_functions, asm_const, asm_sym)]
#![allow(dead_code)]

use core::arch::asm;
use core::panic::PanicInfo;

mod loader;
mod os;
mod platform;
mod vga_buffer;
mod x86;

use os::RondOs;

#[lang = "panic_impl"]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);
    loop {}
}

#[no_mangle]
pub extern "C" fn main() -> ! {
    let mut os = RondOs::new();
    println!("Hello World! From ReOS");
    kernel_info();
    println!("Init the interrupts");
    os.intr_init();
    unsafe {
        asm!("int 3");
        *(0xdeadbeaf as *mut u32) = 42;
    }

    println!("Init the timer");
    os.timer_init();

    println!("Enable interrput");
    unsafe {
        asm!("sti");
    }

    loop { }
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
