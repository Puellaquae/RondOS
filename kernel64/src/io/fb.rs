//! Framebuffer text console — P3.
//!
//! On the target machine there is no serial port (the design's decision: the
//! fb console is the *only* debug channel), so the kernel mirrors every log
//! line here as well as to COM1.  User space reaches the same surface through
//! the console device handle (`sys_write` on `Device { node: 0 }`).
//!
//! The console is deliberately simple: an 8×16 bitmap font from
//! [`super::font`], a shadow text buffer (which also makes the console
//! testable without a display), a software cursor and line scrolling.  A real
//! compositor is user space's job (design §9.2) — this is the kernel's own
//! fallback surface, and it owns exactly one rectangle: the whole screen.

#![allow(dead_code)]

use core::cell::UnsafeCell;

use crate::arch::x86_64::paging::X86_64Paging;
use crate::bootinfo::FramebufferInfo;
use crate::mm::vm::{CachePolicy, PagingArch};

use super::font::{FONT, FONT_FIRST, FONT_HEIGHT, FONT_LAST, FONT_WIDTH};

/// Kernel VA window for device mappings (design §3).
pub const DEVICE_BASE: usize = 0xFFFF_A000_0000_0000;

/// Largest console we keep a shadow buffer for (120×60 cells).
const MAX_COLS: usize = 120;
const MAX_ROWS: usize = 60;

const COLOR_FG: u32 = 0x00C0_C0C0; // light grey on ...
const COLOR_BG: u32 = 0x0000_0010; // ... near-black, DOS-ish
const CURSOR_H: u32 = 2;

struct Console {
    ready: bool,
    base: *mut u8,
    width: u32,
    height: u32,
    pitch: u32,
    bpp: u8,
    /// 1 = BGRx (GOP's usual BGRA), 2 = RGBx.
    format: u8,
    cols: u32,
    rows: u32,
    cx: u32,
    cy: u32,
    fg: u32,
    bg: u32,
    text: [[u8; MAX_COLS]; MAX_ROWS],
}

unsafe impl Sync for Console {}

impl Console {
    const fn new() -> Self {
        Self {
            ready: false,
            base: core::ptr::null_mut(),
            width: 0,
            height: 0,
            pitch: 0,
            bpp: 0,
            format: 1,
            cols: 0,
            rows: 0,
            cx: 0,
            cy: 0,
            fg: COLOR_FG,
            bg: COLOR_BG,
            text: [[b' '; MAX_COLS]; MAX_ROWS],
        }
    }
}

#[repr(transparent)]
struct ConsoleCell(UnsafeCell<Console>);

unsafe impl Sync for ConsoleCell {}

static CONSOLE: ConsoleCell = ConsoleCell(UnsafeCell::new(Console::new()));

fn con() -> &'static mut Console {
    unsafe { &mut *CONSOLE.0.get() }
}

pub fn is_ready() -> bool {
    con().ready
}

/// Map the GOP framebuffer into the kernel's device window and clear it.
///
/// Called once the kernel runs on its own page tables (the loader's tables are
/// gone by then).  A framebuffer that is too large for the device window, or a
/// pixel format we cannot draw, leaves the console disabled — logging keeps
/// working on the serial port.
pub fn init(info: &FramebufferInfo) {
    if info.phys == 0 || info.width == 0 || info.height == 0 || info.pitch == 0 {
        crate::serial_println!("fb: no framebuffer in BootInfo, console disabled");
        return;
    }
    if info.bpp != 32 && info.bpp != 24 {
        crate::serial_println!("fb: unsupported {}bpp, console disabled", info.bpp);
        return;
    }

    let len = (info.pitch as usize) * (info.height as usize);
    let root = X86_64Paging::active_root();
    if X86_64Paging::map_device(
        root,
        DEVICE_BASE,
        info.phys as usize,
        len,
        CachePolicy::WriteCombining,
    )
    .is_err()
    {
        crate::serial_println!("fb: cannot map {} bytes at {:#x}", len, DEVICE_BASE);
        return;
    }

    let c = con();
    *c = Console::new();
    c.base = DEVICE_BASE as *mut u8;
    c.width = info.width;
    c.height = info.height;
    c.pitch = info.pitch;
    c.bpp = info.bpp;
    c.format = info.format;
    c.cols = (info.width / FONT_WIDTH as u32).min(MAX_COLS as u32);
    c.rows = (info.height / FONT_HEIGHT as u32).min(MAX_ROWS as u32);
    c.ready = c.cols > 0 && c.rows > 0;
    if !c.ready {
        return;
    }
    clear();
    crate::serial_println!(
        "fb: console {}x{} cells ({}x{} {}bpp, pitch {}, {:#x})",
        c.cols,
        c.rows,
        info.width,
        info.height,
        info.bpp,
        info.pitch,
        info.phys
    );
}

