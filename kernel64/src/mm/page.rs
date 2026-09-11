//! Physical frame allocator (bitmap over the physmap window).
//!
//! Ported from the i686 kernel; the only real change is the base address:
//! frames are handed out as **physmap** pointers
//! (`0xFFFF_8000_0000_0000 + pa`) instead of `0xC000_0000 + pa`.
//!
//! The bitmap itself lives in the first pages after the kernel image, and
//! handed-out frames start after the bitmap.

#![allow(dead_code)]

use crate::arch::x86_64::paging::phys_to_virt;
use crate::bootinfo::{self, MEM_KIND_USABLE};
use crate::utils::singleton::Singleton;

pub static PAGE_ALLOC: Singleton<PageAllocator> = Singleton::UNINIT;

extern "C" {
    static _kernel_end_phys: u8;
}

fn kernel_end_phys() -> usize {
    let sym = &raw const _kernel_end_phys as usize;
    (sym + 0xfff) & !0xfff
}

/// Lowest physical address the allocator may hand out: everything below is
/// firmware/legacy territory (IVT, BDA, EBDA, the loader's scratch).
const ALLOC_BASE: usize = 0x10_0000;

/// Ceiling of the physmap window: one PDPT holds 512 × 1 GiB.
const ALLOC_LIMIT: usize = 512 << 30;

pub struct PageAllocator {
    bitmap: BitMap,
    /// physmap VA of the page whose bit is 0.
    base_addr: usize,
}

impl Default for PageAllocator {
    /// Build the free map from the firmware's memory map.
    ///
    /// This used to assume "everything from the end of the kernel image to the
    /// top of RAM is free", which is **wrong on real hardware**: the loader's
    /// page tables, its `BootInfo`, the boot archive and firmware reservations
    /// can sit right after the image, and the first allocation would silently
    /// overwrite them (the kernel then dies the moment it switches CR3).
    fn default() -> Self {
        let boot = bootinfo::get();
        let kernel_end = kernel_end_phys().max(ALLOC_BASE);
        let limit = (bootinfo::usable_end()).min(ALLOC_LIMIT);
        assert!(
            kernel_end < limit,
            "kernel image (end {kernel_end:#x}) does not fit below the RAM top {limit:#x}"
        );

        let pages = (limit - ALLOC_BASE) / 4096;
        let bitmap_bytes = (pages + 7) / 8;
        let bitmap_pages = (bitmap_bytes + 4095) / 4096;

        // The bitmap itself has to live in RAM the firmware says is free.
        let mut bitmap_pa = 0usize;
        for m in boot.mem_entries() {
            if m.kind != MEM_KIND_USABLE {
                continue;
            }
            let start = (m.addr as usize).max(kernel_end);
            let end = ((m.addr + m.len) as usize).min(limit);
            if end > start && end - start >= bitmap_pages * 4096 {
                bitmap_pa = start;
                break;
            }
        }
        assert!(
            bitmap_pa != 0,
            "no usable RAM region large enough for the frame bitmap ({} KiB)",
            bitmap_pages * 4
        );

        let mut alloc = Self {
            bitmap: BitMap::new(phys_to_virt(bitmap_pa) as *mut u8, pages),
            base_addr: phys_to_virt(ALLOC_BASE),
        };

        // Start from "everything reserved", then free exactly the usable
        // regions, then take back the kernel image and the bitmap.
        alloc.bitmap.set_all();
        for m in boot.mem_entries() {
            if m.kind == MEM_KIND_USABLE {
                alloc.free_range(m.addr as usize, m.len as usize);
            }
        }
        alloc.reserve_range(ALLOC_BASE, kernel_end - ALLOC_BASE);
        alloc.reserve_range(bitmap_pa, bitmap_pages * 4096);
        // Firmware reservations (EfiLoaderCode/Data, page tables, `BootInfo`,
        // the boot archive) are simply never freed above, because they are not
        // `MEM_KIND_USABLE`.

        crate::serial_println!(
            "alloc: {} pages tracked, bitmap at {:#x} ({} KiB), kernel ends {:#x}, RAM top {:#x}",
            pages,
            bitmap_pa,
            bitmap_pages * 4,
            kernel_end,
            limit
        );
        alloc
    }
}

impl PageAllocator {
    fn bit_of(&self, pa: usize) -> Option<usize> {
        if pa < ALLOC_BASE || pa % 4096 != 0 {
            return None;
        }
        let idx = (pa - ALLOC_BASE) / 4096;
        (idx < self.bitmap.size()).then_some(idx)
    }

    fn free_range(&mut self, pa: usize, len: usize) {
        if len == 0 {
            return;
        }
        let start = pa.max(ALLOC_BASE);
        let end = (pa + len).min(self.bitmap.size() * 4096 + ALLOC_BASE);
        if end <= start {
            return;
        }
        let first = (start - ALLOC_BASE) / 4096;
        let last = (end - ALLOC_BASE).div_ceil(4096);
        for i in first..last.min(self.bitmap.size()) {
            self.bitmap.clear(i);
        }
    }

    fn reserve_range(&mut self, pa: usize, len: usize) {
        if len == 0 {
            return;
        }
        let Some(first) = self.bit_of(pa & !0xfff) else {
            return;
        };
        let last = ((pa + len - 1 - ALLOC_BASE) / 4096 + 1).min(self.bitmap.size());
        for i in first..last {
            self.bitmap.set(i);
        }
    }

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

    fn set(&mut self, idx: usize) {
        if idx >= self.size || self.test(idx) {
            return;
        }
        self.flip(idx);
    }

    fn clear(&mut self, idx: usize) {
        if idx >= self.size || !self.test(idx) {
            return;
        }
        self.flip(idx);
    }

    fn set_all(&mut self) {
        let len = (self.size + 7) / 8;
        unsafe {
            self.data_ptr.write_bytes(0xff, len);
        }
    }

    fn size(&self) -> usize {
        self.size
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
