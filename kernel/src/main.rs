#![no_main]
#![no_std]
#![feature(abi_x86_interrupt)]

use core::arch::asm;
use core::mem::size_of;

mod io;
mod loader;
mod x86;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Interrupts {
    DivideError = 0,
    DebugException = 1,
    Interrupt = 2,
    BreakpointException = 3,
    OverflowException = 4,
    BoundRangeExceededException = 5,
    InvalidOpcodeException = 6,
    DeviceNotAvailableException = 7,
    DoubleFaultException = 8,
    CoprocessorSegmentOverrun = 9,
    InvaildTSSException = 10,
    SegmentNotPresent = 11,
    StackFaultException = 12,
    GeneralProtectionException = 13,
    PageFaultException = 14,
    FPUFloatingPointException = 16,
    AlignmentCheckException = 17,
    MachineCheckException = 18,
    SIMDFloatingPointException = 19,
}

static mut TICKS: u64 = 0;
const TIMER_FREQ: u32 = 200;

#[export_name = "_start"]
fn main() -> ! {
    serial_out().init();

    println!("HELLO RondOS");
    println!("Kernel Size {} KiB", loader::get_kernel_size() / 1024);
    println!("MEMLayout:\n{:?}", loader::get_memlayout());
    println!("Init PIC");

    pic_init();
    pit_configure_channel(0, 2, TIMER_FREQ);

    println!("Init Interrupt Table");

    let mut idts = [InterruptEntry {
        pointer_low: 0,
        gdt_selector: 0,
        options: 0b0000111000000000,
        pointer_middle: 0,
    }; 256];

    idts[Interrupts::BreakpointException as usize] = InterruptEntry {
        pointer_low: (breakpoint_handler as usize) as u16,
        gdt_selector: loader::SEGMENT_KERNEL_CODE,
        options: 0b1000111000000000,
        pointer_middle: ((breakpoint_handler as usize) >> 16) as u16,
    };

    idts[Interrupts::PageFaultException as usize] = InterruptEntry {
        pointer_low: (page_fault_handler as usize) as u16,
        gdt_selector: loader::SEGMENT_KERNEL_CODE,
        options: 0b1000111000000000,
        pointer_middle: ((page_fault_handler as usize) >> 16) as u16,
    };

    idts[Interrupts::DoubleFaultException as usize] = InterruptEntry {
        pointer_low: (double_fault_handler as usize) as u16,
        gdt_selector: loader::SEGMENT_KERNEL_CODE,
        options: 0b1000111000000000,
        pointer_middle: ((double_fault_handler as usize) >> 16) as u16,
    };

    idts[Interrupts::SegmentNotPresent as usize] = InterruptEntry {
        pointer_low: (segment_not_present_handler as usize) as u16,
        gdt_selector: loader::SEGMENT_KERNEL_CODE,
        options: 0b1000111000000000,
        pointer_middle: ((segment_not_present_handler as usize) >> 16) as u16,
    };

    idts[0x20] = InterruptEntry {
        pointer_low: (timer_handler as usize) as u16,
        gdt_selector: loader::SEGMENT_KERNEL_CODE,
        options: 0b1000111000000000,
        pointer_middle: ((timer_handler as usize) >> 16) as u16,
    };

    idts[0x21] = InterruptEntry {
        pointer_low: (keyboard_handler as usize) as u16,
        gdt_selector: loader::SEGMENT_KERNEL_CODE,
        options: 0b1000111000000000,
        pointer_middle: ((keyboard_handler as usize) >> 16) as u16,
    };

    let pidt = DescriptorTablePointer {
        base: idts.as_ptr() as u32,
        limit: (size_of::<[InterruptEntry; 256]>() - 1) as u16,
    };

    lidt(&pidt);

    unsafe {
        asm!("sti");
    }

    serial_println!("RondOS> Serial Output Test!");

    loop {
        unsafe {
            asm!("hlt");
        }
    }
}

extern "x86-interrupt" fn breakpoint_handler(f: ExceptionStackFrame) {
    println!("BREAKPOINT: {:?}", f);
}

extern "x86-interrupt" fn double_fault_handler(f: ExceptionStackFrame, _error_code: u32) {
    println!("DOUBLE FAULT {:?}", f);
}

extern "x86-interrupt" fn page_fault_handler(f: ExceptionStackFrame, error_code: u32) {
    println!("PAGE FAULT#{} {:?}", error_code, f);
}

extern "x86-interrupt" fn timer_handler(_f: ExceptionStackFrame) {
    unsafe {
        TICKS += 1;
    }
    end_of_interrupt(0x20);
}

extern "x86-interrupt" fn segment_not_present_handler(f: ExceptionStackFrame, error_code: u32) {
    println!("SEGMENT NOT PRESENT {} {:?}", error_code, f)
}

extern "x86-interrupt" fn keyboard_handler(_f: &ExceptionStackFrame) {
    let scancode = inb(0x60);
    if let Some(ch) = scancode_to_char(scancode) {
        print!("{}", ch);
    }
    end_of_interrupt(0x20);
}

#[panic_handler]
pub fn panic(_info: &::core::panic::PanicInfo) -> ! {
    loop {}
}

#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct InterruptEntry {
    pointer_low: u16,
    gdt_selector: u16,
    options: u16,
    pointer_middle: u16,
}

