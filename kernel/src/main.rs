#![no_main]
#![no_std]
#![feature(abi_x86_interrupt)]

use core::arch::asm;

mod arch;
mod io;
mod loader;
mod mm;
mod utils;

use arch::x86::{
    self, inb,
    intr::{ExceptionStackFrame, INTR_TABLE},
    outb,
    pic::{pic_init, pit_configure_channel},
};

static mut TICKS: u64 = 0;
const TIMER_FREQ: u32 = 200;

#[export_name = "_start"]
fn main() -> ! {
    serial_println!("RondOS> HELLO RondOS");

    println!("HELLO RondOS");
    println!("Kernel Size {} KiB", loader::get_kernel_size() / 1024);
    println!(
        "Available Memory Size {} KiB",
        mm::available_mem_size() / 1024
    );

    pic_init();
    pit_configure_channel(0, 2, TIMER_FREQ);

    INTR_TABLE
        .get_mut()
        .breakpoint
        .set_handle_fn(breakpoint_handler);
    INTR_TABLE
        .get_mut()
        .page_fault
        .set_handle_fn(page_fault_handler);
    INTR_TABLE
        .get_mut()
        .double_fault
        .set_handle_fn(double_fault_handler);
    INTR_TABLE
        .get_mut()
        .segment_not_present
        .set_handle_fn(segment_not_present_handler);
    INTR_TABLE.get_mut()[0x20].set_handle_fn(timer_handler);
    INTR_TABLE.get_mut()[0x21].set_handle_fn(keyboard_handler);

    INTR_TABLE.get_mut().update();

    unsafe {
        asm!("sti");
    }

    println!("esp page: {:x}", mm::pg_round_down(x86::esp() as usize));

    loop {
        unsafe {
            asm!("hlt");
        }
    }
}

extern "x86-interrupt" fn breakpoint_handler(f: ExceptionStackFrame) {
    println!("BREAKPOINT: {:?}", f);
}

extern "x86-interrupt" fn double_fault_handler(f: ExceptionStackFrame, _error_code: u32) -> ! {
    println!("DOUBLE FAULT {:?}", f);
    panic!()
}

extern "x86-interrupt" fn page_fault_handler(f: ExceptionStackFrame, error_code: u32) {
    println!("PAGE FAULT#{} {:?}", error_code, f);
}

extern "x86-interrupt" fn timer_handler(_f: ExceptionStackFrame) {
    unsafe {
        TICKS += 1;
    }
    end_of_interrupt();
}

extern "x86-interrupt" fn segment_not_present_handler(f: ExceptionStackFrame, error_code: u32) {
    println!("SEGMENT NOT PRESENT {} {:?}", error_code, f)
}

extern "x86-interrupt" fn keyboard_handler(_f: ExceptionStackFrame) {
    let scancode = inb(0x60);
    if let Some(ch) = scancode_to_char(scancode) {
        print!("{}", ch);
    }
    end_of_interrupt();
}

#[panic_handler]
pub fn panic(info: &::core::panic::PanicInfo) -> ! {
    println!("{:?}", info);
    serial_println!("{:?}", info);
    loop {}
}

pub fn end_of_interrupt() {
    outb(0x20, 0x20);
}

pub fn scancode_to_char(code: u8) -> Option<char> {
    // println!("scancode {}", code);
    match code {
        0x00 => {
            panic!("Error Scancode 0x00")
        }
        0x02..=0x0a => Some((b'0' + code - 1) as char),
        0x0b => Some('0'),
        0x10..=0x19 => {
            Some(['q', 'w', 'e', 'r', 't', 'y', 'u', 'i', 'o', 'p'][code as usize - 0x10])
        }
        0x1c => Some('\n'),
        0x0e => Some(0x08 as char),
        0x1e..=0x26 => Some(['a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l'][code as usize - 0x1e]),
        0x2c..=0x32 => Some(['z', 'x', 'c', 'v', 'b', 'n', 'm'][code as usize - 0x2c]),
        0x39 => Some(' '),
        0x80.. => None,
        _ => Some('?'),
    }
}
