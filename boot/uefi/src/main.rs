//! RondOS UEFI boot stub — M0.7.
//!
//! Runs as an `x86_64-unknown-uefi` application on the firmware's ESP:
//!
//! 1. list the 32bpp GOP modes and let the user pick one (a `timeout` prompt:
//!    no key for 10 s selects the default, 1280x720);
//! 2. initialise a framebuffer text console (the firmware's SimpleTextOutput
//!    is no longer visible once a graphics mode is active) and print boot
//!    progress;
//! 3. read `\rondos\kernel.elf` and `\rondos\boot.tar` from the ESP;
//! 4. copy the kernel's `PT_LOAD` segments to their `p_paddr`;
//! 5. collect the UEFI memory map into the versioned `BootInfo`;
//! 6. build 4-level page tables (identity + physmap + kernel window);
//! 7. `ExitBootServices` and jump to the kernel with `rdi = &BootInfo`
//!    (physmap view) — there is no second "press a key" pause.
//!
//! Everything it hands over is described by `BootInfo`, whose layout mirrors
//! `kernel64/src/bootinfo.rs` field for field.

#![no_std]
#![no_main]

use core::arch::asm;
use core::fmt::Write;

use uefi::boot;
use uefi::mem::memory_map::{MemoryMap, MemoryType};
use uefi::prelude::*;
use uefi::proto::console::gop::{GraphicsOutput, Mode, PixelFormat};
use uefi::proto::console::text::{Input, Key};
use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode};

mod fb;

// ------------------------------------------------------------- console sink
//
// The loader's own output goes to the framebuffer *and* to COM1.  On a machine
// with no serial port only the framebuffer matters, but under test the serial
// log is the only way to read what the loader printed before the kernel took
// the screen over.  The UART is initialised unconditionally; a missing port
// simply drops the bytes.

const COM1: u16 = 0x3f8;

unsafe fn serial_init() {
    asm!("out dx, al", in("dx") COM1 + 1, in("al") 0x00u8, options(nomem, nostack));
    asm!("out dx, al", in("dx") COM1 + 3, in("al") 0x80u8, options(nomem, nostack));
    asm!("out dx, al", in("dx") COM1, in("al") 0x03u8, options(nomem, nostack));
    asm!("out dx, al", in("dx") COM1 + 1, in("al") 0x00u8, options(nomem, nostack));
    asm!("out dx, al", in("dx") COM1 + 3, in("al") 0x03u8, options(nomem, nostack));
    asm!("out dx, al", in("dx") COM1 + 2, in("al") 0xc7u8, options(nomem, nostack));
    asm!("out dx, al", in("dx") COM1 + 4, in("al") 0x0bu8, options(nomem, nostack));
}

fn serial_byte(b: u8) {
    if b == b'\n' {
        serial_byte_raw(b'\r');
    }
    serial_byte_raw(b);
}

fn serial_byte_raw(b: u8) {
    unsafe {
        // Bounded wait: never hang the loader on an absent or wedged UART.
        let mut spins = 0u32;
        loop {
            let st: u8;
            asm!("in al, dx", in("dx") COM1 + 5, out("al") st, options(nomem, nostack));
            if st & 0x20 != 0 || spins > 100_000 {
                break;
            }
            spins += 1;
        }
        asm!("out dx, al", in("dx") COM1, in("al") b, options(nomem, nostack));
    }
}

// ------------------------------------------------- hand-off interrupt state
//
// Nothing in the UEFI spec requires `ExitBootServices` to leave interrupts
// disabled, and the 8259 masks are simply whatever the firmware last wrote.
// That is a machine-dependent landmine: after the hand-off, the firmware's
// IDT and GDT stop being reachable the moment the kernel drops the loader's
// identity map, so a single leftover timer IRQ — or an exception raised while
// trying to deliver one — becomes a double fault and then a reset.  On a board
// whose firmware boots with the timer still unmasked, the symptom is exactly
// "the loader paints its hand-off colour and the screen instantly goes black".
//
// The kernel must not depend on the firmware here, so the loader clears IF and
// masks every line on both 8259s before the jump.  The kernel repeats the
// `cli` for good measure (see `_start`).

/// Mask all eight lines on the master and slave PICs.
fn mask_pic() {
    unsafe {
        asm!("out dx, al", in("dx") 0x21u16, in("al") 0xffu8, options(nomem, nostack));
        asm!("out dx, al", in("dx") 0xa1u16, in("al") 0xffu8, options(nomem, nostack));
    }
}

/// `cli` plus a fully masked PIC: no maskable interrupt can arrive until the
/// kernel installs its own IDT and unmasks what it wants.
fn quiesce_interrupts() {
    unsafe { asm!("cli", options(nomem, nostack)) };
    mask_pic();
}

fn interrupts_enabled() -> bool {
    let flags: u64;
    unsafe { asm!("pushfq", "pop {}", out(reg) flags, options(nomem, nostack)) };
    flags & (1 << 9) != 0
}

fn pic_mask() -> (u8, u8) {
    let (master, slave): (u8, u8);
    unsafe {
        asm!("in al, dx", in("dx") 0x21u16, out("al") master, options(nomem, nostack));
        asm!("in al, dx", in("dx") 0xa1u16, out("al") slave, options(nomem, nostack));
    }
    (master, slave)
}

/// Console sink: framebuffer + COM1.
struct Tee;

impl core::fmt::Write for Tee {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        core::fmt::Write::write_str(&mut fb::Writer, s)?;
        for b in s.bytes() {
            serial_byte(b);
        }
        Ok(())
    }
}

/// `writeln!` to both the framebuffer console and COM1.
macro_rules! log {
    () => {{
        let _ = writeln!(Tee);
    }};
    ($($arg:tt)*) => {{
        let _ = writeln!(Tee, $($arg)*);
    }};
}

// ------------------------------------------------------------------ BootInfo

const BOOTINFO_MAGIC: u32 = 0x524E_4431;
const BOOTINFO_VERSION: u32 = 2;
const MAX_MEM_ENTRIES: usize = 256;
const MAX_CMDLINE: usize = 128;
const BOOT_KIND_UEFI: u32 = 2;

/// Bumped by hand whenever the image changes; the loader prints it and the
/// kernel stores its own copy in the progress record, so a "still failing"
/// report can be matched against the image that produced it.
///
/// The *kernel* embeds the same string (`kernel64/src/boot.rs::BUILD_TAG`) and
/// the loader greps the loaded image for it, so a mismatched or truncated
/// `kernel.elf` is caught before it can be blamed for a hang.
const BUILD_ID: &str = "fix-2026-09-11g";

