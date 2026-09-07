//! Kernel heap allocator, exposed as the Rust global allocator.
//!
//! A first-fit free-list allocator with coalescing.  Every block carries an
//! 8-byte header holding its total size; free blocks additionally link
//! themselves into an address-sorted list stored in their own payload area,
//! so adjacent frees can be merged back together.
//!
//! The heap is not a fixed `.bss` pool: it owns a reserved *virtual* region
//! and lazily maps physical frames into it (via the VMM, see
//! [`crate::arch::map_kernel_frame`]) whenever the free list runs out.  This
//! keeps the kernel image small and lets the heap grow on demand.

#![allow(dead_code)]

use alloc::alloc::{GlobalAlloc, Layout};
use core::ptr::{self, NonNull};

use crate::arch::InterruptGuard;
use crate::utils::spinlock::SpinLock;

/// Reserved virtual range for the kernel heap.  Above the bootloader's
/// 64 MiB identity/mirror window (0xc000_0000 .. 0xc040_0000), so pages are
/// mapped here explicitly without splitting the mirrored huge pages.
pub const HEAP_START_VA: usize = 0xc400_0000;
pub const HEAP_LIMIT_VA: usize = 0xc500_0000;

/// Frames mapped per heap growth step (64 KiB).
const GROW_PAGES: usize = 16;

/// All allocation payloads are 8-byte aligned; block headers are 8 bytes.
const ALIGN: usize = 8;
const HEADER: usize = 8;

/// A free block is at least this large so it can store size + next pointer.
const MIN_BLOCK: usize = 16;

#[inline]
fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

#[repr(C)]
struct FreeBlock {
    size: usize,
    next: Option<NonNull<FreeBlock>>,
}

impl FreeBlock {
    /// A block is located through its payload pointer: the header sits
    /// immediately below it.
    #[inline]
    fn from_payload(payload: *mut u8) -> *mut FreeBlock {
        (payload as usize - HEADER) as *mut FreeBlock
    }

    /// Read the header of the block containing `payload`.
    ///
    /// # Safety
    /// `payload` must have been returned by this allocator.
    #[inline]
    unsafe fn size_of(payload: *mut u8) -> usize {
        (*Self::from_payload(payload)).size
    }
}

pub struct Heap {
    /// Next virtual address at which new heap pages will be mapped.
    next_va: usize,
    /// One past the last virtual address the heap may use.
    limit_va: usize,
    free_head: Option<NonNull<FreeBlock>>,
}

impl Heap {
    pub const fn empty() -> Heap {
        Heap {
            next_va: 0,
            limit_va: 0,
            free_head: None,
        }
    }

    /// Configure the heap's virtual region.  No memory is mapped yet; pages
    /// are mapped on demand when the free list runs out.
    pub fn configure(&mut self, start_va: usize, limit_va: usize) {
        debug_assert!(start_va % 4096 == 0 && limit_va % 4096 == 0);
        debug_assert!(start_va < limit_va);
        self.next_va = start_va;
        self.limit_va = limit_va;
        self.free_head = None;
    }

    unsafe fn add_region(&mut self, start: usize, end: usize) {
        debug_assert!(start % ALIGN == 0 && end % ALIGN == 0);
        let size = end - start;
        debug_assert!(size >= MIN_BLOCK);
        let block = NonNull::new_unchecked(start as *mut FreeBlock);
        (*block.as_ptr()).size = size;
        (*block.as_ptr()).next = None;
        self.push_block(block);
    }

    /// Map another chunk of frames at `next_va` and add them to the free list.
    /// Returns false when the virtual budget is exhausted.
    fn grow(&mut self) -> bool {
        if self.next_va >= self.limit_va {
            return false;
        }
        let avail_pages = (self.limit_va - self.next_va) / 4096;
        let pages = GROW_PAGES.min(avail_pages);
        if pages == 0 {
            return false;
        }

        let start = self.next_va;
        let mut mapped = 0;
        while mapped < pages && crate::arch::map_kernel_frame(start + mapped * 4096) {
            mapped += 1;
        }
        if mapped == 0 {
            return false;
        }

        self.next_va = start + mapped * 4096;
        unsafe {
            self.add_region(start, self.next_va);
        }
        true
    }

    unsafe fn push_block(&mut self, block: NonNull<FreeBlock>) {
        let mut prev: Option<NonNull<FreeBlock>> = None;
        let mut cur = self.free_head;
        let block_addr = block.as_ptr() as usize;

        // Walk forward until the block's address order position is found.
        while let Some(node) = cur {
            if (node.as_ptr() as usize) > block_addr {
                break;
            }
            prev = cur;
            cur = (*node.as_ptr()).next;
        }

        let mut size = (*block.as_ptr()).size;
        let mut next = cur;

        // Coalesce with the successor (immediately after this block).
        if let Some(succ) = cur {
            if block_addr + size == succ.as_ptr() as usize {
                size += (*succ.as_ptr()).size;
                next = (*succ.as_ptr()).next;
            }
        }

        (*block.as_ptr()).size = size;
        (*block.as_ptr()).next = next;

        // Link (and possibly merge) with the predecessor.
        match prev {
            None => self.free_head = Some(block),
            Some(pred) => {
                if pred.as_ptr() as usize + (*pred.as_ptr()).size == block_addr {
                    (*pred.as_ptr()).size += size;
                    (*pred.as_ptr()).next = next;
                } else {
                    (*pred.as_ptr()).next = Some(block);
                }
            }
        }
    }

