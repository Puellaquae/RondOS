//! Memory management — architecture-neutral parts plus the x86-64 page backend.

#![allow(dead_code)]

pub mod page;
pub mod vm;

use core::ptr::addr_of;


use crate::utils::singleton::Singleton;
use vm::PAGE_SIZE;

/// One firmware memory descriptor (multiboot mmap entry, later UEFI memdesc).
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct MemDesc {
    pub addr: u64,
    pub len: u64,
    pub kind: u32,
}

pub const MEM_KIND_USABLE: u32 = 1;

pub const MAX_MEM_ENTRIES: usize = 64;

pub struct MemoryMap {
    entries: [MemDesc; MAX_MEM_ENTRIES],
    len: usize,
}

impl Default for MemoryMap {
    fn default() -> Self {
        Self {
            entries: [MemDesc::default(); MAX_MEM_ENTRIES],
            len: 0,
        }
    }
}

impl MemoryMap {
    pub fn clear(&mut self) {
        self.len = 0;
    }

    pub fn push(&mut self, addr: u64, len: u64, kind: u32) -> bool {
        if self.len >= MAX_MEM_ENTRIES {
            return false;
        }
        self.entries[self.len] = MemDesc { addr, len, kind };
        self.len += 1;
        true
    }

    pub fn as_slice(&self) -> &[MemDesc] {
        &self.entries[..self.len]
    }
}

pub static MEMORY_MAP: Singleton<MemoryMap> = Singleton::UNINIT;

pub fn mem_map() -> &'static mut MemoryMap {
    MEMORY_MAP.get_mut()
}

/// Total usable RAM above 1 MiB, in bytes.
pub fn available_mem_size() -> u64 {
    let mut size = 0;
    for m in mem_map().as_slice() {
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

/// Highest usable physical address (capped by the caller).
pub fn usable_end() -> usize {
    let mut end = 0u64;
    for m in mem_map().as_slice() {
        if m.kind == MEM_KIND_USABLE {
            end = end.max(m.addr + m.len);
        }
    }
    end as usize
}

pub fn pg_round_down(addr: usize) -> usize {
    addr & !(PAGE_SIZE - 1)
}

pub fn pg_round_up(addr: usize) -> usize {
    (addr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

pub fn total_pages() -> usize {
    (available_mem_size() as usize) / PAGE_SIZE
}

pub fn page_alloc() -> &'static mut page::PageAllocator {
    page::PAGE_ALLOC.get_mut()
}

/// Dump the memory map through the serial log (boot diagnostics).
pub fn dump_memory_map() {
    for m in mem_map().as_slice() {
        crate::serial_println!(
            "  mem {:#012x}-{:#012x} len {:#010x} kind {}",
            m.addr,
            m.addr + m.len,
            m.len,
            m.kind
        );
    }
}

// Silence "field never read" for the packed-style accessors if unused.
#[allow(dead_code)]
fn _touch(m: &MemDesc) -> u64 {
    unsafe { addr_of!(m.addr).read_unaligned() }
}
