#![allow(dead_code)]

use core::fmt::Debug;
use core::ptr::addr_of;

use page::PAGE_ALLOC;

use crate::loader;

pub mod heap;
pub mod page;
pub mod vm;

pub use heap::{heap_free_bytes, init_heap};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MemLayoutKind {
    Usable = 1,
    Reserved = 2,
    ACPIReclaimableMemory = 3,
    ACPINVSMemory = 4,
    BadMemory = 5,
}

#[derive(Clone, Copy)]
#[repr(C, packed)]
pub struct MemLayout {
    addr: u64,
    len: u64,
    kind: MemLayoutKind,
}

impl MemLayout {
    pub fn addr(&self) -> u64 {
        unsafe { addr_of!(self.addr).read_unaligned() }
    }

    pub fn len(&self) -> u64 {
        unsafe { addr_of!(self.len).read_unaligned() }
    }

    pub fn kind(&self) -> MemLayoutKind {
        unsafe { addr_of!(self.kind).read_unaligned() }
    }
}

impl Debug for MemLayout {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MemLayout")
            .field("addr", &format_args!("0x{:x}", self.addr()))
            .field("len", &format_args!("0x{:x}", self.len()))
            .field("kind", &self.kind())
            .finish()
    }
}

pub fn available_mem_size() -> u64 {
    let mut last_end = 0;
    let mut size = 0;
    for mem in loader::get_memlayout() {
        if mem.kind() == MemLayoutKind::Usable && mem.addr() >= 0x100000 {
            if mem.addr() > last_end {
                last_end = mem.addr();
            }
            let this_end = mem.addr() + mem.len();
            size += this_end - last_end;
            last_end = this_end;
        }
    }
    size
}

pub fn pg_round_down(addr: usize) -> usize {
    addr & (!((1 << 12) - 1))
}

pub fn pg_round_up(addr: usize) -> usize {
    (addr + 4095) & (!((1 << 12) - 1))
}

pub fn total_pages() -> usize {
    (available_mem_size() as usize) / 4096
}

pub fn page_alloc() -> &'static mut page::PageAllocator {
    PAGE_ALLOC.get_mut()
}