#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct DescriptorTablePointer {
    /// Size of the DT.
    pub limit: u16,
    /// Pointer to the memory region containing the DT.
    pub base: u32,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ExceptionStackFrame {
    pub instruction_pointer: u32,
    pub code_segment: u16,
    pub cpu_flags: u32,
    pub stack_pointer: u32,
    pub stack_segment: u16,
}

pub fn lidt(idt: &DescriptorTablePointer) {
    unsafe {
        asm!("lidt [{}]", in(reg) idt, options(readonly, nostack, preserves_flags));
    }
}

/// Programmable Interrupt Controller (PIC) registers.
/// A PC has two PICs, called the master and slave PICs, with the
/// slave attached ("cascaded") to the master IRQ line 2.

/// Master PIC control register address.
const PIC0_CTRL: u16 = 0x20;

/// Master PIC data register address.
const PIC0_DATA: u16 = 0x21;

/// Slave PIC control register address.
const PIC1_CTRL: u16 = 0xa0;

/// Slave PIC data register address.
const PIC1_DATA: u16 = 0xa1;

use x86::{inb, outb};

use crate::io::serial_out;

pub fn pic_init() {
    outb(PIC0_DATA, 0xff);
    outb(PIC1_DATA, 0xff);

    /* Initialize master. */
    outb(PIC0_CTRL, 0x11); /* ICW1: single mode, edge triggered, expect ICW4. */
    outb(PIC0_DATA, 0x20); /* ICW2: line IR0...7 -> irq 0x20...0x27. */
    outb(PIC0_DATA, 0x04); /* ICW3: slave PIC on line IR2. */
    outb(PIC0_DATA, 0x01); /* ICW4: 8086 mode, normal EOI, non-buffered. */

    /* Initialize slave. */
    outb(PIC1_CTRL, 0x11); /* ICW1: single mode, edge triggered, expect ICW4. */
    outb(PIC1_DATA, 0x28); /* ICW2: line IR0...7 -> irq 0x28...0x2f. */
    outb(PIC1_DATA, 0x02); /* ICW3: slave ID is 2. */
    outb(PIC1_DATA, 0x01); /* ICW4: 8086 mode, normal EOI, non-buffered. */

    /* Unmask all interrupts. */
    outb(PIC0_DATA, 0x00);
    outb(PIC1_DATA, 0x00);
}

/* Interface to 8254 Programmable Interrupt Timer (PIT).
Refer to [8254] for details. */

/* 8254 registers. */
const PIT_PORT_CONTROL: u16 = 0x43; /* Control port. */
const PIT_PORT_COUNTER_OFFSET_TO_CHANNEL: u16 = 0x40; /* Counter port. */

/* PIT cycles per second. */
const PIT_HZ: u32 = 1193180;

/* Configure the given CHANNEL in the PIT.  In a PC, the PIT's
three output channels are hooked up like this:

  - Channel 0 is connected to interrupt line 0, so that it can
    be used as a periodic timer interrupt, as implemented in
    Pintos in devices/timer.c.

  - Channel 1 is used for dynamic RAM refresh (in older PCs).
    No good can come of messing with this.

  - Channel 2 is connected to the PC speaker, so that it can
    be used to play a tone, as implemented in Pintos in
    devices/speaker.c.

MODE specifies the form of output:

  - Mode 2 is a periodic pulse: the channel's output is 1 for
    most of the period, but drops to 0 briefly toward the end
    of the period.  This is useful for hooking up to an
    interrupt controller to generate a periodic interrupt.

  - Mode 3 is a square wave: for the first half of the period
    it is 1, for the second half it is 0.  This is useful for
    generating a tone on a speaker.

  - Other modes are less useful.

FREQUENCY is the number of periods per second, in Hz. */
pub fn pit_configure_channel(channel: u16, mode: u16, frequency: u32) {
    let count: u16;
    // enum intr_level old_level;

    assert!(channel == 0 || channel == 2);
    assert!(mode == 2 || mode == 3);

    /* Convert FREQUENCY to a PIT counter value.  The PIT has a
    clock that runs at PIT_HZ cycles per second.  We must
    translate FREQUENCY into a number of these cycles. */
    if frequency < 19 {
        /* Frequency is too low: the quotient would overflow the
        16-bit counter.  Force it to 0, which the PIT treats as
        65536, the highest possible count.  This yields a 18.2
        Hz timer, approximately. */
        count = 0;
    } else if frequency > PIT_HZ {
        /* Frequency is too high: the quotient would underflow to
        0, which the PIT would interpret as 65536.  A count of 1
        is illegal in mode 2, so we force it to 2, which yields
        a 596.590 kHz timer, approximately.  (This timer rate is
        probably too fast to be useful anyhow.) */
        count = 2;
    } else {
        count = ((PIT_HZ + frequency / 2) / frequency) as u16;
    }

    /* Configure the PIT mode and load its counters. */
    // old_level = intr_disable();
    outb(
        PIT_PORT_CONTROL,
        ((channel << 6) | 0x30 | (mode << 1)) as u8,
    );
    outb(PIT_PORT_COUNTER_OFFSET_TO_CHANNEL + channel, count as u8);
    outb(
        PIT_PORT_COUNTER_OFFSET_TO_CHANNEL + channel,
        (count >> 8) as u8,
    );
    // intr_set_level(old_level);
}

pub fn end_of_interrupt(pic: u16) {
    outb(pic, 0x20);
}

pub fn scancode_to_char(code: u8) -> Option<char> {
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
