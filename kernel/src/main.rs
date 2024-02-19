#![no_main]
#![no_std]
#![feature(abi_x86_interrupt)]

use core::arch::global_asm;
use core::cell::Cell;
use core::fmt;
use core::mem::size_of;
use core::{arch::asm, panic::PanicInfo};

mod loader;

use core::fmt::Write;

static mut VGA: Buf = Buf { ix: 0, iy: 0 };

#[export_name = "_start"]
fn main() -> ! {
    unsafe { &mut VGA }.clear();
    unsafe {
        asm!("cli");
    }
    writeln!(unsafe { &mut VGA }, "HELLO").unwrap();
    writeln!(unsafe { &mut VGA }, "Prepare Interrupt Table").unwrap();

    let mut idts = [InterruptEntry {
        pointer_low: 0,
        gdt_selector: 0,
        options: 0x0e00,
        pointer_middle: 0,
    }; 256];

    idts[3] = InterruptEntry {
        pointer_low: (breakpoint_handler as usize) as u16,
        gdt_selector: loader::SEGMENT_KERNEL_CODE,
        options: 0b1000111000000000,
        pointer_middle: ((breakpoint_handler as usize) >> 16) as u16,
    };
    writeln!(unsafe { &mut VGA }, "{:?}", idts[3]).unwrap();

    let pidt = DescriptorTablePointer {
        base: idts.as_ptr() as u32,
        limit: (size_of::<[InterruptEntry; 256]>() - 1) as u16,
    };

    writeln!(unsafe { &mut VGA }, "Load Interrupt Table").unwrap();
    lidt(&pidt);

    // writeln!(unsafe { &mut VGA }, "Enable Interrupt").unwrap();
    // unsafe {
    //     asm!("sti");
    // }

    writeln!(unsafe { &mut VGA }, "Interrupt Test").unwrap();
    unsafe {
        asm!("int 3");
    }

    loop {}
}

extern "x86-interrupt" fn breakpoint_handler(f: ExceptionStackFrame) {
    writeln!(unsafe { &mut VGA }, "{:?}", f).unwrap();
}

#[panic_handler]
pub fn panic(_info: &::core::panic::PanicInfo) -> ! {
    loop {}
}

struct Buf {
    // 0..80
    ix: isize,
    // 0..25
    iy: isize,
}

impl Buf {
    fn putc(&mut self, c: u8) {
        match c {
            b'\n' => {
                self.iy += 1;
                self.ix = 0;
            }
            b'\r' => {
                self.ix = 0;
            }
            _ => {
                let vga_buffer = 0xb8000 as *mut u8;
                unsafe {
                    *vga_buffer.offset((self.iy * 80 + self.ix) * 2) = c;
                    *vga_buffer.offset((self.iy * 80 + self.ix) * 2 + 1) = 15;
                }
                self.iy += (self.ix + 1) / 80;
                self.ix = (self.ix + 1) % 80;
            }
        }
    }

    fn write_bytes(&mut self, s: &[u8]) {
        for &b in s.iter() {
            self.putc(b);
        }
    }

    fn clear(&mut self) {
        let vga_buffer = 0xb8000 as *mut u8;
        for i in 0..(25 * 80) {
            unsafe {
                *vga_buffer.offset(i * 2) = b' ';
                *vga_buffer.offset(i * 2 + 1) = 0xb;
            }
        }
        self.ix = 0;
        self.iy = 0;
    }
}

impl fmt::Write for Buf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_bytes(s.as_bytes());
        Ok(())
    }
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
    pub error_code: u32,
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
