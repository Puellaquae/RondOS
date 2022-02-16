use crate::x86::reg::outb;

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
    outb(PIT_PORT_CONTROL, ((channel << 6) | 0x30 | (mode << 1)) as u8);
    outb(PIT_PORT_COUNTER_OFFSET_TO_CHANNEL + channel, count as u8);
    outb(PIT_PORT_COUNTER_OFFSET_TO_CHANNEL + channel, (count >> 8) as u8);
    // intr_set_level(old_level);
}

pub fn end_of_interrupt(pic: u16) {
    outb(pic, 0x20);
}
