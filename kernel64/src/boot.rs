//! Boot self-checks and the normal (non-test) boot path.
//!
//! `make run` boots through here: a handful of *necessary* invariants are
//! verified, the scheduler is started, `/bin/init` is spawned and the boot
//! thread idles.  The full test suite lives in [`crate::tests`] and is compiled
//! in only with the `kernel-tests` feature (`make test`).

use crate::arch::x86_64::paging::{phys_to_virt, X86_64Paging, KERNEL_VIRT_BASE};
use crate::arch::x86_64::{inb, outb, pic, sti};
use crate::mm;
use crate::mm::vm::PagingArch;
use crate::thread;

/// PIT frequency: one tick every 5 ms.
pub const TIMER_HZ: u32 = 200;

// ------------------------------------------------------- boot stage marker
//
// A machine with no serial port and a kernel that resets gives no evidence at
// all, so the kernel writes its progress into one byte of battery-backed CMOS
// RAM.  The UEFI loader prints the value it finds on the next boot: if the
// machine resets in a loop, that number is the last stage that was reached.
//
// Register 0x2E is in the "extended" NVRAM area (0x10..0x2F) that the RTC
// checksum does not cover, and it is restored to 0 once the kernel is idle.

/// CMOS NVRAM byte used for the boot stage.
const CMOS_STAGE: u8 = 0x2e;

/// Stage values written by [`stage`]; 0 = never started, 0xff = idle.
pub const STAGE_ENTERED: u8 = 1;
pub const STAGE_BOOTINFO: u8 = 2;
pub const STAGE_PAGING: u8 = 3;
pub const STAGE_CONSOLE: u8 = 4;
pub const STAGE_SELFCHECK: u8 = 5;
pub const STAGE_TRAPS: u8 = 6;
pub const STAGE_SCHED: u8 = 7;
pub const STAGE_IRQ_ON: u8 = 8;
pub const STAGE_INIT: u8 = 9;
pub const STAGE_IDLE: u8 = 0xff;

fn cmos_read(reg: u8) -> u8 {
    outb(0x70, 0x80 | reg); // bit 7 keeps NMI disabled during the access
    let v = inb(0x71);
    outb(0x70, 0x00); // back to register 0 with NMI enabled
    v
}

fn cmos_write(reg: u8, value: u8) {
    outb(0x70, 0x80 | reg);
    outb(0x71, value);
    outb(0x70, 0x00);
}

/// Registers the stage marker is written to, in order.
///
/// `0x2E` is the original choice; `0x34`/`0x35` are falling back registers, so a
/// board that silently drops the first write still reports something.
const CMOS_STAGE_REGS: [u8; 3] = [0x2e, 0x34, 0x35];

// ------------------------------------------------ durable progress record
//
// CMOS is a few bytes any firmware may rewrite during POST, and the screen only
// survives until the next power-up.  DRAM, on the other hand, keeps its contents
// across a warm reset, so the kernel also writes its progress into one page at
// the top of usable RAM.  The loader reads that page *before* handing over, puts
// it in the on-disk record, and then the kernel starts overwriting it for the
// next boot.

/// `"RPRG"`.
pub const PROGRESS_MAGIC: u32 = 0x4752_5052;
pub const PROGRESS_VERSION: u32 = 1;

/// Must match the loader's `BUILD_ID` (the two are built together).  This exact
/// string also lands in `.rodata`, where the loader looks for it to prove that
/// the `kernel.elf` it just loaded is the one it was built with.
pub const BUILD_TAG: &str = "fix-2026-09-11f";
pub const BUILD_ID: u32 = 0x4449_3043; // "DI0C"

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProgressRecord {
    pub magic: u32,
    pub version: u32,
    /// Which kernel wrote the record, so a stale one is recognisable.
    pub build: u32,
    /// Last stage code written.
    pub stage: u32,
    /// Stage bitmasks (`mask | mask2 << 8`).
    pub mask: u32,
    pub mask2: u32,
    /// Number of writes that stuck (CMOS counter).
    pub count: u32,
    /// `rdtsc` right before the first stage.
    pub tsc_lo: u32,
    pub tsc_hi: u32,
    pub _pad: [u32; 7],
}

impl ProgressRecord {
    const fn new() -> Self {
        Self {
            magic: PROGRESS_MAGIC,
            version: PROGRESS_VERSION,
            build: BUILD_ID,
            stage: 0,
            mask: 0,
            mask2: 0,
            count: 0,
            tsc_lo: 0,
            tsc_hi: 0,
            _pad: [0; 7],
        }
    }
}