const PHYS_MAP_BASE: u64 = 0xFFFF_8000_0000_0000;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct StructHeader {
    magic: u32,
    size: u32,
    version: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct MemDesc {
    addr: u64,
    len: u64,
    kind: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FramebufferInfo {
    phys: u64,
    width: u32,
    height: u32,
    pitch: u32,
    bpp: u8,
    format: u8,
    _pad: [u8; 2],
}

#[repr(C)]
struct BootInfo {
    hdr: StructHeader,
    boot_kind: u32,
    mem_count: u32,
    physmap_base: u64,
    initrd_phys: u64,
    initrd_len: u64,
    acpi_rsdp: u64,
    fb_present: u32,
    _pad: u32,
    fb: FramebufferInfo,
    cmdline_len: u32,
    /// Address of the loader's cross-reset log (version 2).
    bootlog_phys: u32,
    /// Which boot wrote that log (0xff = loader, 1 = kernel).
    bootlog_stage: u32,
    /// Durable progress record: where the kernel keeps it, whether the previous
    /// boot left one, and what it said.
    progress_phys: u32,
    progress_prev: u32,
    progress_mask: u32,
    progress_mask2: u32,
    progress_stage: u32,
    progress_count: u32,
    _reserved: [u32; 2],
    cmdline: [u8; MAX_CMDLINE],
    mem: [MemDesc; MAX_MEM_ENTRIES],
}

impl BootInfo {
    const fn new() -> Self {
        Self {
            hdr: StructHeader {
                magic: BOOTINFO_MAGIC,
                size: core::mem::size_of::<BootInfo>() as u32,
                version: BOOTINFO_VERSION,
                _pad: 0,
            },
            boot_kind: BOOT_KIND_UEFI,
            mem_count: 0,
            physmap_base: PHYS_MAP_BASE,
            initrd_phys: 0,
            initrd_len: 0,
            acpi_rsdp: 0,
            fb_present: 0,
            _pad: 0,
            fb: FramebufferInfo {
                phys: 0,
                width: 0,
                height: 0,
                pitch: 0,
                bpp: 0,
                format: 0,
                _pad: [0; 2],
            },
            cmdline_len: 0,
            bootlog_phys: 0,
            bootlog_stage: 0,
            progress_phys: 0,
            progress_prev: 0,
            progress_mask: 0,
            progress_mask2: 0,
            progress_stage: 0,
            progress_count: 0,
            _reserved: [0; 2],
            cmdline: [0; MAX_CMDLINE],
            mem: [MemDesc {
                addr: 0,
                len: 0,
                kind: 0,
                _pad: 0,
            }; MAX_MEM_ENTRIES],
        }
    }

    fn set_cmdline(&mut self, s: &[u8]) {
        let n = s.len().min(MAX_CMDLINE);
        self.cmdline[..n].copy_from_slice(&s[..n]);
        self.cmdline_len = n as u32;
    }
}

// Static storage: everything here must survive ExitBootServices and is
// identity mapped by the page tables we install.
#[repr(align(4096))]
#[derive(Clone, Copy)]
struct Page([u64; 512]);

/// One page directory per GiB, used to build the identity/physmap windows with
/// **2 MiB** pages on CPUs that have no 1 GiB pages (Intel before ~2010).
///
/// Writing a PS=1 entry into a PDPT is a reserved-bit violation on such a CPU,
/// so the very first `mov cr3` would page-fault; delivering that fault re-walks
/// the same broken tables, which is a triple fault — i.e. the loader paints its
/// hand-off colour and the screen instantly goes black.  64 GiB is far beyond
/// anything a machine without 1 GiB pages can hold.
const PD_POOL_PAGES: usize = 64;
static mut PD_ID: [Page; PD_POOL_PAGES] = [Page([0; 512]); PD_POOL_PAGES];
static mut PD_PHYS: [Page; PD_POOL_PAGES] = [Page([0; 512]); PD_POOL_PAGES];
static mut BOOTINFO: BootInfo = BootInfo::new();
static mut PML4: Page = Page([0; 512]);
static mut PDPT_ID: Page = Page([0; 512]);
static mut PDPT_PHYS: Page = Page([0; 512]);
static mut PDPT_KERN: Page = Page([0; 512]);
static mut PD_KERN: Page = Page([0; 512]);
static mut STACK: [u8; 32 * 1024] = [0; 32 * 1024];

// ------------------------------------------------------------------ entry

#[entry]
fn main() -> Status {
    // COM1 mirrors everything below; the framebuffer is still the primary sink.
    unsafe { serial_init() };
    // Take the interrupt state over from the firmware from the very first line:
    // from here on nothing but our own code runs until the kernel owns the CPU.
    quiesce_interrupts();
    // 1. GOP: list the 32bpp modes, then let the user pick one.  The prompt is
    //    a *timeout* one: 1280x720 (or the closest mode the firmware offers)
    //    wins when no key arrives.
    let handle = match boot::get_handle_for_protocol::<GraphicsOutput>() {
        Ok(h) => h,
        Err(_) => return Status::UNSUPPORTED,
    };
    let mut gop = match boot::open_protocol_exclusive::<GraphicsOutput>(handle) {
        Ok(g) => g,
        Err(_) => return Status::UNSUPPORTED,
    };
    let (modes, mode_count) = collect_modes(&gop);
    if mode_count == 0 {
        log!("  error: no suitable GOP mode found");
        return Status::UNSUPPORTED;
    }
    let default_idx = closest_mode(&modes[..mode_count], DEFAULT_W, DEFAULT_H);
    let chosen = default_idx;
    // Set the default first so the menu below has a framebuffer to draw on.
    let (mut fb_base, mut fb_w, mut fb_h, mut fb_pitch, mut fb_format) =
        match apply_mode(&mut gop, modes[chosen].unwrap().mode) {
            Some(p) => p,
            None => {
                log!("  error: cannot set GOP mode");
                return Status::UNSUPPORTED;
            }
        };

    // Initialise the framebuffer text console.  After setting a GOP graphics
    // mode the firmware's SimpleTextOutput is no longer visible, so we render
    // text directly to the linear framebuffer.
    fb::init(fb_base, fb_w, fb_h, fb_pitch, fb_format);
    banner(fb_base, fb_w, fb_h);

    // Resolution prompt.  Drawn on our own console so the choice is visible on
    // a machine with no serial port; the bytes also go to COM1.
    log!("  resolutions (32bpp):");
    for i in 0..mode_count {
        let m = modes[i].unwrap();
        log!(
            "    [{}] {}x{}{}",
            i + 1,
            m.w,
            m.h,
            if i == default_idx { "  (default)" } else { "" }
        );
    }
    log!();
    let def = modes[default_idx].unwrap();
    if def.w != DEFAULT_W || def.h != DEFAULT_H {
        log!(
            "  note: {}x{} is not offered, using the closest mode above",
            DEFAULT_W,
            DEFAULT_H
        );
    }
    log!(
        "  choose 1-{} then Enter; no key for {} s takes the default",
        mode_count,
        RES_TIMEOUT_MS / 1000
    );
    let picked = choose_mode(mode_count, default_idx);
    if picked != chosen {
        match apply_mode(&mut gop, modes[picked].unwrap().mode) {
            Some(p) => {
                (fb_base, fb_w, fb_h, fb_pitch, fb_format) = p;
                fb::init(fb_base, fb_w, fb_h, fb_pitch, fb_format);
                banner(fb_base, fb_w, fb_h);
                log!("  resolution: {}x{} (user choice)", fb_w, fb_h);
            }
            None => {
                let m = modes[picked].unwrap();
                log!(
                    "  warning: {}x{} could not be set, staying at {}x{}",
                    m.w,
                    m.h,
                    fb_w,
                    fb_h
                );
            }
        }
    }
    drop(gop);
    log!();

    // 2. Kernel image and boot archive from the ESP.
    let kernel = match read_file(cstr16!("\\rondos\\kernel.elf")) {
        Some(v) => v,
        None => {
            log!("  error: kernel.elf not found");
            return Status::NOT_FOUND;
        }
    };
    log!("  kernel image: {} bytes", kernel.1);
    // The kernel embeds the same build tag in `.rodata`; 0 occurrences means
    // this is not the image this loader was built with.
    {
        let bytes =
            unsafe { core::slice::from_raw_parts(kernel.0 as *const u8, kernel.1 as usize) };
        let tag = BUILD_ID.as_bytes();
        let count = bytes.windows(tag.len()).filter(|w| *w == tag).count();
        log!("  kernel build tag: {} occurrence(s)", count);
        if count == 0 {
            log!("  ERROR: kernel.elf lacks tag {} (wrong image!)", BUILD_ID);
        }
    }
    let tar = read_file(cstr16!("\\rondos\\boot.tar"));

    // 3. Load the kernel's PT_LOAD segments.
    let (entry, kernel_phys_end) = match load_elf(kernel.0, kernel.1) {
        Some(e) => e,
        None => {
            log!("  error: failed to load kernel ELF");
            return Status::LOAD_ERROR;
        }
    };
    let _ = log!(
        "  kernel: entry 0x{:08x}, image ends 0x{:08x}",
        entry, kernel_phys_end
    );

    // 4. BootInfo: memory map, framebuffer, initrd.
    let bi = unsafe { &mut *core::ptr::addr_of_mut!(BOOTINFO) };
    if let Ok(map) = boot::memory_map(MemoryType::LOADER_DATA) {
        for d in map.entries() {
            if bi.mem_count as usize >= MAX_MEM_ENTRIES {
                log!("  warning: memory map truncated at {} entries", MAX_MEM_ENTRIES);
                break;
            }
            bi.mem[bi.mem_count as usize] = MemDesc {
                addr: d.phys_start,
                len: d.page_count * 4096,
                kind: if d.ty == MemoryType::CONVENTIONAL { 1 } else { 2 },
                _pad: 0,
            };
            bi.mem_count += 1;
        }
    }
    log!("  memory map: {} entries kept", bi.mem_count);
    bi.fb_present = 1;
    bi.fb = FramebufferInfo {
        phys: fb_base,
        width: fb_w,
        height: fb_h,
        pitch: fb_pitch,
        bpp: 32,
        format: fb_format,
        _pad: [0; 2],
    };
    if let Some((pa, len)) = tar {
        bi.initrd_phys = pa;
        bi.initrd_len = len;
        log!("  initrd: {} bytes @ 0x{:08x}", len, pa);
    }
    bi.set_cmdline(b"rondos.uefi=1");

    // 5. Page tables.
    //
    // The durable record first: it is read before the kernel can touch
    // anything, and this boot's own record is appended below.
    records_report();
    let (prev_stage, stage_mask, mask2, run_sig) = unsafe { report_previous_stage() };
    // The kernel's own record lives in DRAM; read it before the next kernel
    // starts overwriting it.
    let progress = progress_read(&bi);
    bi.progress_phys = progress_phys(&bi) as u32;
    bi.progress_prev = progress.is_some() as u32;
    if let Some(p) = &progress {
        bi.progress_mask = p.mask;
        bi.progress_mask2 = p.mask2;
        bi.progress_stage = p.stage;
        bi.progress_count = p.count;
        log!(
            "  kernel progress: stage {:02x} mask {:02x} {:02x} writes {} build {:#010x}",
            p.stage,
            p.mask & 0xff,
            p.mask2 & 0xff,
            p.count,
            p.build
        );
    } else {
        log!("  kernel progress: none in RAM");
    }
    // Prefer the kernel's own record: it is DRAM, so it survives a warm reset
    // even where firmware rewrites CMOS during POST.  Fall back to the CMOS
    // masks.  The "last stage" is *derived from the mask*: the 0x2E byte has
    // proven unreliable on the target board, while the mask consistently shows
    // how far the kernel got.
    let (stage, mask, count, sig, tsc_lo, tsc_hi) = match &progress {
        Some(p) => (
            highest_stage(p.mask, p.mask2),
            p.mask | (p.mask2 << 8),
            p.count,
            0x5a,
            p.tsc_lo,
            p.tsc_hi,
        ),
        None => (
            highest_stage(stage_mask as u32, mask2 as u32),
            (stage_mask as u32) | ((mask2 as u32) << 8),
            0,
            if run_sig == 0x5a { 0x5a } else { 0 },
            rdtsc() as u32,
            (rdtsc() >> 32) as u32,
        ),
    };
    let _ = prev_stage;
    let rec = BootRecord {
            boot: 0,
            stage,
            mask,
            count,
            sig,
            tsc_lo,
            tsc_hi,
            mem_count: 0,
            _pad: [0; 1],
            mem: [[0; 3]; REC_MEM_SHOWN],
        };
    records_save(record_mem(rec, &bi), run_sig as u32);
    let ram_end = ram_highest(&bi);
    let usable = {
        let mut u = 0u64;
        for i in 0..bi.mem_count as usize {
            let m = &bi.mem[i];
            if m.kind == 1 {
                u += m.len;
            }
        }
        u
    };
    log!(
        "  RAM top: 0x{:08x}  (usable {} MiB, {} entries)",
        ram_end,
        usable / 1024 / 1024,
        bi.mem_count
    );
    // First few map entries, so a board that reports its RAM oddly is visible
    // on the screen (photos beat hex dumps when there is no serial port).
    {
        let n = (bi.mem_count as usize).min(6);
        let mut i = 0;
        while i < n {
            let m = &bi.mem[i];
            log!(
                "    mem[{}] {:#012x} len {:#010x} kind {}",
                i,
                m.addr,
                m.len,
                m.kind
            );
            i += 1;
        }
    }
    // Everything the kernel may touch through the loader's identity map must be
    // inside it: its own image, `BootInfo`, the stack, the low-memory mailbox
    // **and the framebuffer**.  The GOP framebuffer is not part of the RAM the
    // firmware reports, so it can sit anywhere in MMIO space — on many boards
    // it is placed at or above 4 GiB.  If it falls outside the identity map,
    // the kernel's very first paint page-faults before it has an IDT, which is
    // a triple fault: the screen shows the loader's hand-off colour and then
    // goes black.  Size the identity map to cover it.
    let fb_end = fb_base.saturating_add((fb_pitch as u64).saturating_mul(fb_h as u64));
    let cr3 = build_page_tables(kernel_phys_end, ram_end, fb_end);
    log!("  page tables: CR3=0x{:08x}", cr3);

    log!();
    log!("  booting...");

    // 6. Hand over.
    let bi_pa = core::ptr::addr_of!(BOOTINFO) as u64;
    let stack_top = (core::ptr::addr_of!(STACK) as u64) + (32 * 1024) as u64;

    // `exit_boot_services` retries internally; the owned map it returns proves
    // it succeeded (it would otherwise have panicked/looped).
    let _map = unsafe { boot::exit_boot_services(Some(MemoryType::LOADER_DATA)) };
    log!("  exit_boot_services: ok");
    // Belt and braces: firmware is out of the picture now, so make sure it did
    // not hand the CPU back with a live interrupt source.
    quiesce_interrupts();
    {
        let (master, slave) = pic_mask();
        log!(
            "  handoff: interrupts {} PIC mask {:#04x}/{:#04x}",
            if interrupts_enabled() { "ENABLED" } else { "off" },
            master,
            slave
        );
    }

    // Paint the whole screen a colour nothing else uses.  It stays until the
    // kernel's first paint replaces it, so a photo distinguishes "the loader
    // reached the hand-off but the kernel never ran" (red) from "the kernel
    // started" (blue/green) without any logging.
    publish_mailbox(fb_base, fb_w, fb_h, fb_pitch, fb_format);
    paint_screen(fb_base, fb_w, fb_h, fb_pitch, fb_format, 0x00_00_80);

    unsafe { jump_to_kernel(entry, bi_pa, cr3, stack_top) }
}

/// Blocks until a key is pressed on the console, or until a timeout expires.
/// Must be called before `ExitBootServices`, since console input is only
/// available while boot services are active.
/// Fill the framebuffer with one colour, straight through the loader's own map.
fn paint_screen(
    fb_base: u64,
    fb_w: u32,
    fb_h: u32,
    fb_pitch: u32,
    format: u8,
    rgb: u32,
) {
    let bytes = 4usize;
    let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
    let v = match format {
        2 => (b << 16) | (g << 8) | r,
        _ => (r << 16) | (g << 8) | b,
    };
    let mut y = 0u32;
    while y < fb_h {
        let row = (fb_base as usize + y as usize * fb_pitch as usize) as *mut u8;
        let mut x = 0u32;
        while x < fb_w {
            unsafe {
                let p = row.add(x as usize * bytes) as *mut u32;
                core::ptr::write_volatile(p, v);
            }
            x += 1;
        }
        y += 1;
    }
}

// ------------------------------------------------------- resolution picker
//
// The firmware's GOP exposes a list of modes; we keep the 32bpp ones and let
// the user pick.  The prompt is a `timeout` one: it never blocks the boot, and
// a machine with no keyboard (or `make test`) simply takes the default.

/// Preferred resolution when the user does not choose one.
const DEFAULT_W: u32 = 1280;
const DEFAULT_H: u32 = 720;
/// How long the resolution prompt waits for a key before taking the default.
const RES_TIMEOUT_MS: u64 = 10_000;
/// Upper bound on the modes we list; firmware offers far fewer.
const MAX_MODES: usize = 64;

/// One 32bpp GOP mode, remembered so a mode can be set after the menu is drawn
/// (`GraphicsOutput::modes` borrows the protocol).
#[derive(Clone, Copy)]
struct ModeEntry {
    mode: Mode,
    w: u32,
    h: u32,
}

type ModeTable = [Option<ModeEntry>; MAX_MODES];

/// Framebuffer parameters: `(base, width, height, pitch, format)`.
type FbParams = (u64, u32, u32, u32, u8);

/// Print the loader banner and the CPU feature line for the active mode.
fn banner(base: u64, w: u32, h: u32) {
    log!("RondOS UEFI Loader  [{}]", BUILD_ID);
    log!();
    log!("  framebuffer: {}x{} @ 0x{:08x} (32bpp)", w, h, base);
    // Which page-table shape this CPU needs; on a screen-only machine this is
    // the only record of the feature bits the hand-off depends on.
    log_cpu();
}

/// Collect the 32bpp modes the firmware offers, in its own order.
fn collect_modes(gop: &GraphicsOutput) -> (ModeTable, usize) {
    let mut out: ModeTable = [None; MAX_MODES];
    let mut n = 0usize;
    for mode in gop.modes() {
        if n == MAX_MODES {
            log!("  warning: more than {} GOP modes, ignoring the rest", MAX_MODES);
            break;
        }
        let info = mode.info();
        if info.pixel_format() != PixelFormat::Bgr && info.pixel_format() != PixelFormat::Rgb {
            continue; // indexed/Bitmask modes cannot be drawn directly
        }
        let (w, h) = info.resolution();
        out[n] = Some(ModeEntry {
            mode,
            w: w as u32,
            h: h as u32,
        });
        n += 1;
    }
    (out, n)
}

/// Index of the mode closest to `want_w` x `want_h` (exact match wins).
fn closest_mode(modes: &[Option<ModeEntry>], want_w: u32, want_h: u32) -> usize {
    let mut best = 0usize;
    let mut best_score = i64::MAX;
    for (i, m) in modes.iter().enumerate() {
        let m = m.unwrap();
        let score = (m.w as i64 - want_w as i64).abs() + (m.h as i64 - want_h as i64).abs();
        if score < best_score {
            best_score = score;
            best = i;
        }
    }
    best
}

/// Set `mode` and return the framebuffer parameters it produced.
fn apply_mode(gop: &mut GraphicsOutput, mode: Mode) -> Option<FbParams> {
    gop.set_mode(&mode).ok()?;
    let info = gop.current_mode_info();
    let (w, h) = info.resolution();
    let base = {
        let mut fb = gop.frame_buffer();
        fb.as_mut_ptr() as u64
    };
    let format = match info.pixel_format() {
        PixelFormat::Rgb => 2u8,
        _ => 1u8, // Bgr — GOP's usual BGRA
    };
    Some((base, w as u32, h as u32, (info.stride() * 4) as u32, format))
}

/// Wait up to [`RES_TIMEOUT_MS`] for a mode number plus Enter.  Returns
/// `default_idx` on timeout, on Esc, or on an empty/invalid entry.
fn choose_mode(count: usize, default_idx: usize) -> usize {
    let mut acc: u32 = 0;
    let mut have = false;
    let mut picked: Option<usize> = None;
    uefi::system::with_stdin(|input: &mut Input| {
        let _ = input.reset(false); // drop anything the firmware buffered
        let poll = core::time::Duration::from_millis(100);
        let mut remaining = RES_TIMEOUT_MS;
        while remaining > 0 {
            if let Ok(Some(key)) = input.read_key() {
                match key {
                    Key::Printable(c) => {
                        match char::from(c) {
                            '\r' | '\n' => {
                                picked = Some(if have && acc >= 1 && acc as usize <= count {
                                    acc as usize - 1
                                } else {
                                    default_idx
                                });
                                return;
                            }
                            '0'..='9' => {
                                acc = acc.saturating_mul(10) + (char::from(c) as u32 - '0' as u32);
                                have = true;
                                if acc as usize > count {
                                    acc = 0;
                                    have = false;
                                }
                            }
                            '\x1b' => return, // cancel: take the default
                            _ => {}
                        }
                    }
                    Key::Special(_) => {}
                }
            }
            boot::stall(poll);
            remaining = remaining.saturating_sub(100);
        }
    });
    picked.unwrap_or(default_idx)
}

/// Read a whole file from the ESP.  Returns `(physical address, length)`.
fn read_file(path: &uefi::CStr16) -> Option<(u64, u64)> {
    let mut fs = boot::get_image_file_system(boot::image_handle()).ok()?;
    let mut root = fs.open_volume().ok()?;
    let handle = root
        .open(path, FileMode::Read, FileAttribute::empty())
        .ok()?;
    let mut file = handle.into_regular_file()?;
    let mut info_buf = [0u8; 512];
    let size = file.get_info::<FileInfo>(&mut info_buf).ok()?.file_size() as usize;
    if size == 0 {
        return None;
    }
    let buf = boot::allocate_pool(MemoryType::LOADER_DATA, size).ok()?;
    let slice = unsafe { core::slice::from_raw_parts_mut(buf.as_ptr(), size) };
    let n = file.read(slice).ok()?;
    Some((buf.as_ptr() as u64, n as u64))
}

/// Minimal ELF64 loader: copy each `PT_LOAD` to its `p_paddr`, zero the bss
/// tail, return `e_entry`.
/// Load the kernel's `PT_LOAD` segments, returning `(entry, phys_end)` where
/// `phys_end` is the highest physical byte the image occupies.
fn load_elf(pa: u64, len: u64) -> Option<(u64, u64)> {
    let buf = unsafe { core::slice::from_raw_parts(pa as *const u8, len as usize) };
    if buf.len() < 64 || &buf[0..4] != b"\x7fELF" || buf[4] != 2 || buf[5] != 1 {
        return None;
    }
    let e_entry = u64::from_le_bytes(buf[24..32].try_into().ok()?);
    let e_phoff = u64::from_le_bytes(buf[32..40].try_into().ok()?) as usize;
    let e_phentsize = u16::from_le_bytes(buf[54..56].try_into().ok()?) as usize;
    let e_phnum = u16::from_le_bytes(buf[56..58].try_into().ok()?) as usize;

    let mut phys_end = 0u64;
    for i in 0..e_phnum {
        let off = e_phoff + i * e_phentsize;
        let ph = buf.get(off..off + e_phentsize)?;
        let p_type = u32::from_le_bytes(ph[0..4].try_into().ok()?);
        if p_type != 1 {
            continue; // PT_LOAD
        }
        let p_offset = u64::from_le_bytes(ph[8..16].try_into().ok()?) as usize;
        let p_paddr = u64::from_le_bytes(ph[24..32].try_into().ok()?);
        let p_filesz = u64::from_le_bytes(ph[32..40].try_into().ok()?) as usize;
        let p_memsz = u64::from_le_bytes(ph[40..48].try_into().ok()?) as usize;
        // Claim the destination range so the firmware does not hand it to
        // anybody else.  If the range is already taken we still write to it:
        // the kernel image is the one thing that must survive boot services.
        reserve(p_paddr, p_memsz.max(p_filesz));
        if p_filesz > 0 {
            let src = buf.get(p_offset..p_offset + p_filesz)?;
            unsafe {
                core::ptr::copy_nonoverlapping(src.as_ptr(), p_paddr as *mut u8, p_filesz);
            }
        }
        if p_memsz > p_filesz {
            unsafe {
                core::ptr::write_bytes((p_paddr as *mut u8).add(p_filesz), 0, p_memsz - p_filesz);
            }
        }
        phys_end = phys_end.max(p_paddr + p_memsz as u64);
    }
    Some((e_entry, phys_end))
}

/// Best-effort `AllocateAddress` reservation of `[paddr, paddr+size)`.
fn reserve(paddr: u64, size: usize) {
    if size == 0 {
        return;
    }
    let pages = size.div_ceil(0x1000);
    let _ = boot::allocate_pages(
        boot::AllocateType::Address(paddr),
        MemoryType::LOADER_DATA,
        pages,
    );
}

/// Read one CMOS NVRAM byte (ports 0x70/0x71 are always available at CPL0).
unsafe fn cmos_read(reg: u8) -> u8 {
    let mut v: u8;
    asm!("out dx, al", in("dx") 0x70u16, in("al") 0x80 | reg, options(nomem, nostack));
    asm!("in al, dx", in("dx") 0x71u16, out("al") v, options(nomem, nostack));
    asm!("out dx, al", in("dx") 0x70u16, in("al") 0u8, options(nomem, nostack));
    v
}

unsafe fn cmos_write(reg: u8, value: u8) {
    asm!("out dx, al", in("dx") 0x70u16, in("al") 0x80 | reg, options(nomem, nostack));
    asm!("out dx, al", in("dx") 0x71u16, in("al") value, options(nomem, nostack));
    asm!("out dx, al", in("dx") 0x70u16, in("al") 0u8, options(nomem, nostack));
}

/// The kernel records its progress in CMOS NVRAM byte 0x2E; a machine that
/// resets gives no other evidence, so print what the *previous* boot reached
/// and clear it before handing over.
// --------------------------------------------------- durable progress record
//
// The kernel cannot write the boot medium after `ExitBootServices`, but DRAM
// keeps its contents across a warm reset.  The kernel therefore writes one
// record into the last page of usable RAM and the loader picks it up on the
// *next* boot, before handing control over again.  Must mirror
// `kernel64/src/boot.rs::ProgressRecord`.

const PROGRESS_MAGIC: u32 = 0x4752_5052;
const PROGRESS_VERSION: u32 = 1;
/// Must match `kernel64/src/boot.rs::BUILD_ID`.
const KERNEL_BUILD_ID: u32 = 0x4449_3043;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProgressRecord {
    magic: u32,
    version: u32,
    /// Which kernel wrote the record (0 means a stale/foreign one).
    build: u32,
    stage: u32,
    mask: u32,
    mask2: u32,
    count: u32,
    tsc_lo: u32,
    tsc_hi: u32,
    _pad: [u32; 7],
}
const _: () = assert!(core::mem::size_of::<ProgressRecord>() == 64);

/// Last page of the RAM the loader reported: where the kernel keeps the record.
fn progress_phys(bi: &BootInfo) -> u64 {
    let mut top = 0u64;
    for i in 0..bi.mem_count as usize {
        let m = &bi.mem[i];
        if m.kind == 1 {
            top = top.max(m.addr + m.len);
        }
    }
    if top == 0 {
        0
    } else {
        (top & !0xfff) - 0x1000
    }
}

/// Read the record the previous boot left; `None` when there is none.
fn progress_read(bi: &BootInfo) -> Option<ProgressRecord> {
    let pa = progress_phys(bi);
    if pa == 0 {
        return None;
    }
    let r = unsafe { core::ptr::read_unaligned(pa as *const ProgressRecord) };
    if r.magic == PROGRESS_MAGIC && r.version == PROGRESS_VERSION && r.build == KERNEL_BUILD_ID {
        Some(r)
    } else {
        // Say *why* it was rejected: a stale record and an absent one look the
        // same otherwise, and the difference matters when a machine keeps
        // reporting the same stage.
        log!(
            "  kernel progress at {:#x}: magic {:#010x} ver {} build {:#010x} (want {:#010x})",
            pa,
            r.magic,
            r.version,
            r.build,
            KERNEL_BUILD_ID
        );
        None
    }
}

// ------------------------------------------------- cumulative on-disk record
//
// CMOS is a handful of bytes that any firmware may `POST` over, and a reset
// wipes the screen.  The one durable place is the boot medium itself: one fixed
// record per boot, prepended so the newest is always first.  Unlike the old
// text log this is never *rewritten* from scratch — it is read, shifted down,
// and written back — so a boot that dies mid-write still leaves the previous
// records intact (the header's boot counter says which records are complete).

/// `"RBRD"`.
const REC_MAGIC: u32 = 0x4452_4252;
const REC_VERSION: u32 = 1;
/// Boots kept in the file; the newest is index 0.
const REC_KEEP: usize = 16;
/// Memory-map entries copied into each record.
const REC_MEM_SHOWN: usize = 24;
const REC_SIZE: usize = 40 + REC_MEM_SHOWN * 24;
/// A layout drift between the Rust struct and the tool that reads the file
/// would be silent, so pin it at compile time.
const _: () = assert!(core::mem::size_of::<BootRecord>() == REC_SIZE);
const _: () = assert!(core::mem::offset_of!(BootRecord, mem) == 40);
const _: () = assert!(core::mem::size_of::<RecordFile>() == 16 + REC_KEEP * REC_SIZE);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct BootRecord {
    /// 1-based sequence number, so gaps (failed writes) are visible.
    boot: u32,
    /// Last stage reached (see the README table).
    stage: u32,
    /// Stage bitmasks: `mask | mask2 << 8`.
    mask: u32,
    /// Number of stage writes that stuck.
    count: u32,
    /// Run signature (`0x5a` = at least one stage recorded).
    sig: u32,
    /// The CPU's TSC at the loader's own first entry, for a rough timestamp.
    tsc_lo: u32,
    tsc_hi: u32,
    /// How many memory-map entries the loader kept, and the first
    /// [`REC_MEM_SHOWN`] of them: this is what decides how much RAM the kernel
    /// believes in, and a board that reports it strangely is otherwise
    /// invisible.
    mem_count: u32,
    _pad: [u32; 1],
    /// `[addr, len, kind]` per entry, kept as raw 64-bit values: packing them
    /// into 32 bits was clever and wrong.
    mem: [[u64; 3]; REC_MEM_SHOWN],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RecordFile {
    magic: u32,
    version: u32,
    /// Total boots ever recorded; also validates the header.
    boots: u32,
    _pad: u32,
    recs: [BootRecord; REC_KEEP],
}

const REC_FILE_BYTES: usize = core::mem::size_of::<RecordFile>();
const REC_PATH: &uefi::CStr16 = cstr16!("\\rondos\\bootlog.bin");

fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags))
    };
    ((hi as u64) << 32) | lo as u64
}

