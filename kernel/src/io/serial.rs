#![allow(dead_code)]

use core::fmt;

use crate::{
    utils::singleton::Singleton,
    x86::{inb, outb},
};

macro_rules! wait_for {
    ($cond:expr) => {
        while !$cond {
            core::hint::spin_loop()
        }
    };
}

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
pub struct SerialPort(u16 /* base port */);

impl SerialPort {
    /// Base port.
    fn port_base(&self) -> u16 {
        self.0
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
        Self(base)
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

    /// Sends a byte on the serial port.
    pub fn send(&mut self, data: u8) {
        match data {
            8 | 0x7F => {
                wait_for!(self.line_sts().0 & OUTPUT_EMPTY == OUTPUT_EMPTY);
                outb(self.port_data(), 8);
                wait_for!(self.line_sts().0 & OUTPUT_EMPTY == OUTPUT_EMPTY);
                outb(self.port_data(), b' ');
                wait_for!(self.line_sts().0 & OUTPUT_EMPTY == OUTPUT_EMPTY);
                outb(self.port_data(), 8);
            }
            _ => {
                wait_for!(self.line_sts().0 & OUTPUT_EMPTY == OUTPUT_EMPTY);
                outb(self.port_data(), data);
            }
        }
    }

    /// Sends a raw byte on the serial port, intended for binary data.
    pub fn send_raw(&mut self, data: u8) {
        wait_for!(self.line_sts().0 & OUTPUT_EMPTY == OUTPUT_EMPTY);
        outb(self.port_data(), data);
    }

    /// Receives a byte on the serial port.
    pub fn receive(&mut self) -> u8 {
        wait_for!(self.line_sts().0 & INPUT_FULL == INPUT_FULL);
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
    SERIAL_IO.get_mut().write_fmt(args).unwrap();
}