fn progress() -> Option<&'static mut ProgressRecord> {
    let pa = crate::bootinfo::get().progress_phys as usize;
    if pa == 0 {
        return None;
    }
    Some(unsafe { &mut *(crate::arch::x86_64::paging::phys_to_virt(pa) as *mut ProgressRecord) })
}

fn progress_mark(stage: u8) {
    let Some(p) = progress() else { return };
    if p.magic != PROGRESS_MAGIC || p.version != PROGRESS_VERSION {
        *p = ProgressRecord::new();
    }
    if stage == 0 {
        // Fresh run: the timestamp of the first stage is the boot time.
        let t = read_tsc();
        *p = ProgressRecord::new();
        p.tsc_lo = t as u32;
        p.tsc_hi = (t >> 32) as u32;
        return;
    }
    if stage < 16 {
        p.mask |= 1 << stage;
    } else {
        p.mask2 |= 1 << (stage - 16);
    }
    p.stage = stage as u32;
    p.count = p.count.wrapping_add(1);
}

fn read_tsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags))
    };
    ((hi as u64) << 32) | lo as u64
}

/// Bitmask of the stages reached (bit N = stage N), plus a signature byte and a
/// write counter.
///
/// The signature is deliberately *not* cleared to 0 at the start of a boot:
/// a board that silently refuses writes above 0x2F would otherwise look exactly
/// like a kernel that never wrote anything.  It is cleared **before** the first
/// stage bit and set **after** it, so the loader can tell
///
/// * `0x00` — nothing ran (cold, or the writes do not stick),
/// * `0xA5` — `_start` ran but not even the first stage bit stuck,
/// * `0x5A` — at least one stage bit was recorded (the mask is then meaningful).
const CMOS_STAGE_MASK: u8 = 0x36;
const CMOS_STAGE_MASK2: u8 = 0x37;
const CMOS_STAGE_SIG: u8 = 0x38;
/// Counts every stage bit written; a counter stuck at 1 with many stages means
/// the *writes* are being dropped, not the stages.
const CMOS_STAGE_COUNT: u8 = 0x39;
const CMOS_SIG_CLEAR: u8 = 0xa5;
const CMOS_SIG_VALUE: u8 = 0x5a;

fn cmos_stage_bits(n: u8) {
    if n == 0 || n > 0x1f {
        return;
    }
    // Clear first: if the machine dies during this boot, the loader must not
    // mistake the previous boot's signature for this one's.
    cmos_write(CMOS_STAGE_SIG, CMOS_SIG_CLEAR);
    if n < 16 {
        let bits = (cmos_read(CMOS_STAGE_MASK) as u32) | (1u32 << n);
        cmos_write(CMOS_STAGE_MASK, bits as u8);
    } else {
        let bits = (cmos_read(CMOS_STAGE_MASK2) as u32) | (1u32 << (n - 16));
        cmos_write(CMOS_STAGE_MASK2, bits as u8);
    }
    let count = cmos_read(CMOS_STAGE_COUNT).wrapping_add(1);
    cmos_write(CMOS_STAGE_COUNT, count);
    cmos_write(CMOS_STAGE_SIG, CMOS_SIG_VALUE);
}

/// Start a fresh stage mask and progress record for this boot.
/// Prove this really is the image the loader verified: scan our own `.rodata`
/// for the build tag and report how many times it appears.
pub fn verify_build_tag() {
    // Touch every byte through a volatile read so neither the length nor the
    // comparison can be folded away, and report what was actually read back.
    let tag = BUILD_TAG.as_bytes();
    let mut checksum = 0u32;
    for i in 0..tag.len() {
        let b = unsafe { core::ptr::read_volatile(tag.as_ptr().add(i)) };
        checksum = checksum.wrapping_add(((i as u32) << 8) | b as u32);
    }
    crate::serial_println!(
        "kernel build tag {} ({} bytes, sum {:#x})",
        BUILD_TAG,
        tag.len(),
        checksum
    );
    // Publish it in the progress record too, so the next boot's loader sees the
    // same identity even without a serial port.
    if let Some(p) = progress() {
        p.build = BUILD_ID;
    }
}