/// Read the record file into `buf`; returns None when it is absent or invalid.
fn records_load(buf: &mut [u8; REC_FILE_BYTES]) -> Option<RecordFile> {
    use uefi::proto::media::file::{File, FileAttribute, FileMode};
    let mut fs = boot::get_image_file_system(boot::image_handle()).ok()?;
    let mut root = fs.open_volume().ok()?;
    let handle = root.open(REC_PATH, FileMode::Read, FileAttribute::empty()).ok()?;
    let mut f = handle.into_regular_file()?;
    let n = f.read(buf).ok()?;
    f.close();
    if n != REC_FILE_BYTES {
        return None;
    }
    let file: RecordFile = unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const RecordFile) };
    if file.magic != REC_MAGIC || file.version != REC_VERSION {
        return None;
    }
    Some(file)
}

/// Prepend `rec` and write the file back.
/// Fill the memory-map part of a record from the `BootInfo` we just built.
fn record_mem(mut rec: BootRecord, bi: &BootInfo) -> BootRecord {
    rec.mem_count = bi.mem_count;
    let n = (bi.mem_count as usize).min(REC_MEM_SHOWN);
    for i in 0..n {
        let m = &bi.mem[i];
        rec.mem[i] = [m.addr, m.len, m.kind as u64];
    }
    rec
}

