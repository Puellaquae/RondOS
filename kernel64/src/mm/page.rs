//! Physical frame allocator (bitmap over the physmap window).
//!
//! Ported from the i686 kernel; the only real change is the base address:
//! frames are handed out as **physmap** pointers
//! (`0xFFFF_8000_0000_0000 + pa`) instead of `0xC000_0000 + pa`.
//!
//! The bitmap itself lives in the first pages after the kernel image, and
//! handed-out frames start after the bitmap.

#![allow(dead_code)]

use crate::arch::x86_64::paging::{phys_to_virt, PHYS_MAP_LIMIT};
use crate::utils::singleton::Singleton;

pub static PAGE_ALLOC: Singleton<PageAllocator> = Singleton::UNINIT;

extern "C" {
    static _kernel_end_phys: u8;
}

fn kernel_end_phys() -> usize {
    let sym = &raw const _kernel_end_phys as usize;
    (sym + 0xfff) & !0xfff
}

pub struct PageAllocator {
    bitmap: BitMap,
    base_addr: usize,
}

impl Default for PageAllocator {
    fn default() -> Self {
        let kernel_end = kernel_end_phys().max(0x100000);
        let avail_end = super::usable_end().min(PHYS_MAP_LIMIT);
        assert!(
            kernel_end < avail_end,
            "kernel image (end {kernel_end:#x}) exceeds the physmap window {avail_end:#x}"
        );

        let pagecnt = (avail_end - kernel_end) / 4096;
        let bitmap_size = (pagecnt + 7) / 8;
        let bitmap_pages = (bitmap_size + 4095) / 4096;
        let win_pages = pagecnt - bitmap_pages.min(pagecnt);

        let data_ptr = phys_to_virt(kernel_end) as *mut u8;
        let base_addr = phys_to_virt(kernel_end + bitmap_pages * 4096);
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

    pub fn free_pages(&self) -> usize {
        self.bitmap.count_zero()
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
        let elem = unsafe { self.data_ptr.add(idx / 8).read_volatile() };
        (elem & (1 << (idx % 8))) != 0
    }

    fn flip(&mut self, idx: usize) {
        assert!(idx < self.size);
        unsafe {
            let p = self.data_ptr.add(idx / 8);
            let val = p.read_volatile();
            p.write_volatile(val ^ (1 << (idx % 8)));
        }
    }

    fn flips(&mut self, idx: usize, len: usize) {
        for i in 0..len {
            self.flip(idx + i);
        }
    }

    /// True when any bit in `[start, start+len)` equals `val`.
    ///
    /// An out-of-range request reports `true` ("this start is unusable") so
    /// `find` skips it instead of asserting — otherwise every OOM path in the
    /// kernel would panic instead of returning `None`.
    fn contains(&self, start: usize, len: usize, val: bool) -> bool {
        if len == 0 {
            return false;
        }
        if start >= self.size || len > self.size - start {
            return true;
        }
        for i in 0..len {
            if self.test(start + i) == val {
                return true;
            }
        }
        false
    }

    fn find(&self, start: usize, len: usize, val: bool) -> Option<usize> {
        if len == 0 || len > self.size {
            return None;
        }
        for i in start..=self.size - len {
            if !self.contains(i, len, !val) {
                return Some(i);
            }
        }
        None
    }

    fn all(&self, start: usize, len: usize) -> bool {
        !self.contains(start, len, false)
    }

    fn count_zero(&self) -> usize {
        let mut n = 0;
        for i in 0..self.size {
            if !self.test(i) {
                n += 1;
            }
        }
        n
    }
}

/// Total usable memory in KiB (for the boot banner).
pub fn free_kib() -> usize {
    super::available_mem_size() as usize / 1024
}
