#![allow(dead_code)]

use crate::loader::KERNEL_VADDR_BASE;
use crate::utils::{singleton::Singleton, BitAccess};
use core::fmt::Debug;
use super::available_mem_size;

pub static PAGE_ALLOC: Singleton<PageAllocator> = Singleton::UNINIT;

// End of the kernel image, defined by the linker script.
extern "C" {
    static _kernel_end: u8;
}

/// Low memory mapped 1:1 / mirrored at KERNEL_BASE by the bootloader is the
/// first 64 MiB; the page pool must stay inside it.
const MAPPED_END: usize = 0x0400_0000;

// 4KB page
pub struct PageAllocator {
    bitmap: BitMap,
    base_addr: usize,
}

impl Default for PageAllocator {
    fn default() -> Self {
        let avail_end = (0x100000 + available_mem_size() as usize).min(MAPPED_END);
        // The page pool starts after the kernel image, but never inside the
        // low 1 MiB (conventional memory holds BIOS data / the loader).
        let kernel_end = (((&raw const _kernel_end) as usize + 0xfff) & !0xfff).max(0x100000);
        assert!(
            kernel_end < avail_end,
            "kernel image (end {kernel_end:#x}) exceeds the mapped window {avail_end:#x}"
        );

        let memsz = avail_end - kernel_end;
        let pagecnt = memsz / 4096;
        let bitmap_size = (pagecnt + 7) / 8;
        let bitmap_pages = (bitmap_size + 4095) / 4096;
        // Pages handed out to callers live after the bitmap.
        let win_pages = pagecnt - bitmap_pages.min(pagecnt);

        let data_ptr = (KERNEL_VADDR_BASE as usize + kernel_end) as *mut u8;
        let base_addr = KERNEL_VADDR_BASE as usize + kernel_end + bitmap_pages * 4096;
        Self {
            bitmap: BitMap::new(data_ptr, win_pages),
            base_addr,
        }
    }
}

impl PageAllocator {
    pub fn get_page(&mut self, cnt: usize) -> Option<*mut u8> {
        let avl_page = self.bitmap.find(0, cnt, false)?;
        self.bitmap.flips(avl_page, cnt);
        Some((self.base_addr + avl_page * 4096) as *mut u8)
    }

    pub fn free_page(&mut self, page: *mut u8, cnt: usize) {
        let page = page as usize;
        assert!(page % 4096 == 0);
        let pidx = (page - self.base_addr) / 4096;
        assert!(self.bitmap.all(pidx, cnt));
        self.bitmap.flips(pidx, cnt);
    }
}

struct BitMap {
    data_ptr: *mut u8,
    size: usize,
}

impl BitMap {
    fn new(data_ptr: *mut u8, size: usize) -> BitMap {
        let len = (size + 7) / 8;
        unsafe {
            data_ptr.write_bytes(0, len);
        }
        BitMap { data_ptr, size }
    }

    fn test(&self, idx: usize) -> bool {
        assert!(idx < self.size);
        let elem = unsafe { self.data_ptr.offset((idx / 8) as isize).read_volatile() };
        elem & (1 << (idx % 8)) != 0
    }

    fn clear(&mut self, idx: usize) {
        assert!(idx < self.size);
        unsafe {
            let val = self.data_ptr.offset((idx / 8) as isize).read_volatile();
            self.data_ptr
                .offset((idx / 8) as isize)
                .write_volatile(val & !(1 << (idx % 8)));
        }
    }

    fn mark(&mut self, idx: usize) {
        assert!(idx < self.size);
        unsafe {
            let val = self.data_ptr.offset((idx / 8) as isize).read_volatile();
            self.data_ptr
                .offset((idx / 8) as isize)
                .write_volatile(val | (1 << (idx % 8)));
        }
    }

    fn flip(&mut self, idx: usize) {
        assert!(idx < self.size);
        unsafe {
            let val = self.data_ptr.offset((idx / 8) as isize).read_volatile();
            self.data_ptr
                .offset((idx / 8) as isize)
                .write_volatile(val ^ (1 << (idx % 8)));
        }
    }

    fn flips(&mut self, idx: usize, len: usize) {
        for i in 0..len {
            self.flip(idx + i);
        }
    }

    fn set(&mut self, idx: usize, val: bool) {
        assert!(idx < self.size);
        if val {
            self.mark(idx)
        } else {
            self.clear(idx)
        }
    }

    fn sets(&mut self, idx: usize, len: usize, val: bool) {
        for i in 0..len {
            self.set(idx + i, val);
        }
    }

    fn contains(&self, start: usize, len: usize, val: bool) -> bool {
        for i in 0..len {
            if self.test(start + i) == val {
                return true;
            }
        }
        false
    }

    fn find(&self, start: usize, len: usize, val: bool) -> Option<usize> {
        for i in start..self.size {
            if !self.contains(i, len, !val) {
                return Some(i);
            }
        }
        None
    }

    fn all(&self, start: usize, len: usize) -> bool {
        !self.contains(start, len, false)
    }

    fn any(&self, start: usize, len: usize) -> bool {
        self.contains(start, len, true)
    }
}

#[repr(transparent)]
pub struct PageTableEntry(u32);

impl PageTableEntry {
    pub fn present(&self) -> bool {
        self.0.get_bit(0)
    }

    pub fn rw(&self) -> bool {
        self.0.get_bit(1)
    }

    pub fn is_user(&self) -> bool {
        self.0.get_bit(2)
    }

    pub fn write_through(&self) -> bool {
        self.0.get_bit(3)
    }

    pub fn cache_disable(&self) -> bool {
        self.0.get_bit(4)
    }

    pub fn accessed(&self) -> bool {
        self.0.get_bit(5)
    }

    pub fn dirty(&self) -> bool {
        self.0.get_bit(6)
    }

    pub fn page_attribute(&self) -> bool {
        self.0.get_bit(7)
    }

    pub fn global(&self) -> bool {
        self.0.get_bit(8)
    }

    pub fn available(&self) -> u32 {
        self.0.get_bits(9..=11)
    }

    pub fn addr(&self) -> u32 {
        self.0.get_bits(12..=31) << 12
    }
}

impl Debug for PageTableEntry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PageTableEntry")
            .field("present", &self.present())
            .field("rw", &self.rw())
            .field("is_user", &self.is_user())
            .field("write_through", &self.write_through())
            .field("cache_disable", &self.cache_disable())
            .field("accessed", &self.accessed())
            .field("dirty", &self.dirty())
            .field("page_attribute", &self.page_attribute())
            .field("global", &self.global())
            .field("available", &self.available())
            .field("addr", &format_args!("0x{:x}", self.addr()))
            .finish()
    }
}