#[inline]
fn pack(c: &Console, rgb: u32) -> u32 {
    let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
    match c.format {
        2 => (b << 16) | (g << 8) | r, // RGBx: memory order R,G,B,X
        _ => (r << 16) | (g << 8) | b, // BGRx: memory order B,G,R,X
    }
}

#[inline]
unsafe fn put_pixel(c: &Console, x: u32, y: u32, rgb: u32) {
    if x >= c.width || y >= c.height {
        return;
    }
    let p = c.base.add((y * c.pitch + x * (c.bpp as u32 / 8)) as usize);
    match c.bpp {
        32 => core::ptr::write_volatile(p as *mut u32, pack(c, rgb)),
        24 => {
            let v = pack(c, rgb);
            core::ptr::write_volatile(p, (v & 0xff) as u8);
            core::ptr::write_volatile(p.add(1), ((v >> 8) & 0xff) as u8);
            core::ptr::write_volatile(p.add(2), ((v >> 16) & 0xff) as u8);
        }
        _ => {}
    }
}

unsafe fn fill_rect(c: &Console, x: u32, y: u32, w: u32, h: u32, rgb: u32) {
    for yy in y..(y + h).min(c.height) {
        for xx in x..(x + w).min(c.width) {
            put_pixel(c, xx, yy, rgb);
        }
    }
}

/// Render one glyph cell (the shadow buffer is updated by the caller).
unsafe fn draw_cell(c: &Console, col: u32, row: u32, ch: u8, fg: u32, bg: u32) {
    let x0 = col * FONT_WIDTH as u32;
    let y0 = row * FONT_HEIGHT as u32;
    let glyph: &[u8] = if (FONT_FIRST..=FONT_LAST).contains(&ch) {
        &FONT[(ch - FONT_FIRST) as usize]
    } else {
        &FONT[(b'?' - FONT_FIRST) as usize]
    };
    for (dy, bits) in glyph.iter().enumerate() {
        for dx in 0..FONT_WIDTH as u32 {
            let on = (bits >> (7 - dx)) & 1 != 0;
            put_pixel(c, x0 + dx, y0 + dy as u32, if on { fg } else { bg });
        }
    }
}

unsafe fn draw_cursor(c: &Console) {
    let x0 = c.cx * FONT_WIDTH as u32;
    let y0 = c.cy * FONT_HEIGHT as u32 + FONT_HEIGHT as u32 - CURSOR_H;
    fill_rect(c, x0, y0, FONT_WIDTH as u32, CURSOR_H, c.fg);
}

unsafe fn erase_cursor(c: &Console) {
    let x0 = c.cx * FONT_WIDTH as u32;
    let y0 = c.cy * FONT_HEIGHT as u32 + FONT_HEIGHT as u32 - CURSOR_H;
    fill_rect(c, x0, y0, FONT_WIDTH as u32, CURSOR_H, c.bg);
}

pub fn clear() {
    let c = con();
    if !c.ready {
        return;
    }
    unsafe { fill_rect(c, 0, 0, c.width, c.height, c.bg) };
    for row in c.text.iter_mut() {
        for cell in row.iter_mut() {
            *cell = b' ';
        }
    }
    c.cx = 0;
    c.cy = 0;
    unsafe { draw_cursor(c) };
}

pub fn set_color(fg: u32, bg: u32) {
    let c = con();
    c.fg = fg;
    c.bg = bg;
}

fn scroll() {
    let c = con();
    if !c.ready || c.rows == 0 {
        return;
    }
    unsafe {
        // Move every pixel row up by one text line, one scanline at a time.
        // (Copying a whole text line per step would overlap itself, and
        // write-combined memory makes bulk reads expensive.)
        let total_rows = c.rows * FONT_HEIGHT as u32;
        for y in FONT_HEIGHT as u32..total_rows {
            let src = c.base.add(y as usize * c.pitch as usize);
            let dst = c.base.add((y - FONT_HEIGHT as u32) as usize * c.pitch as usize);
            core::ptr::copy_nonoverlapping(src, dst, c.pitch as usize);
        }
        // Blank the freed last line.
        fill_rect(
            c,
            0,
            (c.rows - 1) * FONT_HEIGHT as u32,
            c.width,
            FONT_HEIGHT as u32,
            c.bg,
        );
    }
    for row in 0..(c.rows as usize - 1) {
        c.text[row] = c.text[row + 1];
    }
    let last = c.rows as usize - 1;
    for cell in c.text[last].iter_mut() {
        *cell = b' ';
    }
}

fn newline() {
    let c = con();
    c.cx = 0;
    if c.cy + 1 >= c.rows {
        scroll();
        c.cy = c.rows - 1;
    } else {
        c.cy += 1;
    }
}