    unsafe fn pop_first_fit(&mut self, need: usize) -> Option<NonNull<FreeBlock>> {
        let mut prev: Option<NonNull<FreeBlock>> = None;
        let mut cur = self.free_head;

        while let Some(node) = cur {
            let node_size = (*node.as_ptr()).size;
            if node_size >= need {
                // Split off the tail if it is big enough to be a free block.
                let rest = node_size - need;
                if rest >= MIN_BLOCK {
                    (*node.as_ptr()).size = need;
                    let tail_addr = node.as_ptr() as usize + need;
                    let tail = NonNull::new_unchecked(tail_addr as *mut FreeBlock);
                    (*tail.as_ptr()).size = rest;
                    (*tail.as_ptr()).next = (*node.as_ptr()).next;
                    // Replace node with tail in the list.
                    if let Some(p) = prev {
                        (*p.as_ptr()).next = Some(tail);
                    } else {
                        self.free_head = Some(tail);
                    }
                    return Some(node);
                }
                // Take the whole block out of the free list.
                let next = (*node.as_ptr()).next;
                if let Some(p) = prev {
                    (*p.as_ptr()).next = next;
                } else {
                    self.free_head = next;
                }
                return Some(node);
            }
            prev = cur;
            cur = (*node.as_ptr()).next;
        }
        None
    }

    pub unsafe fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        if size == 0 {
            // Zero-sized allocations still need a unique, aligned, non-null
            // pointer (it is never dereferenced).
            return layout.align() as *mut u8;
        }
        if layout.align() > ALIGN {
            // Over-aligned allocations are not supported yet.  Raising this
            // needs a variable payload offset plus an anchor header.
            panic!("kheap: layout alignment {} > {ALIGN} unsupported", layout.align());
        }

        // Total block size: header + payload rounded up to our alignment.
        let need = align_up(size + HEADER, ALIGN).max(MIN_BLOCK);

        loop {
            if let Some(b) = self.pop_first_fit(need) {
                // pop_first_fit records the exact block size (split tail off,
                // or keeps the whole block) so dealloc can hand it all back.
                return (b.as_ptr() as usize + HEADER) as *mut u8;
            }
            if !self.grow() {
                return ptr::null_mut(); // out of memory
            }
        }
    }

    /// # Safety
    /// `ptr` must come from a previous `alloc` with a matching `layout`.
    pub unsafe fn dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        if layout.size() == 0 {
            return;
        }
        let block = FreeBlock::from_payload(ptr);
        let size = (*block).size;
        let node = NonNull::new_unchecked(block);
        (*node.as_ptr()).next = None;
        self.push_block(node);
        debug_assert_eq!(size % ALIGN, 0);
        let _ = size;
    }

    /// Bytes of free memory (for diagnostics).
    pub fn free_bytes(&self) -> usize {
        let mut total = 0;
        let mut cur = self.free_head;
        while let Some(node) = cur {
            total += unsafe { (*node.as_ptr()).size };
            cur = unsafe { (*node.as_ptr()).next };
        }
        total
    }
}

/// Global kernel allocator.  All heap operations run with interrupts disabled
/// so the preemptive scheduler cannot switch away while the lock is held.
///
/// Safety: the free list stores raw pointers into the heap's own region, which
/// is only touched while holding the internal spin lock.  The heap itself
/// never escapes to another address space, so it is sound to treat it as Send
/// + Sync on this single-address-space kernel.
pub struct KernelHeap;

// The `Heap` inside the static `SpinLock` must be Send for the lock to be
// Sync.  All access is serialized by the lock, so this is sound.
unsafe impl Send for Heap {}

#[global_allocator]
pub static KERNEL_HEAP_ALLOC: KernelHeap = KernelHeap;

unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _g = InterruptGuard::new();
        HEAP.with(|heap| heap.alloc(layout))
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let _g = InterruptGuard::new();
        HEAP.with(|heap| heap.dealloc(ptr, layout))
    }
}

static HEAP: SpinLock<Heap> = SpinLock::new(Heap::empty());

/// Initialize the kernel heap's virtual region.  Must be called once, before
/// any allocation; frames are mapped lazily on first use.
pub fn init_heap() {
    HEAP.with(|heap| heap.configure(HEAP_START_VA, HEAP_LIMIT_VA));
}

/// Free bytes currently on the heap free list (diagnostics).
pub fn heap_free_bytes() -> usize {
    HEAP.with(|heap| heap.free_bytes())
}
