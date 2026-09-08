//! Memory management — architecture-neutral parts plus the x86-64 page backend.

#![allow(dead_code)]

pub mod page;
pub mod vm;

pub use crate::bootinfo::{available_mem_size, usable_end};
pub use vm::PAGE_SIZE;

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
