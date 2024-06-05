#![allow(dead_code)]

use core::{fmt::Debug, ptr::addr_of};

use crate::loader;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MemLayoutKind {
    Usable = 1,
    Reserved = 2,
    ACPIReclaimableMemory = 3,
    ACPINVSMemory = 4,
    BadMemory = 5,
}

#[derive(Debug, Clone, Copy)]
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

pub fn available_mem_size() -> u64 {
    let mut last_end = 0;
    let mut size = 0;
    for mem in loader::get_memlayout() {
        if mem.kind() == MemLayoutKind::Usable && mem.addr() >= 0x10000 {
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

pub fn pg_round_down(addr: usize) -> usize{
    // round 4KiB
    addr & (!((1 << 12) - 1))
}
