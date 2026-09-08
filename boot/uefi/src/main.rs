//! RondOS UEFI boot stub — M0.7.
//!
//! Runs as an `x86_64-unknown-uefi` application on the firmware's ESP:
//!
//! 1. pick a 32bpp GOP mode close to 640x480 and set it;
//! 2. read `\rondos\kernel.elf` and `\rondos\boot.tar` from the ESP;
//! 3. copy the kernel's `PT_LOAD` segments to their `p_paddr`;
//! 4. collect the UEFI memory map into the versioned `BootInfo`;
//! 5. build 4-level page tables (identity + physmap + kernel window);
//! 6. `ExitBootServices` and jump to the kernel with `rdi = &BootInfo`
//!    (physmap view).
//!
//! Everything it hands over is described by `BootInfo`, whose layout mirrors
//! `kernel64/src/bootinfo.rs` field for field.

#![no_std]
#![no_main]

use core::arch::asm;

use uefi::boot;
use uefi::mem::memory_map::{MemoryMap, MemoryType};
use uefi::prelude::*;
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};
use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode};

// ------------------------------------------------------------------ BootInfo

const BOOTINFO_MAGIC: u32 = 0x524E_4431;
const BOOTINFO_VERSION: u32 = 1;
const MAX_MEM_ENTRIES: usize = 64;
const MAX_CMDLINE: usize = 128;
const BOOT_KIND_UEFI: u32 = 2;

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
    _reserved: [u32; 4],
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
            _reserved: [0; 4],
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
struct Page([u64; 512]);

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
    // 1. GOP: pick a 32bpp mode close to 640x480.
    let handle = match boot::get_handle_for_protocol::<GraphicsOutput>() {
        Ok(h) => h,
        Err(_) => return Status::UNSUPPORTED,
    };
    let mut gop = match boot::open_protocol_exclusive::<GraphicsOutput>(handle) {
        Ok(g) => g,
        Err(_) => return Status::UNSUPPORTED,
    };
    if pick_mode(&mut gop).is_err() {
        return Status::UNSUPPORTED;
    }
    let (fb_base, fb_w, fb_h, fb_pitch, fb_format) = {
        let info = gop.current_mode_info();
        let (w, h) = info.resolution();
        let mut fb = gop.frame_buffer();
        (
            fb.as_mut_ptr() as u64,
            w as u32,
            h as u32,
            (info.stride() * 4) as u32,
            match info.pixel_format() {
                PixelFormat::Rgb => 2u8,
                _ => 1u8, // Bgr — GOP's usual BGRA
            },
        )
    };

    // 2. Kernel image and boot archive from the ESP.
    let kernel = match read_file(cstr16!("\\rondos\\kernel.elf")) {
        Some(v) => v,
        None => return Status::NOT_FOUND,
    };
    let tar = read_file(cstr16!("\\rondos\\boot.tar"));

    // 3. Load the kernel's PT_LOAD segments.
    let entry = match load_elf(kernel.0, kernel.1) {
        Some(e) => e,
        None => return Status::LOAD_ERROR,
    };

    // 4. BootInfo: memory map, framebuffer, initrd.
    let bi = unsafe { &mut *core::ptr::addr_of_mut!(BOOTINFO) };
    if let Ok(map) = boot::memory_map(MemoryType::LOADER_DATA) {
        for d in map.entries() {
            if bi.mem_count as usize >= MAX_MEM_ENTRIES {
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
    }
    bi.set_cmdline(b"rondos.uefi=1");

    // 5. Page tables.
    let cr3 = build_page_tables();

    // 6. Hand over.
    let bi_pa = core::ptr::addr_of!(BOOTINFO) as u64;
    let stack_top = (core::ptr::addr_of!(STACK) as u64) + (32 * 1024) as u64;

    unsafe { let _ = boot::exit_boot_services(Some(MemoryType::LOADER_DATA)); }
    unsafe { jump_to_kernel(entry, bi_pa, cr3, stack_top) }
}

/// Prefer a 32bpp mode close to 640x480; fall back to any 32bpp mode.
fn pick_mode(gop: &mut GraphicsOutput) -> Result<(), ()> {
    let mut best = None;
    let mut best_score = i64::MAX;
    for mode in gop.modes() {
        let info = mode.info();
        if info.pixel_format() != PixelFormat::Bgr && info.pixel_format() != PixelFormat::Rgb {
            continue;
        }
        let (w, h) = info.resolution();
        let score = (w as i64 - 640).abs() + (h as i64 - 480).abs();
        if score < best_score {
            best_score = score;
            best = Some(mode);
        }
    }
    match best {
        Some(m) => gop.set_mode(&m).map_err(|_| ()),
        None => Err(()),
    }
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
fn load_elf(pa: u64, len: u64) -> Option<u64> {
    let buf = unsafe { core::slice::from_raw_parts(pa as *const u8, len as usize) };
    if buf.len() < 64 || &buf[0..4] != b"\x7fELF" || buf[4] != 2 || buf[5] != 1 {
        return None;
    }
    let e_entry = u64::from_le_bytes(buf[24..32].try_into().ok()?);
    let e_phoff = u64::from_le_bytes(buf[32..40].try_into().ok()?) as usize;
    let e_phentsize = u16::from_le_bytes(buf[54..56].try_into().ok()?) as usize;
    let e_phnum = u16::from_le_bytes(buf[56..58].try_into().ok()?) as usize;

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
    }
    Some(e_entry)
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

/// Identity map 0..4 GiB, the physmap window and the kernel's 2 MiB window.
fn build_page_tables() -> u64 {
    unsafe {
        let pml4 = &mut *core::ptr::addr_of_mut!(PML4);
        let pdpt_id = &mut *core::ptr::addr_of_mut!(PDPT_ID);
        let pdpt_phys = &mut *core::ptr::addr_of_mut!(PDPT_PHYS);
        let pdpt_kern = &mut *core::ptr::addr_of_mut!(PDPT_KERN);
        let pd_kern = &mut *core::ptr::addr_of_mut!(PD_KERN);

        let pa = |p: &Page| p as *const Page as u64;

        // identity 0..4 GiB with 1 GiB pages (keeps the stub itself mapped)
        pml4.0[0] = pa(pdpt_id) | 0x3;
        for i in 0..4 {
            pdpt_id.0[i] = ((i as u64) << 30) | 0x83;
        }
        // physmap 0xFFFF_8000_0000_0000 + pa -> pa, NX
        pml4.0[256] = pa(pdpt_phys) | 0x3;
        for i in 0..4 {
            pdpt_phys.0[i] = ((i as u64) << 30) | 0x83 | (1u64 << 63);
        }
        // kernel window 0xFFFF_FFFF_8000_0000 + pa -> pa, 16 x 2 MiB
        pml4.0[511] = pa(pdpt_kern) | 0x3;
        pdpt_kern.0[510] = pa(pd_kern) | 0x3;
        for i in 0..16 {
            pd_kern.0[i] = ((i as u64) << 21) | 0x83;
        }

        // EFER.NXE so the NX bits above are legal, and CR0.WP.
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

        let cr0: u64;
        asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack));
        asm!("mov cr0, {}", in(reg) cr0 | (1 << 16), options(nomem, nostack));

        pa(pml4)
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