pub fn cmos_begin_run() {
    // Clear the record *before* anything else: a stale record from a previous
    // boot must never be mistaken for this one's progress.
    if let Some(p) = progress() {
        *p = ProgressRecord::new();
    }
    progress_mark(0);
    cmos_write(CMOS_STAGE_MASK, 0);
    cmos_write(CMOS_STAGE_MASK2, 0);
    cmos_write(CMOS_STAGE_COUNT, 0);
    cmos_write(CMOS_STAGE_SIG, CMOS_SIG_CLEAR);
}

/// The stage the previous boot reached (0 when the last boot finished).
pub fn previous_stage() -> u8 {
    cmos_read(CMOS_STAGE)
}

/// Record progress.  Also printed, so a hang (as opposed to a reset) shows it.
pub fn stage(n: u8) {
    // Painted straight into the framebuffer, independent of the text console:
    // this is the marker that survives a reset on a machine with no serial port.
    crate::bootscreen::mark(n);
    progress_mark(n);
    cmos_stage_bits(n);
    for (i, reg) in CMOS_STAGE_REGS.iter().enumerate() {
        cmos_write(*reg, n);
        let got = cmos_read(*reg);
        if got == n {
            if i > 0 {
                crate::serial_println!("boot: stage {:#04x} (cmos {:#04x})", n, reg);
            } else {
                crate::serial_println!("boot: stage {:#04x}", n);
            }
            return;
        }
        crate::serial_println!(
            "boot: cmos {:#04x} did not hold {:#04x} (read {:#04x})",
            reg,
            n,
            got
        );
    }
    crate::serial_println!("boot: stage {:#04x} (cmos unusable)", n);
}

/// Called once the system is up, so a normal boot leaves a clean marker.
pub fn stage_done() {
    for reg in CMOS_STAGE_REGS {
        cmos_write(reg, STAGE_IDLE);
    }
    // Do not leave a stale CPU-fault record for the loader to report next boot:
    // the kernel-mode tests deliberately raise #GP/#PF to probe the handlers.
    cmos_write(0x3b, 0);
    cmos_write(0x3c, 0);
}

/// The minimum the rest of the kernel assumes.  A failure here is fatal, so
/// panic with a precise message instead of limping on with a broken mapping.
/// End of the kernel image in physical memory (for diagnostics).
fn kernel_image_end() -> usize {
    extern "C" {
        static _kernel_end_phys: u8;
    }
    (&raw const _kernel_end_phys as usize + 0xfff) & !0xfff
}

pub fn self_check() {
    // A 16 MiB machine cannot hold the kernel image plus its frame bitmap plus
    // the tables the allocator is about to hand out; saying so is far more
    // useful than the allocator's "empty" panic (or a silent reset).
    let usable = crate::mm::available_mem_size();
    let top = crate::mm::usable_end();
    crate::serial_println!(
        "self-check: {} MiB usable below {:#x}, kernel ends {:#x}",
        usable / 1024 / 1024,
        top,
        kernel_image_end()
    );
    assert!(
        top > 32 << 20,
        "system has only {} MiB of usable RAM below {:#x}; RondOS needs at least 32 MiB",
        usable / 1024 / 1024,
        top
    );

    let root = X86_64Paging::active_root();

    // The physmap window must translate every page back to itself; everything
    // in `mm` walks memory through it.
    for pa in [0usize, 0x1000, 0x20_0000, 0x4000_0000] {
        assert_eq!(
            X86_64Paging::translate(root, phys_to_virt(pa)),
            Some(pa),
            "self-check: physmap broken at {:#x}",
            pa
        );
    }

    // The kernel image must be reachable through the kernel window.
    assert_eq!(
        X86_64Paging::translate(root, KERNEL_VIRT_BASE + 0x20_0000),
        Some(0x20_0000),
        "self-check: kernel window broken"
    );

    // The frame allocator must hand out and take back a frame.
    let before = mm::page_alloc().free_pages();
    let frame = mm::page_alloc()
        .get_page(1)
        .expect("self-check: page allocator is empty");
    unsafe { core::ptr::write_bytes(frame, 0xa5, mm::PAGE_SIZE) };
    mm::page_alloc().free_page(frame, 1);
    assert_eq!(
        mm::page_alloc().free_pages(),
        before,
        "self-check: frame leaked"
    );

    crate::serial_println!("self-check: physmap, kernel window, allocator ok");
}

