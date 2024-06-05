#![allow(dead_code)]

use crate::mm::MemLayout;

pub fn get_kernel_size() -> u32 {
    unsafe { *(0xc0020200 as *const u32) }
}

pub fn get_memlayout_buflen() -> u32 {
    unsafe { *(0xc0020204 as *const u32) }
}

pub fn get_memlayout() -> &'static [MemLayout] {
    unsafe {
        &*core::ptr::slice_from_raw_parts(0xc0020208 as *const MemLayout, get_memlayout_buflen() as usize)
    }
}

pub const KERNEL_VADDR_BASE: u32 = 0xc0000000;
pub const KERNEL_STACK_PADDR: u32 = 0x7c00;
pub const KERNEL_PLACE_BEGIN_PADDR: u32 = 0x20000;

pub const SEGMENT_KERNEL_CODE: u16 = 0x8;
