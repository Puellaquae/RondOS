//! Versioned boot handoff structure — M0.6.
//!
//! Until now the kernel read the multiboot info block directly and scattered
//! its contents into `mm`.  This module defines the real handoff contract:
//! a single, versioned, fixed-width structure that *any* loader can produce.
//! Today `multiboot.rs` fills it (the temporary 32-bit trampoline path); at
//! M0.7 the UEFI stub fills the same structure from GOP + `GetMemoryMap` +
//! the ESP files, and the trampoline disappears.
//!
//! Layout rules (the same discipline the user ABI will use, design §6.8):
//!
//! * a `StructHeader { magic, size, version }` first, so the kernel can
//!   recognise the structure and a future loader can append fields;
//! * fixed-width fields only (`u32`/`u64`), explicit padding, no `usize`;
//! * the memory map is a fixed inline array for now (the kernel has no heap at
//!   this point); the UEFI stub will be able to fill the same array.

#![allow(dead_code)]

use core::cell::UnsafeCell;

/// `"RND1"` — lets the kernel tell a `BootInfo` apart from a multiboot block.
pub const BOOTINFO_MAGIC: u32 = 0x524E_4431;
pub const BOOTINFO_VERSION: u32 = 1;

pub const MAX_MEM_ENTRIES: usize = 64;
pub const MAX_CMDLINE: usize = 128;

pub const BOOT_KIND_MULTIBOOT: u32 = 1;
pub const BOOT_KIND_UEFI: u32 = 2;

/// Firmware memory descriptor kind (`EFI_CONVENTIONAL_MEMORY` / multiboot
/// "available" are both 1).
pub const MEM_KIND_USABLE: u32 = 1;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct StructHeader {
    pub magic: u32,
    pub size: u32,
    pub version: u32,
    pub _pad: u32,
}

