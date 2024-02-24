use core::{
    fmt,
    marker::Copy,
    ptr::{read_volatile, write_volatile},
};

use crate::x86::{inb, outb};

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct ColorCode(u8);

impl ColorCode {
    fn new(foreground: Color, background: Color) -> ColorCode {
        ColorCode((background as u8) << 4 | (foreground as u8))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct ScreenChar {
    ascii_character: u8,
    color_code: ColorCode,
}

const BUFFER_HEIGHT: usize = 25;
const BUFFER_WIDTH: usize = 80;

struct Voliate<T: Sized + Copy> {
    val: T,
}

impl<T: Sized + Copy> Voliate<T> {
    fn read(&self) -> T {
        unsafe { read_volatile((&self.val) as *const T) }
    }

    fn write(&mut self, val: T) {
        unsafe { write_volatile((&mut self.val) as *mut T, val) };
    }
}

impl<T: Sized + Copy> Copy for Voliate<T> {}

impl<T: Sized + Copy> Clone for Voliate<T> {
    fn clone(&self) -> Self {
        Self { val: self.read() }
    }
}

#[repr(transparent)]
struct Buffer {
    chars: [[Voliate<ScreenChar>; BUFFER_WIDTH]; BUFFER_HEIGHT],
}

pub struct VgaBuffer {
    col_pos: usize,
    cur_color: ColorCode,
}

impl VgaBuffer {
    pub fn new() -> VgaBuffer {
        VgaBuffer {
            col_pos: 0,
            cur_color: ColorCode::new(Color::White, Color::Black),
        }
    }

    fn clear_row(&mut self, row: usize) {
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: self.cur_color,
        };
        let buf = unsafe { &mut *(0xb8000 as *mut Buffer) };
        for col in 0..BUFFER_WIDTH {
            buf.chars[row][col].write(blank);
        }
    }

    fn new_line(&mut self) {
        let buf = unsafe { &mut *(0xb8000 as *mut Buffer) };
        for row in 1..BUFFER_HEIGHT {
            buf.chars.copy_within(row..=row, row - 1);
        }
        self.clear_row(BUFFER_HEIGHT - 1);
        self.col_pos = 0;
    }

    fn putc(&mut self, c: u8) {
        let buf = unsafe { &mut *(0xb8000 as *mut Buffer) };
        match c {
            b'\n' => self.new_line(),
            b'\r' => {
                self.col_pos = 0;
            }
            _ => {
                buf.chars[BUFFER_HEIGHT - 1][self.col_pos].write(ScreenChar {
                    ascii_character: c,
                    color_code: self.cur_color,
                });
                self.col_pos += 1;
                if self.col_pos == BUFFER_WIDTH {
                    self.new_line()
                }
            }
        }
    }

    fn write_bytes(&mut self, s: &[u8]) {
        for &b in s.iter() {
            self.putc(b);
        }
    }

    pub fn clear(&mut self) {
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: self.cur_color,
        };
        let buf = unsafe { &mut *(0xb8000 as *mut Buffer) };
        buf.chars.fill([Voliate { val: blank }; BUFFER_WIDTH])
    }
}

impl fmt::Write for VgaBuffer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_bytes(s.as_bytes());
        Ok(())
    }
}

static mut VGA_BUFFER: VgaBuffer = VgaBuffer {
    col_pos: 0,
    cur_color: ColorCode(0x0f),
};

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::io::_vga_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

pub fn vga_out() -> &'static mut VgaBuffer {
    unsafe { &mut VGA_BUFFER }
}

#[doc(hidden)]
pub fn _vga_print(args: fmt::Arguments) {
    use core::fmt::Write;
    vga_out().write_fmt(args).unwrap();
}

use bitflags::bitflags;

macro_rules! wait_for {
    ($cond:expr) => {
        while !$cond {
            core::hint::spin_loop()
        }
    };
}

bitflags! {
    /// Interrupt enable flags
    struct IntEnFlags: u8 {
        const RECEIVED = 1;
        const SENT = 1 << 1;
        const ERRORED = 1 << 2;
        const STATUS_CHANGE = 1 << 3;
        // 4 to 7 are unused
    }
}

bitflags! {
    /// Line status flags
    struct LineStsFlags: u8 {
        const INPUT_FULL = 1;
        // 1 to 4 unknown
        const OUTPUT_EMPTY = 1 << 5;
        // 6 and 7 unknown
    }
}

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
        unsafe {
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
    }

    fn line_sts(&mut self) -> LineStsFlags {
        unsafe { LineStsFlags::from_bits_truncate(inb(self.port_line_sts())) }
    }

    /// Sends a byte on the serial port.
    pub fn send(&mut self, data: u8) {
        unsafe {
            match data {
                8 | 0x7F => {
                    wait_for!(self.line_sts().contains(LineStsFlags::OUTPUT_EMPTY));
                    outb(self.port_data(), 8);
                    wait_for!(self.line_sts().contains(LineStsFlags::OUTPUT_EMPTY));
                    outb(self.port_data(), b' ');
                    wait_for!(self.line_sts().contains(LineStsFlags::OUTPUT_EMPTY));
                    outb(self.port_data(), 8);
                }
                _ => {
                    wait_for!(self.line_sts().contains(LineStsFlags::OUTPUT_EMPTY));
                    outb(self.port_data(), data);
                }
            }
        }
    }

    /// Sends a raw byte on the serial port, intended for binary data.
    pub fn send_raw(&mut self, data: u8) {
        unsafe {
            wait_for!(self.line_sts().contains(LineStsFlags::OUTPUT_EMPTY));
            outb(self.port_data(), data);
        }
    }

    /// Receives a byte on the serial port.
    pub fn receive(&mut self) -> u8 {
        unsafe {
            wait_for!(self.line_sts().contains(LineStsFlags::INPUT_FULL));
            inb(self.port_data())
        }
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

static mut SERIAL_IO: SerialPort = SerialPort(0x3f8);

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::io::_serial_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => ($crate::serial_print!("{}\n", format_args!($($arg)*)));
}

pub fn serial_out() -> &'static mut SerialPort {
    unsafe { &mut SERIAL_IO }
}

#[doc(hidden)]
pub fn _serial_print(args: fmt::Arguments) {
    use core::fmt::Write;
    serial_out().write_fmt(args).unwrap();
}
