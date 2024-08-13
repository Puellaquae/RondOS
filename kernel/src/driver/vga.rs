use crate::arch::x86;
// https://files.osdev.org/mirrors/geezer/osd/graphics/modes.c
const VGA_AC_INDEX: u16 = 0x3C0;
const VGA_AC_WRITE: u16 = 0x3C0;
const VGA_AC_READ: u16 = 0x3C1;
const VGA_MISC_WRITE: u16 = 0x3C2;
const VGA_SEQ_INDEX: u16 = 0x3C4;
const VGA_SEQ_DATA: u16 = 0x3C5;
const VGA_DAC_READ_INDEX: u16 = 0x3C7;
const VGA_DAC_WRITE_INDEX: u16 = 0x3C8;
const VGA_DAC_DATA: u16 = 0x3C9;
const VGA_MISC_READ: u16 = 0x3CC;
const VGA_GC_INDEX: u16 = 0x3CE;
const VGA_GC_DATA: u16 = 0x3CF;
/*			COLOR emulation		MONO emulation */
const VGA_CRTC_INDEX: u16 = 0x3D4; /* 0x3B4 */
const VGA_CRTC_DATA: u16 = 0x3D5; /* 0x3B5 */
const VGA_INSTAT_READ: u16 = 0x3DA;

const VGA_NUM_SEQ_REGS: u8 = 5;
const VGA_NUM_CRTC_REGS: u8 = 25;
const VGA_NUM_GC_REGS: u8 = 9;
const VGA_NUM_AC_REGS: u8 = 21;
fn write_regs(regs: &mut [u8]) {
    let mut regi = 0;

    /* write MISCELLANEOUS reg */
    x86::outb(VGA_MISC_WRITE, regs[regi]);
    regi += 1;
    /* write SEQUENCER regs */
    for i in 0..VGA_NUM_SEQ_REGS {
        x86::outb(VGA_SEQ_INDEX, i);
        x86::outb(VGA_SEQ_DATA, regs[regi]);
        regi += 1;
    }
    /* unlock CRTC registers */
    x86::outb(VGA_CRTC_INDEX, 0x03);
    x86::outb(VGA_CRTC_DATA, x86::inb(VGA_CRTC_DATA) | 0x80);
    x86::outb(VGA_CRTC_INDEX, 0x11);
    x86::outb(VGA_CRTC_DATA, x86::inb(VGA_CRTC_DATA) & (!0x80));
    /* make sure they remain unlocked */
    regs[0x03] |= 0x80;
    regs[0x11] &= !0x80;
    /* write CRTC regs */
    for i in 0..VGA_NUM_CRTC_REGS {
        x86::outb(VGA_CRTC_INDEX, i);
        x86::outb(VGA_CRTC_DATA, regs[regi]);
        regi += 1;
    }
    /* write GRAPHICS CONTROLLER regs */
    for i in 0..VGA_NUM_GC_REGS {
        x86::outb(VGA_GC_INDEX, i);
        x86::outb(VGA_GC_DATA, regs[regi]);
        regi += 1;
    }
    /* write ATTRIBUTE CONTROLLER regs */
    for i in 0..VGA_NUM_AC_REGS {
        _ = x86::inb(VGA_INSTAT_READ);
        x86::outb(VGA_AC_INDEX, i);
        x86::outb(VGA_AC_WRITE, regs[regi]);
        regi += 1;
    }
    /* lock 16-color palette and unblank display */
    _ = x86::inb(VGA_INSTAT_READ);
    x86::outb(VGA_AC_INDEX, 0x20);
}

pub fn set_320x200x256g() {
    let mut g_320x200x256 = [
        /* MISC */
        0x63, /* SEQ */
        0x03, 0x01, 0x0F, 0x00, 0x0E, /* CRTC */
        0x5F, 0x4F, 0x50, 0x82, 0x54, 0x80, 0xBF, 0x1F, 0x00, 0x41, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x9C, 0x0E, 0x8F, 0x28, 0x40, 0x96, 0xB9, 0xA3, 0xFF, /* GC */
        0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x05, 0x0F, 0xFF, /* AC */
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
        0x0F, 0x41, 0x00, 0x0F, 0x00, 0x00,
    ];
    write_regs(&mut g_320x200x256);
}

pub fn set_80x50t() {
    let mut g_80x50_text = [
        /* MISC */
        0x67, /* SEQ */
        0x03, 0x00, 0x03, 0x00, 0x02, /* CRTC */
        0x5F, 0x4F, 0x50, 0x82, 0x55, 0x81, 0xBF, 0x1F, 0x00, 0x47, 0x06, 0x07, 0x00, 0x00, 0x01,
        0x40, 0x9C, 0x8E, 0x8F, 0x28, 0x1F, 0x96, 0xB9, 0xA3, 0xFF, /* GC */
        0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x0E, 0x00, 0xFF, /* AC */
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x14, 0x07, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E,
        0x3F, 0x0C, 0x00, 0x0F, 0x08, 0x00,
    ];
    write_regs(&mut g_80x50_text);
}

pub fn set_80x25t() {
    let mut g_80x25_text = [
        /* MISC */
        0x67, /* SEQ */
        0x03, 0x00, 0x03, 0x00, 0x02, /* CRTC */
        0x5F, 0x4F, 0x50, 0x82, 0x55, 0x81, 0xBF, 0x1F, 0x00, 0x4F, 0x0D, 0x0E, 0x00, 0x00, 0x00,
        0x50, 0x9C, 0x0E, 0x8F, 0x28, 0x1F, 0x96, 0xB9, 0xA3, 0xFF, /* GC */
        0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x0E, 0x00, 0xFF, /* AC */
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x14, 0x07, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E,
        0x3F, 0x0C, 0x00, 0x0F, 0x08, 0x00,
    ];
    write_regs(&mut g_80x25_text);
}

pub fn set_plane(p: u8) {
    let p = p & 3;
    let pmask = 1 << p;
    /* set read plane */
    x86::outb(VGA_GC_INDEX, 4);
    x86::outb(VGA_GC_DATA, p);
    /* set write plane */
    x86::outb(VGA_SEQ_INDEX, 2);
    x86::outb(VGA_SEQ_DATA, pmask);
}

pub fn get_fb_seg() -> u32 {
    x86::outb(VGA_GC_INDEX, 6);
    let seg = x86::inb(VGA_GC_DATA);
    match (seg >> 2) & 3 {
        0 | 1 => 0xa0000,
        2 => 0xb0000,
        3 => 0xb8000,
        _ => unreachable!(),
    }
}

fn vpokeb(off: u32, val: u8) {
    let addr = get_fb_seg() + off;
    unsafe { (addr as *mut u8).write(val) };
}

pub fn write_pixel8x(x: u32, y: u32, c: u8) {
    // let wd_in_bytes = 320 / 4;
    // let off = wd_in_bytes * y + x / 4;
    // set_plane((x & 3) as u8);
    vpokeb(y * 320 + x, c);
}