fn records_save(rec: BootRecord, prev_boots: u32) {
    let mut buf = [0u8; REC_FILE_BYTES];
    let old = records_load(&mut buf);
    let mut rec = rec;
    rec.boot = old.as_ref().map(|f| f.boots).unwrap_or(0) + 1;
    let mut file = RecordFile {
        magic: REC_MAGIC,
        version: REC_VERSION,
        boots: rec.boot,
        _pad: 0,
        recs: [BootRecord::default(); REC_KEEP],
    };
    file.recs[0] = rec;
    if let Some(o) = &old {
        let n = o.recs.len().min(REC_KEEP - 1);
        file.recs[1..1 + n].copy_from_slice(&o.recs[..n]);
    }
    let _ = prev_boots;
    unsafe {
        let bytes = core::slice::from_raw_parts(
            &file as *const RecordFile as *const u8,
            REC_FILE_BYTES,
        );
        if write_file(REC_PATH, bytes).is_none() {
            // Best effort: a firmware that cannot write the ESP still has the
            // screen and CMOS channels.
        }
    }
}

/// Create/truncate `path` and write `data`.  Returns the byte count.
fn write_file(path: &uefi::CStr16, data: &[u8]) -> Option<usize> {
    use uefi::proto::media::file::{File, FileAttribute, FileMode};
    let mut fs = boot::get_image_file_system(boot::image_handle()).ok()?;
    let mut root = fs.open_volume().ok()?;
    let handle = root
        .open(path, FileMode::CreateReadWrite, FileAttribute::empty())
        .ok()?;
    let mut f = handle.into_regular_file()?;
    f.set_position(0).ok()?;
    f.write(data).ok()?;
    f.flush().ok()?;
    f.close();
    Some(data.len())
}

