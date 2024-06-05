#![allow(dead_code)]

use core::{
    fmt::{self},
    ptr::{read_volatile, write_volatile},
};

use crate::{
    utils::singleton::Singleton,
    x86::{inb, outb},
};

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

impl ScreenChar {
    fn new(ch: u8, color: ColorCode) -> ScreenChar {
        ScreenChar {
            ascii_character: ch,
            color_code: color,
        }
    }
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

const VGA_BUFFER_ADDR: u32 = 0xc00b8000;

fn enable_cursor(cursor_start: u8, cursor_end: u8) {
    outb(0x3D4, 0x0A);
    outb(0x3D5, (inb(0x3D5) & 0xC0) | cursor_start);

    outb(0x3D4, 0x0B);
    outb(0x3D5, (inb(0x3D5) & 0xE0) | cursor_end);
}

fn update_cursor(height: usize, width: usize) {
    let pos = height * BUFFER_WIDTH + width;

    outb(0x3D4, 0x0F);
    outb(0x3D5, (pos & 0xFF) as u8);
    outb(0x3D4, 0x0E);
    outb(0x3D5, ((pos >> 8) & 0xFF) as u8);
}

fn disable_cursor() {
    outb(0x3D4, 0x0A);
    outb(0x3D5, 0x20);
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
        let buf = unsafe { &mut *(VGA_BUFFER_ADDR as *mut Buffer) };
        for col in 0..BUFFER_WIDTH {
            buf.chars[row][col].write(blank);
        }
    }

    fn new_line(&mut self) {
        let buf = unsafe { &mut *(VGA_BUFFER_ADDR as *mut Buffer) };
        for row in 1..BUFFER_HEIGHT {
            buf.chars.copy_within(row..=row, row - 1);
        }
        self.clear_row(BUFFER_HEIGHT - 1);
        self.col_pos = 0;
        update_cursor(BUFFER_HEIGHT - 1, self.col_pos);
    }

    fn putc(&mut self, c: u8) {
        let buf = unsafe { &mut *(VGA_BUFFER_ADDR as *mut Buffer) };
        match c {
            b'\n' => self.new_line(),
            b'\r' => {
                self.col_pos = 0;
                update_cursor(BUFFER_HEIGHT - 1, self.col_pos);
            }
            // backspace
            0x08 => {
                if self.col_pos > 0 {
                    buf.chars[BUFFER_HEIGHT - 1][self.col_pos - 1]
                        .write(ScreenChar::new(b' ', self.cur_color));
                    self.col_pos -= 1;
                    update_cursor(BUFFER_HEIGHT - 1, self.col_pos);
                }
            }
            // del
            0x7f => {
                buf.chars[BUFFER_HEIGHT - 1]
                    .copy_within(self.col_pos + 1..BUFFER_WIDTH, self.col_pos);
                buf.chars[BUFFER_HEIGHT - 1][BUFFER_WIDTH - 1]
                    .write(ScreenChar::new(b' ', self.cur_color));
            }
            _ => {
                buf.chars[BUFFER_HEIGHT - 1][self.col_pos]
                    .write(ScreenChar::new(c, self.cur_color));
                if self.col_pos == BUFFER_WIDTH - 1 {
                    self.new_line()
                } else {
                    self.col_pos += 1;
                    update_cursor(BUFFER_HEIGHT - 1, self.col_pos);
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
        let blank = ScreenChar::new(b' ', self.cur_color);
        let buf = unsafe { &mut *(VGA_BUFFER_ADDR as *mut Buffer) };
        buf.chars.fill([Voliate { val: blank }; BUFFER_WIDTH])
    }
}

impl fmt::Write for VgaBuffer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_bytes(s.as_bytes());
        Ok(())
    }
}

impl Default for VgaBuffer {
    fn default() -> Self {
        Self::new()
    }
}

static VGA_BUFFER: Singleton<VgaBuffer> = Singleton::UNINIT;

#[doc(hidden)]
pub fn _vga_print(args: fmt::Arguments) {
    use core::fmt::Write;
    VGA_BUFFER.get_mut().write_fmt(args).unwrap();
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::io::vga::_vga_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}