// ------------------------------------------------------------ paint helpers
//
// Deliberately tiny and dependency-free: these run before the console, before
// the kernel's own page tables, before anything that could plausibly fail.  A
// full-screen colour is something a photograph cannot miss.

/// Ceiling of the loader's flat identity map: one PDPT of 1 GiB pages.
///
/// The loader sizes that map to cover the machine (its own image, `BootInfo`,
/// the low-memory mailbox and the framebuffer), but a single PDPT caps it at
/// 512 GiB.  A framebuffer above that is only reachable through the device
/// window, so `paint_phys` must not use the physical address.
const IDENTITY_LIMIT: u64 = 512 << 30;

/// Fill the whole screen with `rgb`, using the loader's identity mapping of the
/// physical framebuffer.  Returns false when there is no framebuffer.
///
/// # Safety
/// Only valid while the **loader's** page tables are active (`BootInfo`'s
/// `fb.phys` is reached as a physical address there).  The loader maps the
/// framebuffer identity as long as it lies below [`IDENTITY_LIMIT`]; above
/// that this transparently falls back to the device window, because a page
/// fault here would happen before the IDT exists (a triple fault, i.e. the
/// "loader painted, then the screen went black" failure).
pub unsafe fn paint_phys(rgb: u32) -> bool {
    let info = crate::bootinfo::get().fb;
    if crate::bootinfo::get().fb_present == 0 || info.phys == 0 || info.width == 0 {
        return false;
    }
    let fb_end = info
        .phys
        .saturating_add((info.pitch as u64).saturating_mul(info.height as u64));
    if fb_end > IDENTITY_LIMIT {
        // Not guaranteed to be identity-mapped: use the window the kernel
        // mapped itself, which reaches any physical address.
        return paint_dev(rgb);
    }
    let bytes = (info.bpp as u32 / 8).max(3);
    let pack = |rgb: u32| -> u32 {
        let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
        match info.format {
            2 => (b << 16) | (g << 8) | r,
            _ => (r << 16) | (g << 8) | b,
        }
    };
    let v = pack(rgb);
    let mut y = 0u32;
    while y < info.height {
        let row = (info.phys as usize + y as usize * info.pitch as usize) as *mut u8;
        let mut x = 0u32;
        while x < info.width {
            let p = row.add(x as usize * bytes as usize);
            core::ptr::write_volatile(p, (v & 0xff) as u8);
            if bytes >= 2 {
                core::ptr::write_volatile(p.add(1), ((v >> 8) & 0xff) as u8);
            }
            if bytes >= 3 {
                core::ptr::write_volatile(p.add(2), ((v >> 16) & 0xff) as u8);
            }
            x += 1;
        }
        y += 1;
    }
    true
}

/// Fill the screen through the **device window** mapping (`DEVICE_BASE`), i.e.
/// only valid once the kernel's own page tables (or the loader's, which map the
/// same VA) are active and the device mapping has been installed.
pub unsafe fn paint_dev(rgb: u32) -> bool {
    let info = crate::bootinfo::get().fb;
    if crate::bootinfo::get().fb_present == 0 || info.phys == 0 || info.width == 0 {
        return false;
    }
    if !crate::bootscreen::init() {
        return false;
    }
    let base = crate::io::fb::DEVICE_BASE as *mut u8;
    let bytes = (info.bpp as u32 / 8).max(3);
    let pack = |rgb: u32| -> u32 {
        let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
        match info.format {
            2 => (b << 16) | (g << 8) | r,
            _ => (r << 16) | (g << 8) | b,
        }
    };
    let v = pack(rgb);
    let mut y = 0u32;
    while y < info.height {
        let row = base.add(y as usize * info.pitch as usize);
        let mut x = 0u32;
        while x < info.width {
            let p = row.add(x as usize * bytes as usize);
            core::ptr::write_volatile(p, (v & 0xff) as u8);
            if bytes >= 2 {
                core::ptr::write_volatile(p.add(1), ((v >> 8) & 0xff) as u8);
            }
            if bytes >= 3 {
                core::ptr::write_volatile(p.add(2), ((v >> 16) & 0xff) as u8);
            }
            x += 1;
        }
        y += 1;
    }
    true
}

