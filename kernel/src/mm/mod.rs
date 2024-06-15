#![allow(dead_code)]

use core::{alloc::GlobalAlloc, fmt::Debug, ptr::addr_of};

use page::PAGE_ALLOC;

use crate::loader;

pub mod page;

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
    // round 4KiB
    addr & (!((1 << 12) - 1))
}

pub struct Allocator {}

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        let reqsize = layout.size();
        PAGE_ALLOC
            .get_mut()
            .get_page((reqsize + 4095) / 4096)
            .unwrap()
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        let reqsize = layout.size();
        PAGE_ALLOC.get_mut().free_page(ptr, (reqsize + 4095) / 4096)
    }
}

#[global_allocator]
static ALLOCATOR: Allocator = Allocator {};