/// Print every record in the file, newest first (best effort).
fn records_report() {
    let mut buf = [0u8; REC_FILE_BYTES];
    let Some(file) = records_load(&mut buf) else {
        log!("  disk log: none yet");
        return;
    };
    log!("  disk log: {} boot(s) recorded", file.boots);
    for (i, r) in file.recs.iter().enumerate() {
        if r.boot == 0 {
            break;
        }
        let _ = log!(
            "    #{:<3} stage {:02x} mask {:02x} {:02x} writes {} sig {:02x}",
            r.boot,
            r.stage,
            r.mask & 0xff,
            (r.mask >> 8) & 0xff,
            r.count,
            r.sig
        );
        if i >= 5 {
            break;
        }
    }
}

/// Human name for the x86-64 exception vectors the kernel may record.
fn exception_name(vector: u8) -> &'static str {
    match vector {
        0 => "#DE divide error",
        1 => "#DB debug",
        2 => "NMI",
        3 => "#BP breakpoint",
        4 => "#OF overflow",
        5 => "#BR bound range",
        6 => "#UD invalid opcode",
        7 => "#NM device not available",
        8 => "#DF double fault",
        10 => "#TS invalid TSS",
        11 => "#NP segment not present",
        12 => "#SS stack fault",
        13 => "#GP general protection",
        14 => "#PF page fault",
        16 => "#MF x87",
        17 => "#AC alignment check",
        18 => "#MC machine check",
        19 => "#XM SIMD",
        _ => "unknown vector",
    }
}

