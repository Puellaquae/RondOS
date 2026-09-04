#![allow(dead_code)]

use crate::mm::MemLayout;

pub fn get_memlayout_buflen() -> u32 {
    unsafe { *(0xc0009300 as *const u32) }
}

pub fn get_memlayout() -> &'static [MemLayout] {
    unsafe {
        &*core::ptr::slice_from_raw_parts(
            0xc0009304 as *const MemLayout,
            get_memlayout_buflen() as usize,
        )
    }
}

pub const KERNEL_VADDR_BASE: u32 = 0xc0000000;
pub const KERNEL_STACK_PADDR: u32 = 0x7c00;

pub const SEGMENT_KERNEL_CODE: u16 = 0x8;
