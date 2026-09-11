//! Boot progress painted straight onto the framebuffer — P3 diagnostics.
//!
//! On the target machine a reset wipes the screen and there is no serial port,
//! so the kernel paints its progress into the framebuffer *itself*, bypassing
//! the text console entirely: the console owns the text grid and clears it on
//! init, while these cells live in their own strip and are never touched again
//! by anyone.  A photo of the screen after a reset then shows the last stage the
//! kernel reached and which of the sub-marks it passed.
//!
//! Layout (screen coordinates, text console starts below):
//!
//! ```text
//!   ┌─────────── 16 stage cells ───────────┐
//!   │■ ■ ■ □ □ □ □ □ □ □ □ □ □ □ □ □        │  y = 4..14
//!   └──────────────────────────────────────┘
//!   [ 1 5 ]   <- last stage, in large-ish hex digits, y = 18..44
//! ```
//!
//! Everything here is best-effort: with no framebuffer in `BootInfo` the module
//! is inert, and a stage that is painted before the device window exists is
//! simply recorded and painted late.

#![allow(dead_code)]

use core::cell::UnsafeCell;

use crate::arch::x86_64::paging::X86_64Paging;
use crate::bootinfo;
use crate::io::fb::DEVICE_BASE;
use crate::mm::vm::{CachePolicy, PagingArch};

/// Height of the strip the display occupies; the text console is moved below it.
pub const STRIP_HEIGHT: u32 = 56;

const CELL: u32 = 10; // cell size in pixels
const GAP: u32 = 4;
const MARGIN: u32 = 4;
const CELLS: u32 = 16;

const COLOR_OFF: u32 = 0x0020_2028;
const COLOR_ON: u32 = 0x0040_c040;
const COLOR_DIM: u32 = 0x0070_7050;
const COLOR_LAST: u32 = 0x00e0_c040;
const COLOR_BG: u32 = 0x0000_0010;

/// 3x5 hex digits, one byte per column, low 5 bits used.
const GLYPHS: [[u8; 3]; 16] = [
    [0b111, 0b101, 0b111], // 0
    [0b001, 0b001, 0b001], // 1
    [0b111, 0b001, 0b111], // 2
    [0b111, 0b011, 0b111], // 3
    [0b101, 0b111, 0b001], // 4
    [0b111, 0b100, 0b111], // 5
    [0b111, 0b101, 0b111], // 6
    [0b111, 0b001, 0b001], // 7
    [0b111, 0b111, 0b111], // 8
    [0b111, 0b101, 0b111], // 9
    [0b111, 0b101, 0b101], // A
    [0b111, 0b110, 0b111], // b
    [0b100, 0b100, 0b111], // c
    [0b111, 0b011, 0b111], // d
    [0b111, 0b100, 0b111], // E
    [0b111, 0b100, 0b100], // F
];

struct Screen {
    base: *mut u8,
    width: u32,
    height: u32,
    pitch: u32,
    bpp: u8,
    format: u8,
    ready: bool,
    /// Stages recorded so far (bit N = stage N).
    mask: u32,
    /// Highest stage seen, for the big readout.
    last: u8,
    painted: bool,
}

impl Screen {
    const fn new() -> Self {
        Self {
            base: core::ptr::null_mut(),
            width: 0,
            height: 0,
            pitch: 0,
            bpp: 0,
            format: 1,
            ready: false,
            mask: 0,
            last: 0,
            painted: false,
        }
    }
}

#[repr(transparent)]
struct ScreenCell(UnsafeCell<Screen>);

unsafe impl Sync for ScreenCell {}

static SCREEN: ScreenCell = ScreenCell(UnsafeCell::new(Screen::new()));

fn scr() -> &'static mut Screen {
    unsafe { &mut *SCREEN.0.get() }
}

/// Map the framebuffer into the device window.  Call as early as `BootInfo` is
/// known; returns true when the display is usable.
pub fn init() -> bool {
    let info = bootinfo::get().fb;
    let s = scr();
    if bootinfo::get().fb_present == 0 || info.phys == 0 || info.width == 0 || info.height == 0 {
        return false;
    }
    if info.bpp != 32 && info.bpp != 24 {
        return false;
    }
    if s.ready {
        return true;
    }

    // Map the whole framebuffer with 4 KiB pages.  The strip readout is drawn
    // through this mapping; the text console later maps the same physical range
    // at the same virtual address, which is idempotent.
    let len = info.pitch as usize * info.height as usize;
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
        return false;
    }

    s.base = DEVICE_BASE as *mut u8;
    s.width = info.width;
    s.height = info.height;
    s.pitch = info.pitch;
    s.bpp = info.bpp;
    s.format = info.format;
    s.ready = true;
    paint();
    true
}