unsafe fn report_previous_stage() -> (u8, u8, u8, u8) {
    let stage = cmos_read(0x2e);
    let mask = cmos_read(0x36);
    let mask2 = cmos_read(0x37);
    let sig = cmos_read(0x38);
    let count = cmos_read(0x39);
    let entry_mark = cmos_read(0x3a);
    // Recorded by the kernel's exception dispatcher before it touches anything
    // that could depend on the page tables, so this survives a reset even when
    // the fault was a bad page walk.
    let fault_vec = cmos_read(0x3b);
    let fault_err = cmos_read(0x3c);
    cmos_write(0x2e, 0);
    cmos_write(0x3a, 0);
    cmos_write(0x3b, 0);
    cmos_write(0x3c, 0);
    cmos_write(0x36, 0);
    cmos_write(0x37, 0);
    cmos_write(0x38, 0);
    cmos_write(0x39, 0);
    {
        let what = match entry_mark {
            0x00 => "nothing (loader never jumped, or CMOS dropped it)",
            0xe1 => "_start entered",
            0xe2 => ".bss/boot stack reachable",
            0xe3 => "entered kmain",
            0xe4 => "BootInfo magic found",
            0xe5 => "BootInfo adopted",
            0xee => "PANIC: BootInfo failed validation",
            0xef => "PANIC: no BootInfo at the given address",
            0xff => "ran to completion",
            _ => "unknown",
        };
        log!("  kernel breadcrumb: {:#04x} ({})", entry_mark, what);
    }
    if fault_vec != 0 {
        let _ = log!(
            "  previous kernel CPU FAULT: vector {:#04x} ({}) error {:#06x}",
            fault_vec,
            exception_name(fault_vec),
            fault_err
        );
    }
    if stage == 0 {
        log!("  previous kernel boot: none (cold start)");
    } else if stage == 0xff {
        log!("  previous kernel boot: reached idle (clean)");
    } else {
        let _ = log!(
            "  previous kernel boot RESET at stage {:#04x}  <-- see README stage table",
            stage
        );
    }
    // Which stages it reached, not just the last one.  One line per mark so the
    // last line on screen is where the previous boot got to.
    if sig == 0x5a {
        let mut b = 0u8;
        while b < 8 {
            if mask & (1u8 << b) != 0 {
                log!("    stage {} reached", b);
            }
            if mask2 & (1u8 << b) != 0 {
                log!("    mark {:#04x} reached", b + 16);
            }
            b += 1;
        }
        let _ = log!(
            "    ({} stage writes, mask {:02x} {:02x})",
            count, mask, mask2
        );
    } else if sig == 0xa5 {
        let _ = log!(
            "    kernel started but no stage was recorded (writes dropped?)"
        );
    } else {
        let _ = log!(
            "    no stage mask from the previous boot (sig {:#04x})",
            sig
        );
    }
    (stage, mask, mask2, sig)
}