impl StructHeader {
    pub const fn new(size: u32) -> Self {
        Self {
            magic: BOOTINFO_MAGIC,
            size,
            version: BOOTINFO_VERSION,
            _pad: 0,
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct MemDesc {
    pub addr: u64,
    pub len: u64,
    pub kind: u32,
    pub _pad: u32,
}

/// Pixel layout of the linear framebuffer (filled by the UEFI stub at M0.7).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct FramebufferInfo {
    pub phys: u64,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bpp: u8,
    /// 1 = BGRx (GOP's usual `PixelBlueGreenRedReserved8BitPerColor`), 2 = RGBx.
    pub format: u8,
    pub _pad: [u8; 2],
}

#[repr(C)]
pub struct BootInfo {
    pub hdr: StructHeader,
    pub boot_kind: u32,
    pub mem_count: u32,
    pub physmap_base: u64,
    pub initrd_phys: u64,
    pub initrd_len: u64,
    pub acpi_rsdp: u64,
    pub fb_present: u32,
    pub _pad: u32,
    pub fb: FramebufferInfo,
    pub cmdline_len: u32,
    pub _reserved: [u32; 4],
    pub cmdline: [u8; MAX_CMDLINE],
    pub mem: [MemDesc; MAX_MEM_ENTRIES],
}

impl BootInfo {
    const fn new() -> Self {
        Self {
            hdr: StructHeader::new(core::mem::size_of::<BootInfo>() as u32),
            boot_kind: 0,
            mem_count: 0,
            physmap_base: crate::arch::x86_64::paging::PHYS_MAP_BASE as u64,
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

    pub fn mem_entries(&self) -> &[MemDesc] {
        &self.mem[..self.mem_count as usize]
    }

    pub fn cmdline(&self) -> &[u8] {
        &self.cmdline[..self.cmdline_len as usize]
    }

    pub fn push_mem(&mut self, addr: u64, len: u64, kind: u32) -> bool {
        if self.mem_count as usize >= MAX_MEM_ENTRIES {
            return false;
        }
        self.mem[self.mem_count as usize] = MemDesc {
            addr,
            len,
            kind,
            _pad: 0,
        };
        self.mem_count += 1;
        true
    }

    pub fn set_cmdline(&mut self, s: &[u8]) {
        let n = s.len().min(MAX_CMDLINE);
        self.cmdline[..n].copy_from_slice(&s[..n]);
        self.cmdline_len = n as u32;
    }

    /// Sanity check for a structure produced by an external loader.
    pub fn validate(&self) -> bool {
        self.hdr.magic == BOOTINFO_MAGIC
            && self.hdr.size as usize >= core::mem::size_of::<StructHeader>()
            && self.hdr.version <= BOOTINFO_VERSION
            && (self.mem_count as usize) <= MAX_MEM_ENTRIES
    }
}

#[repr(align(16))]
struct BootInfoCell(UnsafeCell<BootInfo>);

unsafe impl Sync for BootInfoCell {}

static BOOT_INFO: BootInfoCell = BootInfoCell(UnsafeCell::new(BootInfo::new()));

pub fn get() -> &'static BootInfo {
    unsafe { &*BOOT_INFO.0.get() }
}

pub fn get_mut() -> &'static mut BootInfo {
    unsafe { &mut *BOOT_INFO.0.get() }
}

/// True when `addr` looks like a `BootInfo` produced by an external loader.
pub fn probe(addr: u64) -> bool {
    if addr == 0 || addr % 4 != 0 {
        return false;
    }
    // The physical address must be inside the physmap window we can read.
    let va = crate::arch::x86_64::paging::phys_to_virt(addr as usize);
    let hdr = unsafe { core::ptr::read_unaligned(va as *const StructHeader) };
    hdr.magic == BOOTINFO_MAGIC && hdr.version <= BOOTINFO_VERSION
}

/// Copy an externally built `BootInfo` (UEFI stub) into kernel-owned storage.
pub fn adopt_external(addr: u64) -> bool {
    let va = crate::arch::x86_64::paging::phys_to_virt(addr as usize);
    let src = unsafe { &*(va as *const BootInfo) };
    if !src.validate() {
        return false;
    }
    let dst = get_mut();
    // Copy field by field: BootInfo is large and deliberately not `Copy`.
    dst.hdr = src.hdr;
    dst.boot_kind = src.boot_kind;
    dst.mem_count = src.mem_count;
    dst.physmap_base = src.physmap_base;
    dst.initrd_phys = src.initrd_phys;
    dst.initrd_len = src.initrd_len;
    dst.acpi_rsdp = src.acpi_rsdp;
    dst.fb_present = src.fb_present;
    dst.fb = src.fb;
    dst.cmdline_len = src.cmdline_len;
    dst.cmdline.copy_from_slice(&src.cmdline);
    dst.mem.copy_from_slice(&src.mem);
    true
}

/// Total usable RAM above 1 MiB, in bytes.
pub fn available_mem_size() -> u64 {
    let mut size = 0;
    for m in get().mem_entries() {
        if m.kind != MEM_KIND_USABLE {
            continue;
        }
        let start = m.addr.max(0x10_0000);
        let end = m.addr + m.len;
        if end > start {
            size += end - start;
        }
    }
    size
}

/// Highest usable physical address.
pub fn usable_end() -> usize {
    let mut end = 0u64;
    for m in get().mem_entries() {
        if m.kind == MEM_KIND_USABLE {
            end = end.max(m.addr + m.len);
        }
    }
    end as usize
}

pub fn dump() {
    crate::serial_println!(
        "bootinfo: magic {:#x} size {} version {} kind {}",
        get().hdr.magic,
        get().hdr.size,
        get().hdr.version,
        get().boot_kind
    );
    for m in get().mem_entries() {
        crate::serial_println!(
            "  mem {:#012x}-{:#012x} len {:#010x} kind {}",
            m.addr,
            m.addr + m.len,
            m.len,
            m.kind
        );
    }
}
