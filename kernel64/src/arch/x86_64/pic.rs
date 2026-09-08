//! 8259 PIC + 8254 PIT — M0.4.
//!
//! We keep the legacy interrupt controllers (design decision §13: "沿用现有
//! 8259+PIT"，APIC/IOAPIC 推到 P5).  The PIC is remapped to vectors 0x20..0x2F
//! so it no longer collides with CPU exceptions, and only IRQ0 (timer) and
//! IRQ1 (keyboard) are unmasked.

#![allow(dead_code)]

use super::{inb, outb};

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

const ICW1_INIT: u8 = 0x11; // init + ICW4 needed
const ICW4_8086: u8 = 0x01;

/// Vector the master PIC starts at (IRQ0 -> 0x20).
pub const IRQ_BASE: u8 = 0x20;

pub const IRQ_TIMER: u8 = 0;
pub const IRQ_KEYBOARD: u8 = 1;

/// Program both PICs and unmask only the interrupts we handle.
pub fn init() {
    outb(PIC1_CMD, ICW1_INIT);
    outb(PIC2_CMD, ICW1_INIT);
    
    outb(PIC1_DATA, IRQ_BASE); // master: vectors 0x20..0x27
    outb(PIC2_DATA, IRQ_BASE + 8); // slave: vectors 0x28..0x2F
    
    outb(PIC1_DATA, 0x04); // slave on IRQ2
    outb(PIC2_DATA, 0x02); // slave cascade identity
    
    outb(PIC1_DATA, ICW4_8086);
    outb(PIC2_DATA, ICW4_8086);
    
    // Mask everything but IRQ0 and IRQ1 on the master, everything on the
    // slave (bit set = masked).
    outb(PIC1_DATA, 0b1111_1110);
    outb(PIC2_DATA, 0b1111_1111);
}

pub fn end_of_interrupt(irq: u8) {
    if irq >= 8 {
    outb(PIC2_CMD, 0x20);
    }
    outb(PIC1_CMD, 0x20);
}

/// Configure a PIT channel.  `mode` 2 is the rate generator (periodic).
pub fn configure_pit(channel: u8, mode: u8, freq: u32) {
    assert!(channel < 3 && freq > 0);
    let divisor = (1_193_182 / freq) as u16;
    let cmd = (channel << 6) | (0b11 << 4) | ((mode & 0x7) << 1);
    outb(0x43, cmd);
    outb(0x40 + channel as u16, (divisor & 0xff) as u8);
    outb(0x40 + channel as u16, (divisor >> 8) as u8);
}

pub fn read_irr() -> u8 {
    outb(PIC1_CMD, 0x0A);
    inb(PIC1_CMD)
}

pub fn read_isr() -> u8 {
    outb(PIC1_CMD, 0x0B);
    inb(PIC1_CMD)
}