/// Highest stage code with its bit set in either mask.
fn highest_stage(mask: u32, mask2: u32) -> u32 {
    let mut bit = 31u32;
    while bit > 0 {
        let set = if bit < 16 {
            mask & (1 << bit) != 0
        } else {
            mask2 & (1 << (bit - 16)) != 0
        };
        if set {
            return bit;
        }
        bit -= 1;
    }
    0
}

/// Highest physical byte of usable RAM according to the BootInfo we just filled.
fn ram_highest(bi: &BootInfo) -> u64 {
    let mut end = 0u64;
    for i in 0..bi.mem_count as usize {
        let m = &bi.mem[i];
        if m.kind == 1 {
            end = end.max(m.addr + m.len);
        }
    }
    end
}

/// Highest virtual address the stub itself uses (its image, `BootInfo`, the
/// page tables and the stack).  The identity map must at least cover this, or
/// the `mov cr3` below would unmap the code that executes right after it.
fn stub_high() -> u64 {
    let bi = core::ptr::addr_of!(BOOTINFO) as u64 + core::mem::size_of::<BootInfo>() as u64;
    let stack = core::ptr::addr_of!(STACK) as u64 + 32 * 1024;
    let tables =
        core::ptr::addr_of!(PD_KERN) as u64 + core::mem::size_of::<Page>() as u64;
    let pools = core::ptr::addr_of!(PD_ID) as u64
        + core::mem::size_of::<[Page; PD_POOL_PAGES]>() as u64;
    let pools2 = core::ptr::addr_of!(PD_PHYS) as u64
        + core::mem::size_of::<[Page; PD_POOL_PAGES]>() as u64;
    bi.max(stack).max(tables).max(pools).max(pools2)
}

/// CPUID.8000_0001H:EDX[26] — 1 GiB pages (`PDPE1GB`).
///
/// Optional: Intel 64 CPUs before ~2010 (Core 2, Atom, ...) do not have it, and
/// a PS=1 PDPT entry is then a reserved-bit violation.
fn has_1g_pages() -> bool {
    core::arch::x86_64::__cpuid(0x8000_0001).edx & (1 << 26) != 0
}

/// CPUID.8000_0001H:EDX[20] — NX / XD.  Without it, EFER.NXE is not writable
/// and bit 63 of a page-table entry is reserved, so neither may be used.
fn has_nx() -> bool {
    core::arch::x86_64::__cpuid(0x8000_0001).edx & (1 << 20) != 0
}

/// Print the CPU identity and the two feature bits that decide how the loader
/// builds the page tables.  On a machine with no serial port this is the only
/// way to see *why* the hand-off failed, so it goes to the screen too.
fn log_cpu() {
    let v = core::arch::x86_64::__cpuid(0);
    let mut vendor = [0u8; 12];
    vendor[0..4].copy_from_slice(&v.ebx.to_le_bytes());
    vendor[4..8].copy_from_slice(&v.edx.to_le_bytes());
    vendor[8..12].copy_from_slice(&v.ecx.to_le_bytes());
    let vendor = core::str::from_utf8(&vendor).unwrap_or("?");
    let f = core::arch::x86_64::__cpuid(1);
    log!(
        "  cpu: {} family {:#x} model {:#x} | NX {} 1G-pages {}",
        vendor,
        (f.eax >> 8) & 0xf,
        (f.eax >> 4) & 0xf,
        if has_nx() { "yes" } else { "NO" },
        if has_1g_pages() { "yes" } else { "NO" }
    );
}

/// Fill a PDPT with 1 GiB leaf entries covering `regions` GiB from physical 0.
unsafe fn fill_1g(pdpt: &mut Page, regions: usize, nx: bool) {
    let nxbit = if nx { 1u64 << 63 } else { 0 };
    for i in 0..regions {
        pdpt.0[i] = ((i as u64) << 30) | 0x83 | nxbit;
    }
}

/// Fill a PDPT with page directories of 2 MiB leaf entries covering `regions`
/// GiB from physical 0.  The page directories come from a static pool.
unsafe fn fill_2m(pdpt: &mut Page, pool: &mut [Page], regions: usize, nx: bool) {
    let nxbit = if nx { 1u64 << 63 } else { 0 };
    let base = pool.as_mut_ptr();
    for i in 0..regions {
        let pd = &mut *base.add(i);
        pdpt.0[i] = (pd as *mut Page as u64) | 0x3;
        for j in 0..512 {
            pd.0[j] = (((i as u64) << 30) | ((j as u64) << 21)) | 0x83 | nxbit;
        }
    }
}

