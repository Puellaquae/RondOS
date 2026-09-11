//! Framebuffer console and keyboard test programs (P3).

use crate::io::{fb, font, input};
use crate::serial_println;

use super::{Case, Verdict};

pub static CASES: &[Case] = &[
    Case { name: "fb-console", run: fb_console },
    Case { name: "kbd-map", run: kbd_map },
    Case { name: "input-queue", run: input_queue },
];

fn fb_console() -> Verdict {
    if !fb::is_ready() {
        serial_println!("fb-console: console not initialised");
        return Verdict::Fail;
    }
    let mut ok = true;

    // Text goes into the shadow buffer.
    fb::clear();
    fb::write(b"FB-CONSOLE-MARKER\n");
    ok &= fb::contains("FB-CONSOLE-MARKER");

    // The glyph really reaches the framebuffer: compare every pixel of 'A'
    // against the font bitmap.
    fb::clear();
    fb::write(b"A");
    let (fg, bg) = (fb::fg_color(), fb::bg_color());
    let glyph = &font::FONT[(b'A' - font::FONT_FIRST) as usize];
    for (row, bits) in glyph.iter().enumerate() {
        for dx in 0..font::FONT_WIDTH {
            let on = (bits >> (7 - dx)) & 1 != 0;
            let got = fb::read_pixel(dx as u32, row as u32);
            let want = if on { fg } else { bg };
            if got != want {
                serial_println!("fb-console: pixel ({},{}) = {:#x} want {:#x}", dx, row, got, want);
                ok = false;
            }
        }
    }

    // Scrolling.  Write rows+2 numbered lines: exactly two must have scrolled
    // off, the last written line sits one row above the bottom (a trailing
    // newline leaves the cursor on the last row).
    fb::clear();
    let lines = fb::rows() + 2;
    for i in 0..lines {
        let line = [
            b'L',
            b'0' + ((i / 10) % 10) as u8,
            b'0' + (i % 10) as u8,
            b'\n',
        ];
        fb::write(&line);
    }
    // Copy before comparing: printing would scroll the very buffer we read.
    // Every line ends with '\n', so after the last one the cursor sits on the
    // bottom row and `lines - (rows - 1)` lines have scrolled off.
    let (first, first_len) = fb::row_copy(0);
    let (last, last_len) = fb::row_copy(fb::rows() as usize - 2);
    let (_bottom, bottom_len) = fb::row_copy(fb::rows() as usize - 1);
    let expect_first = {
        let f = lines - (fb::rows() - 1);
        [b'L', b'0' + ((f / 10) % 10) as u8, b'0' + (f % 10) as u8]
    };
    let expect_last = {
        let l = lines - 1;
        [b'L', b'0' + ((l / 10) % 10) as u8, b'0' + (l % 10) as u8]
    };
    if first[..first_len] != expect_first || last[..last_len] != expect_last {
        serial_println!(
            "fb-console: scroll wrong: first {:?} want {:?}, last {:?} want {:?}",
            &first[..first_len],
            expect_first,
            &last[..last_len],
            expect_last
        );
        ok = false;
    }
    // The bottom row must be blank: the trailing newline scrolled it away.
    ok &= bottom_len == 0;

    fb::clear();
    if ok {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}

fn kbd_map() -> Verdict {
    let mut ok = true;
    ok &= input::map_key(0x1e, false, false) == Some(b'a');
    ok &= input::map_key(0x1e, true, false) == Some(b'A');
    ok &= input::map_key(0x1e, false, true) == Some(b'A');
    ok &= input::map_key(0x1e, true, true) == Some(b'a'); // shift wins
    ok &= input::map_key(0x02, false, false) == Some(b'1');
    ok &= input::map_key(0x02, true, false) == Some(b'!');
    ok &= input::map_key(0x1c, false, false) == Some(b'\n');
    ok &= input::map_key(0x0e, false, false) == Some(0x08);
    ok &= input::map_key(0x39, false, false) == Some(b' ');
    ok &= input::map_key(0x2a, false, false).is_none(); // shift itself
    if !ok {
        serial_println!("kbd-map: scancode translation mismatch");
    }
    if ok {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}

fn input_queue() -> Verdict {
    let mut ok = true;
    // Raw injection is what the shell test uses.
    input::inject(b"hi\n");
    let mut buf = [0u8; 8];
    let n = input::read(&mut buf);
    ok &= &buf[..n] == b"hi\n";
    ok &= input::is_empty();

    // The IRQ1 path: make/release, caps, and an extended arrow sequence.
    input::handle_scancode(0x1e); // 'a' down
    input::handle_scancode(0x9e); // 'a' up (ignored)
    input::handle_scancode(0x1d); // ctrl down
    input::handle_scancode(0x2e); // 'c'
    input::handle_scancode(0x9d); // ctrl up
    input::handle_scancode(0xe0); // extended prefix
    input::handle_scancode(0x48); // up arrow
    let n = input::read(&mut buf);
    ok &= &buf[..n] == b"a\x03\x1b[A";
    if !ok {
        serial_println!("input-queue: got {:?}", core::str::from_utf8(&buf[..n]));
    }
    if ok {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}
