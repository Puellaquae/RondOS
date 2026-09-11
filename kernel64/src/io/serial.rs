#![allow(dead_code)]

use core::fmt;

use crate::{
    arch::x86_64::{inb, outb},
    utils::singleton::Singleton,
};

/// How many polls to wait for the UART before giving up.
///
/// The target machine has **no serial port**.  A floating bus returns all-ones
/// for the line-status register, which reads as "transmitter empty", so the
/// UART looks alive and then never drains — and the old unbounded `wait_for!`
/// spun there forever.  The machine then looked like it "reset at stage 3"
/// (the stage ends with a log line) when it had really just hung.  Every wait
/// is now bounded, and a port that times out is switched off for good.
const WAIT_LIMIT: u32 = 200_000;

const RECEIVED: u8 = 1;
const SENT: u8 = 1 << 1;
const ERRORED: u8 = 1 << 2;
const STATUS_CHANGE: u8 = 1 << 3;
// 4 to 7 are unused

/// Line status flags
struct LineStsFlags(pub u8);

const INPUT_FULL: u8 = 1;
// 1 to 4 unknown
const OUTPUT_EMPTY: u8 = 1 << 5;
// 6 and 7 unknown

#[derive(Debug)]
pub struct SerialPort {
    base: u16,
    /// Set once a wait timed out: the port is absent or wedged, so stop using
    /// it.  Logging keeps working on the framebuffer console.
    dead: bool,
}

impl SerialPort {
    /// Base port.
    fn port_base(&self) -> u16 {
        self.base
    }

    /// Data port.
    ///
    /// Read and write.
    fn port_data(&self) -> u16 {
        self.port_base()
    }

    /// Interrupt enable port.
    ///
    /// Write only.
    fn port_int_en(&self) -> u16 {
        self.port_base() + 1
    }

    /// Fifo control port.
    ///
    /// Write only.
    fn port_fifo_ctrl(&self) -> u16 {
        self.port_base() + 2
    }

    /// Line control port.
    ///
    /// Write only.
    fn port_line_ctrl(&self) -> u16 {
        self.port_base() + 3
    }

    /// Modem control port.
    ///
    /// Write only.
    fn port_modem_ctrl(&self) -> u16 {
        self.port_base() + 4
    }

    /// Line status port.
    ///
    /// Read only.
    fn port_line_sts(&self) -> u16 {
        self.port_base() + 5
    }

    /// Creates a new serial port interface on the given I/O base port.
    ///
    /// This function is unsafe because the caller must ensure that the given base address
    /// really points to a serial port device and that the caller has the necessary rights
    /// to perform the I/O operation.
    pub const unsafe fn new(base: u16) -> Self {
        Self { base, dead: false }
    }

    /// Poll `cond`, giving up after [`WAIT_LIMIT`] iterations.  `false` means
    /// the port is not answering and should be retired.
    fn wait(&mut self, cond: impl Fn(&mut Self) -> bool) -> bool {
        let mut spins = 0u32;
        while !cond(self) {
            if spins >= WAIT_LIMIT {
                self.dead = true;
                return false;
            }
            spins += 1;
            core::hint::spin_loop();
        }
        true
    }

    /// Initializes the serial port.
    ///
    /// The default configuration of [38400/8-N-1](https://en.wikipedia.org/wiki/8-N-1) is used.
    pub fn init(&mut self) {
        // Disable interrupts
        outb(self.port_int_en(), 0x00);

        // Enable DLAB
        outb(self.port_line_ctrl(), 0x80);

        // Set maximum speed to 38400 bps by configuring DLL and DLM
        outb(self.port_data(), 0x03);
        outb(self.port_int_en(), 0x00);

        // Disable DLAB and set data word length to 8 bits
        outb(self.port_line_ctrl(), 0x03);

        // Enable FIFO, clear TX/RX queues and
        // set interrupt watermark at 14 bytes
        outb(self.port_fifo_ctrl(), 0xc7);

        // Mark data terminal ready, signal request to send
        // and enable auxilliary output #2 (used as interrupt line for CPU)
        outb(self.port_modem_ctrl(), 0x0b);

        // Enable interrupts
        outb(self.port_int_en(), 0x01);
    }

