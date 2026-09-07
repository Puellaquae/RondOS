#![no_main]
#![no_std]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod arch;
mod io;
mod loader;
mod mm;
mod thread;
mod utils;

use arch::x86::{self, inb};
use arch::x86::intr::{end_of_interrupt, ExceptionStackFrame, INTR_TABLE};
use arch::x86::pic::{pic_init, pit_configure_channel};

const TIMER_FREQ: u32 = 200;

#[export_name = "_start"]
fn main() -> ! {
    serial_println!("RondOS> HELLO RondOS");

    mm::init_heap();
    heap_smoke_test();

    println!("HELLO RondOS");
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
    INTR_TABLE.get_mut()[0x21].set_handle_fn(keyboard_handler);

    // Bootstrap the thread subsystem. This must happen before interrupts are
    // enabled: `thread::init` registers the boot flow as the "main" thread and
    // allocates the idle thread's stack.
    thread::init();

    // Vector 0x20 (PIT timer) drives the preemptive scheduler. It is routed to
    // an assembly stub rather than an `x86-interrupt` handler so that the full
    // interrupted register state is saved in a fixed layout.
    INTR_TABLE.get_mut()[0x20]
        .set_handle_addr(thread::irq0_stub as unsafe extern "C" fn() as usize);

    INTR_TABLE.get_mut().update();

    for m in loader::get_memlayout() {
        println!("{:?}", m);
    }

    let tp = mm::pg_round_down(x86::esp() as usize);
    println!("esp page: {:x}", tp);

    thread::thread_create("busy-a", busy_a, 0);
    thread::thread_create("busy-b", busy_b, 1);
    thread::thread_create("sleeper", sleeper, 2);

    println!("RondOS> threads created, enabling preemption");

    x86::sti();

    // The boot flow stays alive as an ordinary round-robin thread. `hlt` keeps
    // it mostly idle; the timer tick preempts it like any other thread.
    loop {
        x86::hlt();
    }
}

fn heap_smoke_test() {
    use alloc::boxed::Box;
    use alloc::format;
    use alloc::vec::Vec;

    let mut v: Vec<u64> = Vec::new();
    for i in 0..16 {
        v.push(i as u64 * 3);
    }
    let mut b = Box::new(7u32);
    *b += 1;
    serial_println!(
        "heap: v.len={} v[5]={} box={} free={}KiB",
        v.len(),
        v[5],
        *b,
        mm::heap_free_bytes() / 1024
    );
    drop(v);
    drop(b);
    let s = format!("freed -> free={}KiB", mm::heap_free_bytes() / 1024);
    serial_println!("{}", s);
}

fn busy_a(_arg: usize) {
    let mut n: u64 = 0;
    loop {
        n = n.wrapping_add(1);
        if n % 10_000_000 == 0 {
            serial_println!("a {} @t{}", n, thread::ticks());
        }
    }
}

fn busy_b(_arg: usize) {
    let mut n: u64 = 0;
    loop {
        n = n.wrapping_add(1);
        if n % 10_000_000 == 0 {
            serial_println!("b {} @t{}", n, thread::ticks());
        }
    }
}

fn sleeper(_arg: usize) {
    for i in 0..5 {
        serial_println!("c {} @t{}", i, thread::ticks());
        thread::sleep(500);
    }
    serial_println!("c done @t{}", thread::ticks());
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