/// A fatal CPU fault reached the kernel's own IDT: paint the screen through the
/// **device window** so a photo on a machine with no serial port says "a fault
/// was caught here, the machine did not reset" instead of just holding the last
/// colour.  The vector and error code are in CMOS 0x3B/0x3C for the next boot.
///
/// Deliberately avoids the physmap, the frame allocator and the text console:
/// any of those may be exactly what broke.  If the device window is not mapped
/// yet this is a no-op and the CMOS record is the only evidence.
pub fn fault_signal() {
    if !crate::bootscreen::is_mapped() {
        return;
    }
    // With the window already mapped, `paint_dev`'s `bootscreen::init()` call is
    // a no-op, so this allocates nothing and touches no page table.
    // `rgb` is 0xRRGGBB: bright red, a colour no boot stage paints.
    let _ = unsafe { paint_dev(0x00_ff_0000) };
}

/// Pause for the self-evidence paints, in spin iterations.
///
/// Deliberately small: it only has to keep the screen state visible long enough
/// for a human to see it.  A large value is unusable under emulation (QEMU
/// executes every spin, so 150M iterations blew through the test timeout), and
/// the screen stays painted anyway because nothing clears it until the console
/// comes up.
pub const DELAY_DEFAULT: u32 = 20_000_000;

/// Set at boot from the loader's command line (`rondos.delay=N`).
static DELAY_ITERS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Resolve the pause once `BootInfo` is available.
pub fn init_delay() {
    use core::sync::atomic::Ordering;
    let cmd = crate::bootinfo::get().cmdline();
    let mut iters = DELAY_DEFAULT;
    if let Some(pos) = cmd.windows(12).position(|w| w == b"rondos.delay") {
        let mut n: u32 = 0;
        let mut seen_eq = false;
        for &b in &cmd[pos + 12..] {
            if b == b'=' && !seen_eq {
                seen_eq = true;
                continue;
            }
            if !b.is_ascii_digit() {
                break;
            }
            n = n.saturating_mul(10).saturating_add((b - b'0') as u32);
        }
        iters = n;
    }
    DELAY_ITERS.store(iters, Ordering::Release);
    crate::serial_println!("delay: {} iterations", iters);
}

pub fn delay_loops(_iterations: u32) {
    use core::sync::atomic::Ordering;
    let iterations = DELAY_ITERS.load(Ordering::Acquire);
    let mut i = 0u32;
    while i < iterations {
        core::hint::spin_loop();
        i += 1;
    }
}

/// Bring up the framebuffer console (design §8.2): the kernel's own output
/// channel on machines without serial.  Safe to call with no framebuffer.
pub fn init_fb_console() {
    let fb = crate::bootinfo::get().fb;
    if crate::bootinfo::get().fb_present == 0 {
        crate::serial_println!("fb: BootInfo has no framebuffer");
        return;
    }
    crate::io::fb::init(&fb);
}

/// Start the timer and the scheduler.  GDT/IDT must already be loaded.
pub fn bring_up_scheduler() {
    stage(STAGE_SCHED);
    // Sub-marks: on the target machine the first ring0 timer tick arrives right
    // after `sti`, and "stage 0x07" alone does not say whether the PIC, the PIT
    // or the scheduler was the last thing to run.
    stage(0x10); // entering
    pic::init();
    stage(0x11); // PIC remapped, IRQ0/IRQ1 unmasked
    pic::configure_pit(0, 2, TIMER_HZ);
    stage(0x12); // PIT programmed
    thread::init();
    stage(0x13); // scheduler state + idle stack ready
    stage(STAGE_IRQ_ON);
    sti();
    stage(0x15); // interrupts on (the first tick may already have run)
}

/// Normal boot: scheduler up, `/bin/init` running, boot thread idle.
#[cfg_attr(feature = "kernel-tests", allow(dead_code))]
pub fn normal_boot() -> ! {
    bring_up_scheduler();
    stage(STAGE_INIT);
    match crate::exec::spawn_path(b"/bin/init") {
        Ok(pid) => crate::serial_println!("boot: /bin/init is pid {}", pid),
        Err(e) => crate::serial_println!("boot: cannot start /bin/init: {:?}", e),
    }
    stage_done();
    // Boot is no longer in question: drop the diagnostic progress strip and let
    // the framebuffer console own the whole screen, so the shell (spawned by
    // /bin/init) does not run under a row of coloured stage blocks.  A failed
    // boot never reaches here, so the evidence survives exactly when it matters.
    crate::bootscreen::hide();
    crate::io::fb::reclaim_full_screen();
    loop {
        crate::arch::x86_64::hlt();
    }
}
