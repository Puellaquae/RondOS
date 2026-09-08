//! Minimal multiboot 1 info parsing.
//!
//! **Temporary**: this is the M0 stand-in for the versioned `BootInfo` that the
//! UEFI stub will produce at M0.6/M0.7 (design §8.3).  It only needs to give
//! us the memory map so the frame allocator can come up.

#![allow(dead_code)]

use core::ptr::addr_of;

use crate::arch::x86_64::paging::phys_to_virt;
use crate::mm;

const FLAG_MEM: u32 = 1 << 0;
const FLAG_MMAP: u32 = 1 << 6;

#[repr(C)]
pub struct MultibootInfo {
    pub flags: u32,
    pub mem_lower: u32,
    pub mem_upper: u32,
    pub boot_device: u32,
    pub cmdline: u32,
    pub mods_count: u32,
    pub mods_addr: u32,
    pub syms: [u32; 4],
    pub mmap_length: u32,
    pub mmap_addr: u32,
    pub drives_length: u32,
    pub drives_addr: u32,
    pub config_table: u32,
    pub boot_loader_name: u32,
    pub apm_table: u32,
    pub vbe_control_info: u32,
    pub vbe_mode_info: u32,
    pub vbe_mode: u16,
    pub vbe_interface_seg: u16,
    pub vbe_interface_off: u16,
    pub vbe_interface_len: u16,
}

#[repr(C, packed)]
struct MmapEntry {
    size: u32,
    addr: u64,
    len: u64,
    kind: u32,
}

/// Parse the multiboot info block at physical address `mbi_phys` and fill the
/// kernel memory map.
pub fn parse(mbi_phys: u32) {
    let mbi = phys_to_virt(mbi_phys as usize) as *const MultibootInfo;
    let flags = unsafe { addr_of!((*mbi).flags).read_unaligned() };

    let map = mm::mem_map();
    map.clear();

    if (flags & FLAG_MMAP) != 0 {
        let mmap_len = unsafe { addr_of!((*mbi).mmap_length).read_unaligned() } as usize;
        let mmap_addr = unsafe { addr_of!((*mbi).mmap_addr).read_unaligned() } as usize;
        let base = phys_to_virt(mmap_addr);
        let mut off = 0usize;
        while off < mmap_len {
            let e = (base + off) as *const MmapEntry;
            let size = unsafe { addr_of!((*e).size).read_unaligned() } as usize;
            let addr = unsafe { addr_of!((*e).addr).read_unaligned() };
            let len = unsafe { addr_of!((*e).len).read_unaligned() };
            let kind = unsafe { addr_of!((*e).kind).read_unaligned() };
            map.push(addr, len, kind);
            if size == 0 {
                break;
            }
            off += size + 4;
        }
        return;
    }

    if (flags & FLAG_MEM) != 0 {
        let lower = unsafe { addr_of!((*mbi).mem_lower).read_unaligned() } as u64;
        let upper = unsafe { addr_of!((*mbi).mem_upper).read_unaligned() } as u64;
        map.push(0, lower * 1024, mm::MEM_KIND_USABLE);
        map.push(0x10_0000, upper * 1024, mm::MEM_KIND_USABLE);
    }
}

/// Boot loader name, if provided (useful in the boot banner).
pub fn boot_loader(mbi_phys: u32) -> Option<&'static str> {
    let mbi = phys_to_virt(mbi_phys as usize) as *const MultibootInfo;
    let flags = unsafe { addr_of!((*mbi).flags).read_unaligned() };
    if (flags & (1 << 9)) == 0 {
        return None;
    }
    let addr = unsafe { addr_of!((*mbi).boot_loader_name).read_unaligned() } as usize;
    if addr == 0 {
        return None;
    }
    let s = phys_to_virt(addr) as *const u8;
    let mut len = 0;
    unsafe {
        while *s.add(len) != 0 && len < 64 {
            len += 1;
        }
        core::str::from_utf8(core::slice::from_raw_parts(s, len)).ok()
    }
}