    fn line_sts(&mut self) -> LineStsFlags {
        LineStsFlags(inb(self.port_line_sts()))
    }

    /// Sends a byte on the serial port.  Bounded: a dead port drops the byte.
    pub fn send(&mut self, data: u8) {
        if self.dead {
            return;
        }
        match data {
            8 | 0x7F => {
                if !self.wait(|s| s.line_sts().0 & OUTPUT_EMPTY == OUTPUT_EMPTY) {
                    return;
                }
                outb(self.port_data(), 8);
                if !self.wait(|s| s.line_sts().0 & OUTPUT_EMPTY == OUTPUT_EMPTY) {
                    return;
                }
                outb(self.port_data(), b' ');
                if !self.wait(|s| s.line_sts().0 & OUTPUT_EMPTY == OUTPUT_EMPTY) {
                    return;
                }
                outb(self.port_data(), 8);
            }
            _ => {
                if !self.wait(|s| s.line_sts().0 & OUTPUT_EMPTY == OUTPUT_EMPTY) {
                    return;
                }
                outb(self.port_data(), data);
            }
        }
    }

    /// Sends a raw byte on the serial port, intended for binary data.
    pub fn send_raw(&mut self, data: u8) {
        if self.dead || !self.wait(|s| s.line_sts().0 & OUTPUT_EMPTY == OUTPUT_EMPTY) {
            return;
        }
        outb(self.port_data(), data);
    }

    /// True when the port has been retired after a timeout.
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// Receives a byte on the serial port.  Returns 0 when the port is silent.
    pub fn receive(&mut self) -> u8 {
        if self.dead || !self.wait(|s| s.line_sts().0 & INPUT_FULL == INPUT_FULL) {
            return 0;
        }
        inb(self.port_data())
    }
}

impl fmt::Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            self.send(byte);
        }
        Ok(())
    }
}

impl Default for SerialPort {
    fn default() -> Self {
        let mut serial = unsafe { Self::new(0x3f8) };
        serial.init();
        serial
    }
}

static SERIAL_IO: Singleton<SerialPort> = Singleton::UNINIT;

/// Mirror raw terminal bytes (console-device writes) to COM1 only.
///
/// Unlike [`_serial_print`], this does **not** touch the framebuffer: the
/// caller already wrote the same bytes there, and going through the log path
/// would prefix them and append a newline.  `send` already turns a backspace
/// into `BS space BS`, so a serial terminal erases the same cell.
pub fn write_raw(bytes: &[u8]) {
    let port = SERIAL_IO.get_mut();
    for &b in bytes {
        if b == b'\n' {
            port.send(b'\r');
        }
        port.send(b);
    }
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::io::serial::_serial_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => ($crate::serial_print!("{}\n", format_args!($($arg)*)));
}

#[doc(hidden)]
pub fn _serial_print(args: fmt::Arguments) {
    use core::fmt::Write;
    // Interrupts off: a preempted thread inside `write_fmt` would otherwise
    // interleave bytes with the next thread (and a naive spin lock would
    // deadlock on a single CPU).
    let if_set = crate::arch::x86_64::interrupts_enabled();
    crate::arch::x86_64::cli();
    // Mirror to the framebuffer console as well: on the target machine there is
    // no serial port, so this is the only way to see kernel output.
    if crate::io::fb::is_ready() {
        let mut sink = crate::io::fb::Writer;
        let _ = fmt::Write::write_fmt(&mut sink, args);
    }
    SERIAL_IO.get_mut().write_fmt(args).unwrap();
    if if_set {
        crate::arch::x86_64::sti();
    }
}