fn put_char(ch: u8) {
    let c = con();
    if !c.ready {
        return;
    }
    unsafe { erase_cursor(c) };
    match ch {
        b'\n' => newline(),
        b'\r' => c.cx = 0,
        0x0c => {
            // Form feed: the shell's `clear`.
            clear();
            return;
        }
        b'\t' => {
            let next = (c.cx / 8 + 1) * 8;
            while c.cx < next && c.cx < c.cols {
                put_char(b' ');
            }
        }
        0x08 => {
            // Backspace: step back and blank the cell.
            if c.cx > 0 {
                c.cx -= 1;
            }
            let (cx, cy) = (c.cx as usize, c.cy as usize);
            c.text[cy][cx] = b' ';
            unsafe { draw_cell(c, c.cx, c.cy, b' ', c.fg, c.bg) };
        }
        _ => {
            if c.cx >= c.cols {
                newline();
            }
            let (cx, cy) = (c.cx as usize, c.cy as usize);
            c.text[cy][cx] = ch;
            unsafe { draw_cell(c, c.cx, c.cy, ch, c.fg, c.bg) };
            c.cx += 1;
            if c.cx >= c.cols {
                newline();
            }
        }
    }
    unsafe { draw_cursor(c) };
}

/// Write raw bytes; `\n` moves to the next line, `\r` to column 0.
pub fn write(bytes: &[u8]) {
    if !is_ready() {
        return;
    }
    for &b in bytes {
        put_char(b);
    }
}

/// `core::fmt::Write` adapter so the kernel log can be mirrored here.
pub struct Writer;

impl core::fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        write(s.as_bytes());
        Ok(())
    }
}

/// Does the shadow buffer contain `needle`?  Used by the console tests and by
/// user-space screen scrapers; scanning text is far cheaper than pixels.
pub fn contains(needle: &str) -> bool {
    let c = con();
    if !c.ready {
        return false;
    }
    let n = needle.as_bytes();
    if n.is_empty() {
        return true;
    }
    let cols = c.cols as usize;
    let rows = c.rows as usize;
    let mut matched = 0usize;
    for row in 0..rows {
        for col in 0..cols {
            let ch = c.text[row][col];
            if ch == n[matched] {
                matched += 1;
                if matched == n.len() {
                    return true;
                }
            } else {
                // Restart, but allow the current byte to start a new match.
                matched = if ch == n[0] { 1 } else { 0 };
            }
        }
        // A row boundary is a line break in the shadow buffer.
        matched = 0;
    }
    false
}

/// One text row as bytes (trailing blanks trimmed).
///
/// The slice aliases the live shadow buffer: anything that writes to the
/// console — including the log line you are about to print — changes it.  Use
/// [`row_copy`] when the value must survive further output.
pub fn row(row: usize) -> &'static [u8] {
    let c = con();
    if !c.ready || row >= c.rows as usize {
        return &[];
    }
    let end = c.text[row]
        .iter()
        .rposition(|&b| b != b' ')
        .map(|i| i + 1)
        .unwrap_or(0);
    &c.text[row][..end]
}

/// Read a pixel back (tests, screen scrapers).  The mapping is write-combined;
/// reads are slow but correct.
pub fn read_pixel(x: u32, y: u32) -> u32 {
    let c = con();
    if !c.ready || x >= c.width || y >= c.height {
        return 0;
    }
    unsafe {
        let p = c.base.add((y * c.pitch + x * (c.bpp as u32 / 8)) as usize);
        match c.bpp {
            32 => core::ptr::read_volatile(p as *const u32),
            24 => {
                let b0 = core::ptr::read_volatile(p) as u32;
                let b1 = core::ptr::read_volatile(p.add(1)) as u32;
                let b2 = core::ptr::read_volatile(p.add(2)) as u32;
                b0 | (b1 << 8) | (b2 << 16)
            }
            _ => 0,
        }
    }
}

pub fn fg_color() -> u32 {
    con().fg
}

pub fn bg_color() -> u32 {
    con().bg
}

/// Copy of one text row (trailing blanks trimmed) and its length.
pub fn row_copy(row: usize) -> ([u8; MAX_COLS], usize) {
    let c = con();
    let mut out = [0u8; MAX_COLS];
    if !c.ready || row >= c.rows as usize {
        return (out, 0);
    }
    let end = c.text[row]
        .iter()
        .rposition(|&b| b != b' ')
        .map(|i| i + 1)
        .unwrap_or(0);
    out[..end].copy_from_slice(&c.text[row][..end]);
    (out, end)
}

pub fn cols() -> u32 {
    con().cols
}

pub fn rows() -> u32 {
    con().rows
}
