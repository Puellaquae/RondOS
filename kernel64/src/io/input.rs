//! PS/2 keyboard driver and the input queue — P3.
//!
//! The target machine has a PS/2 keyboard and no PS/2 mouse (design §13.7), so
//! this is the only input device v1 needs.  IRQ1 reads scancode set 1 from port
//! 0x60, translates it to bytes and pushes them into a ring buffer that user
//! space drains through the keyboard device handle (`sys_read` on
//! `Device { node: 1 }`).
//!
//! Arrow keys become the usual ANSI escape sequences (`ESC [ A`..) so a shell
//! can do line editing without a second event ABI.  `inject()` exists for the
//! tests: the harness types into the same queue the driver feeds, which makes
//! the shell testable in a headless boot.

#![allow(dead_code)]

use core::cell::UnsafeCell;

use crate::arch::x86_64::inb;

const PS2_DATA: u16 = 0x60;
const BUF_LEN: usize = 256;

struct Input {
    buf: [u8; BUF_LEN],
    head: usize,
    tail: usize,
    shift: bool,
    caps: bool,
    ctrl: bool,
    /// Set after an `0xE0` prefix (extended key).
    extended: bool,
}

impl Input {
    const fn new() -> Self {
        Self {
            buf: [0; BUF_LEN],
            head: 0,
            tail: 0,
            shift: false,
            caps: false,
            ctrl: false,
            extended: false,
        }
    }

    fn push(&mut self, b: u8) -> bool {
        let next = (self.tail + 1) % BUF_LEN;
        if next == self.head {
            return false; // full: drop the newest byte
        }
        self.buf[self.tail] = b;
        self.tail = next;
        true
    }

    fn pop(&mut self) -> Option<u8> {
        if self.head == self.tail {
            return None;
        }
        let b = self.buf[self.head];
        self.head = (self.head + 1) % BUF_LEN;
        Some(b)
    }
}

#[repr(transparent)]
struct InputCell(UnsafeCell<Input>);

unsafe impl Sync for InputCell {}

static INPUT: InputCell = InputCell(UnsafeCell::new(Input::new()));

fn input() -> &'static mut Input {
    unsafe { &mut *INPUT.0.get() }
}

pub fn init() {
    let i = input();
    i.head = 0;
    i.tail = 0;
    i.extended = false;
    // Drain anything the firmware left in the controller.
    while inb(0x64) & 1 != 0 {
        let _ = inb(PS2_DATA);
    }
}

/// Unshifted / shifted ASCII for scancode set 1 make codes (`0` = no character).
static UNSHIFTED: [u8; 64] = [
    0x00, 0x1b, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x30, 0x2d, 0x3d, 0x08, 0x09,
    0x71, 0x77, 0x65, 0x72, 0x74, 0x79, 0x75, 0x69, 0x6f, 0x70, 0x5b, 0x5d, 0x0a, 0x00, 0x61, 0x73,
    0x64, 0x66, 0x67, 0x68, 0x6a, 0x6b, 0x6c, 0x3b, 0x27, 0x60, 0x00, 0x5c, 0x7a, 0x78, 0x63, 0x76,
    0x62, 0x6e, 0x6d, 0x2c, 0x2e, 0x2f, 0x00, 0x2a, 0x00, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

static SHIFTED: [u8; 64] = [
    0x00, 0x1b, 0x21, 0x40, 0x23, 0x24, 0x25, 0x5e, 0x26, 0x2a, 0x28, 0x29, 0x5f, 0x2b, 0x08, 0x09,
    0x51, 0x57, 0x45, 0x52, 0x54, 0x59, 0x55, 0x49, 0x4f, 0x50, 0x7b, 0x7d, 0x0a, 0x00, 0x41, 0x53,
    0x44, 0x46, 0x47, 0x48, 0x4a, 0x4b, 0x4c, 0x3a, 0x22, 0x7e, 0x00, 0x7c, 0x5a, 0x58, 0x43, 0x56,
    0x42, 0x4e, 0x4d, 0x3c, 0x3e, 0x3f, 0x00, 0x2a, 0x00, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Translate one make code.  `caps` only affects letters (shift still wins).
pub fn map_key(make: u8, shift: bool, caps: bool) -> Option<u8> {
    let idx = make as usize;
    if make as usize >= UNSHIFTED.len() {
        return None;
    }
    let plain = UNSHIFTED[idx];
    if plain == 0 {
        return None;
    }
    if plain.is_ascii_alphabetic() {
        let upper = shift ^ caps;
        return Some(if upper {
            plain.to_ascii_uppercase()
        } else {
            plain
        });
    }
    Some(if shift { SHIFTED[idx] } else { plain })
}

fn push_arrow(code: u8) {
    let seq: &[u8] = match code {
        0x48 => b"\x1b[A", // up
        0x50 => b"\x1b[B", // down
        0x4d => b"\x1b[C", // right
        0x4b => b"\x1b[D", // left
        0x47 => b"\x1b[H", // home
        0x4f => b"\x1b[F", // end
        0x53 => b"\x1b[3~", // delete
        _ => return,
    };
    for &b in seq {
        input().push(b);
    }
}

/// IRQ1: one scancode from the controller.
pub fn handle_scancode(sc: u8) {
    let i = input();

    if sc == 0xe0 {
        i.extended = true;
        return;
    }
    let extended = i.extended;
    i.extended = false;

    let released = sc & 0x80 != 0;
    let make = sc & 0x7f;

    // Modifier keys track their own state, press and release.
    match make {
        0x2a | 0x36 => {
            i.shift = !released;
            return;
        }
        0x1d => {
            i.ctrl = !released;
            return;
        }
        0x3a => {
            if !released {
                i.caps = !i.caps;
            }
            return;
        }
        _ => {}
    }
    if released {
        return;
    }
    if extended {
        push_arrow(make);
        return;
    }

    // Ctrl+letter -> control character (Ctrl+C = 0x03, Ctrl+D = 0x04, ...).
    if i.ctrl {
        if let Some(c) = map_key(make, false, false) {
            if c.is_ascii_alphabetic() {
                i.push(c.to_ascii_lowercase() - b'a' + 1);
                return;
            }
        }
    }
    if let Some(c) = map_key(make, i.shift, i.caps) {
        i.push(c);
    }
}

/// Queue raw bytes as if they had been typed (tests, future paste support).
pub fn inject(bytes: &[u8]) {
    for &b in bytes {
        input().push(b);
    }
}

pub fn pop() -> Option<u8> {
    input().pop()
}

/// Drain up to `dst.len()` queued bytes; returns how many were copied.
pub fn read(dst: &mut [u8]) -> usize {
    let i = input();
    let mut n = 0;
    while n < dst.len() {
        match i.pop() {
            Some(b) => {
                dst[n] = b;
                n += 1;
            }
            None => break,
        }
    }
    n
}

pub fn is_empty() -> bool {
    let i = input();
    i.head == i.tail
}

pub fn pending() -> usize {
    let i = input();
    (i.tail + BUF_LEN - i.head) % BUF_LEN
}