/// Build the loader's page tables and return the CR3 value.
///
/// Three windows, all sized from the machine instead of hard-coded:
///   * identity `0 .. max(4 GiB, stub_high, fb_end)` — keeps the stub mapped
///     across the `mov cr3` (firmware may load the image above 4 GiB) and keeps
///     the framebuffer reachable, wherever the firmware put it (see the caller);
///   * physmap `0xFFFF_8000_0000_0000 + pa -> pa` for **all** usable RAM, so the
///     kernel can reach a `BootInfo`/initrd/framebuffer above 4 GiB;
///   * kernel window `0xFFFF_FFFF_8000_0000 + pa -> pa`, 2 MiB pages, covering
///     the whole kernel image.
fn build_page_tables(kernel_phys_end: u64, ram_end: u64, fb_end: u64) -> u64 {
    unsafe {
        let pml4 = &mut *core::ptr::addr_of_mut!(PML4);
        let pdpt_id = &mut *core::ptr::addr_of_mut!(PDPT_ID);
        let pdpt_phys = &mut *core::ptr::addr_of_mut!(PDPT_PHYS);
        let pdpt_kern = &mut *core::ptr::addr_of_mut!(PDPT_KERN);
        let pd_kern = &mut *core::ptr::addr_of_mut!(PD_KERN);

        let pa = |p: &Page| p as *const Page as u64;
        let pd_id = &mut *core::ptr::addr_of_mut!(PD_ID);
        let pd_phys = &mut *core::ptr::addr_of_mut!(PD_PHYS);

        // A single PDPT holds 512 x 1 GiB = 512 GiB, which is plenty here.
        let gib = |bytes: u64| ((bytes + (1 << 30) - 1) >> 30).clamp(1, 512) as usize;

        // CPUs without 1 GiB pages must get 2 MiB pages (see `PD_POOL_PAGES`),
        // and CPUs without NX must not have bit 63 set anywhere.
        let one_g = has_1g_pages();
        let nx = has_nx();

        let id_bytes = stub_high().max(4 << 30).max(fb_end);
        let id_want = gib(id_bytes);
        let id_pages = if one_g { id_want } else { id_want.min(PD_POOL_PAGES) };
        if fb_end > (512u64 << 30) {
            // Beyond one PDPT: cannot be covered by the flat identity map.  The
            // kernel's `paint_phys` falls back to the device window in this
            // case, so the hand-off is still safe; say so rather than lying.
            let _ = log!(
                "  warning: framebuffer ends at {:#x}, above the 512 GiB identity limit",
                fb_end
            );
        }
        if !one_g && id_want > PD_POOL_PAGES {
            let _ = log!(
                "  warning: identity map wants {} GiB, capped at {} GiB (2 MiB pages)",
                id_want,
                PD_POOL_PAGES
            );
        }
        pml4.0[0] = pa(pdpt_id) | 0x3;
        if one_g {
            fill_1g(pdpt_id, id_pages, false);
        } else {
            fill_2m(pdpt_id, pd_id, id_pages, false);
        }

        let phys_want = gib(ram_end.max(4 << 30));
        let phys_pages = if one_g {
            phys_want
        } else {
            phys_want.min(PD_POOL_PAGES)
        };
        if !one_g && phys_want > PD_POOL_PAGES {
            let _ = log!(
                "  warning: physmap wants {} GiB, capped at {} GiB (2 MiB pages)",
                phys_want,
                PD_POOL_PAGES
            );
        }
        pml4.0[256] = pa(pdpt_phys) | 0x3;
        if one_g {
            fill_1g(pdpt_phys, phys_pages, nx);
        } else {
            fill_2m(pdpt_phys, pd_phys, phys_pages, nx);
        }

        let kern_pages = ((kernel_phys_end + (1 << 21) - 1) >> 21).clamp(1, 512) as usize;
        pml4.0[511] = pa(pdpt_kern) | 0x3;
        pdpt_kern.0[510] = pa(pd_kern) | 0x3;
        for i in 0..kern_pages {
            pd_kern.0[i] = ((i as u64) << 21) | 0x83;
        }
        let _ = log!(
            "  pages: identity {} GiB ({} pages), physmap {} GiB, kernel {} MiB",
            id_pages,
            if one_g { "1 GiB" } else { "2 MiB" },
            phys_pages,
            kern_pages * 2
        );

        // EFER.NXE so the NX bits above are legal, and CR0.WP.  Only touch
        // EFER.NXE on a CPU that actually has NX: on one that does not, the
        // bit is reserved and the `wrmsr` is a #GP.
        if nx {
            let efer: u32 = 0xC000_0080;
            let (lo, hi): (u32, u32);
            asm!("rdmsr", in("ecx") efer, out("eax") lo, out("edx") hi, options(nomem, nostack));
            let val = ((hi as u64) << 32 | lo as u64) | (1 << 11);
            asm!(
                "wrmsr",
                in("ecx") efer,
                in("eax") val as u32,
                in("edx") (val >> 32) as u32,
                options(nomem, nostack)
            );
        }

        let cr0: u64;
        asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack));
        asm!("mov cr0, {}", in(reg) cr0 | (1 << 16), options(nomem, nostack));

        pa(pml4)
    }
}

/// Fixed low-memory mailbox the kernel reads at entry, before it trusts
/// anything else.  `[magic, fb_phys, width, height, pitch, format]`.
///
/// The kernel's very first paint must not depend on the stack, the kernel's own
/// page tables, CMOS or `BootInfo`: this is the one thing that survives all of
/// them, so "did the kernel start?" has an answer even when everything else is
/// in doubt.
const MAILBOX_PHYS: u64 = 0x0000_6000;
const MAILBOX_MAGIC: u64 = 0x0058_4f42_4e4f_4f52; // "ROONBOX\0"

fn publish_mailbox(fb_base: u64, w: u32, h: u32, pitch: u32, format: u8) {
    let m = MAILBOX_PHYS as *mut u64;
    unsafe {
        core::ptr::write_volatile(m, MAILBOX_MAGIC);
        core::ptr::write_volatile(m.add(1), fb_base);
        core::ptr::write_volatile(m.add(2), w as u64);
        core::ptr::write_volatile(m.add(3), h as u64);
        core::ptr::write_volatile(m.add(4), pitch as u64);
        core::ptr::write_volatile(m.add(5), format as u64);
    }
}

/// `ExitBootServices` has returned: install our page tables and hand over.
///
/// `rdi` is the *physical* address of `BootInfo`; the kernel reaches it through
/// its own physmap window.
unsafe fn jump_to_kernel(entry: u64, bootinfo_pa: u64, cr3: u64, stack_top: u64) -> ! {
    asm!(
        "mov cr3, {cr3}",
        "mov rsp, {stack}",
        "mov rdi, {bi}",
        "jmp {entry}",
        cr3 = in(reg) cr3,
        stack = in(reg) stack_top,
        bi = in(reg) bootinfo_pa,
        entry = in(reg) entry,
        options(noreturn)
    );
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {
        unsafe { asm!("hlt", options(nomem, nostack)) };
    }
}