#[inline]
fn pack(s: &Screen, rgb: u32) -> u32 {
    let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
    match s.format {
        2 => (b << 16) | (g << 8) | r,
        _ => (r << 16) | (g << 8) | b,
    }
}

unsafe fn put(s: &Screen, x: u32, y: u32, rgb: u32) {
    if x >= s.width || y >= s.height {
        return;
    }
    let p = s.base.add((y * s.pitch + x * (s.bpp as u32 / 8)) as usize);
    match s.bpp {
        32 => core::ptr::write_volatile(p as *mut u32, pack(s, rgb)),
        24 => {
            let v = pack(s, rgb);
            core::ptr::write_volatile(p, (v & 0xff) as u8);
            core::ptr::write_volatile(p.add(1), ((v >> 8) & 0xff) as u8);
            core::ptr::write_volatile(p.add(2), ((v >> 16) & 0xff) as u8);
        }
        _ => {}
    }
}

unsafe fn rect(s: &Screen, x: u32, y: u32, w: u32, h: u32, rgb: u32) {
    let mut yy = y;
    while yy < y + h && yy < s.height {
        let mut xx = x;
        while xx < x + w && xx < s.width {
            put(s, xx, yy, rgb);
            xx += 1;
        }
        yy += 1;
    }
}

/// Redraw the whole strip from the current state.
fn paint() {
    let s = scr();
    if !s.ready {
        return;
    }
    unsafe {
        // Background.
        rect(s, 0, 0, s.width, STRIP_HEIGHT, COLOR_BG);
        // One cell per stage code 1..=16, drawn as a fixed 4x4 grid.
        let mut i = 0u32;
        while i < CELLS {
            let n = (i + 1) as u8;
            let col = i % 8;
            let row = i / 8;
            let x = MARGIN + col * (CELL + GAP);
            let y = MARGIN + row * (CELL + GAP);
            let color = if n == s.last {
                COLOR_LAST
            } else if s.mask & (1u32 << n) != 0 {
                COLOR_ON
            } else {
                COLOR_OFF
            };
            rect(s, x, y, CELL, CELL, color);
            i += 1;
        }
        // The last stage in hex, as two big glyphs.
        let hi = (s.last >> 4) & 0xf;
        let lo = s.last & 0xf;
        let scale = 5;
        let mut x = MARGIN;
        let y = MARGIN + 2 * (CELL + GAP) + 4;
        draw_glyph(s, x, y, hi, scale, COLOR_DIM);
        x += 3 * scale + scale;
        draw_glyph(s, x, y, lo, scale, COLOR_LAST);
    }
    s.painted = true;
}

unsafe fn draw_glyph(s: &Screen, x: u32, y: u32, d: u8, scale: u32, rgb: u32) {
    let g = GLYPHS[(d & 0xf) as usize];
    let mut col = 0;
    while col < 3 {
        let bits = g[col as usize];
        let mut row = 0;
        while row < 5 {
            if bits & (1 << row) != 0 {
                rect(s, x + col * scale, y + row * scale, scale, scale, rgb);
            }
            row += 1;
        }
        col += 1;
    }
}

/// Record that stage `n` was reached and repaint.
pub fn mark(n: u8) {
    let s = scr();
    if n == 0 || n > 31 {
        return;
    }
    s.mask |= 1u32 << n;
    // The display only has room for 1..=16; keep the coarse stages visible and
    // remember the highest code seen.
    if n <= 16 {
        s.last = n;
    }
    if s.ready {
        paint();
    } else {
        // Not mapped yet (very early boot): keep the state, paint on init.
        let _ = init();
    }
}

/// True once the framebuffer is mapped and the strip can be painted.
pub fn is_mapped() -> bool {
    scr().ready
}

/// Repaint from the recorded state (after something else wiped the screen).
pub fn repaint() {
    if scr().ready {
        paint();
    }
}

/// A distinct, always-painted marker used only around the scheduler bring-up,
/// so a photo distinguishes "died in `sti`" from "died earlier".
pub fn mark_big(n: u8) {
    mark(n);
}
