//! i686 32-bit paging backend.
//!
//! All physical memory the kernel manages lives inside the permanently mapped
//! low 64 MiB window (identity at `0..0x0400_0000` and mirrored at
//! `KERNEL_VADDR_BASE`).  Every frame therefore has a kernel virtual address:
//!
//! ```text
//!   phys_to_kernel(pa) = KERNEL_VADDR_BASE + pa
//! ```
//!
//! That lets us read and write any page directory / page table directly
//! through that window, whether or not it is the *active* one.  This is the
//! same idea as a "physmap" on 64-bit kernels and keeps the code independent
//! of recursive-mapping tricks.
//!
//! Page tables are the classic two-level 4 KiB layout:
//!
//! ```text
//!   va[31:22] -> PDE index   (4 MiB region / pointer to a page table)
//!   va[21:12] -> PTE index   (4 KiB page inside the table)
//! ```
//!
//! The bootloader maps the low 64 MiB with 4 MiB pages (PSE).  Mapping a
//! single 4 KiB page inside such a region splits the PDE into a page table.

#![allow(dead_code)]

use core::arch::asm;

use crate::loader::KERNEL_VADDR_BASE;
use crate::mm::page_alloc;
use crate::mm::vm::{MapError, PageFlags, PagingArch, PAGE_KERNEL_RW, PAGE_SIZE};

const PTE_PRESENT: u32 = 1 << 0;
const PTE_WRITABLE: u32 = 1 << 1;
const PTE_USER: u32 = 1 << 2;
const PDE_PSE: u32 = 1 << 7; // 4 MiB page in a PDE

const PDE_COUNT: usize = 1024;
const PTE_COUNT: usize = 1024;

const PTE_PHYS_MASK: u32 = 0xffff_f000;

#[inline]
fn phys_to_kernel(pa: usize) -> usize {
    KERNEL_VADDR_BASE as usize + pa
}

#[inline]
fn kernel_to_phys(kva: usize) -> usize {
    kva - KERNEL_VADDR_BASE as usize
}

/// Read/write a `u32` at a kernel virtual address.
#[inline]
unsafe fn ld(addr: usize) -> u32 {
    core::ptr::read_volatile(addr as *const u32)
}

#[inline]
unsafe fn st(addr: usize, val: u32) {
    core::ptr::write_volatile(addr as *mut u32, val);
}

/// Allocate one zeroed physical frame and return its physical address.
fn alloc_zero_frame() -> Option<usize> {
    let ptr = page_alloc().get_page(1)?;
    unsafe { core::ptr::write_bytes(ptr, 0, PAGE_SIZE) };
    Some(kernel_to_phys(ptr as usize))
}

fn free_frame(pa: usize) {
    page_alloc().free_page(phys_to_kernel(pa) as *mut u8, 1);
}

fn entry_flags(flags: PageFlags) -> u32 {
    let mut e = PTE_PRESENT;
    if flags.writable {
        e |= PTE_WRITABLE;
    }
    if flags.user {
        e |= PTE_USER;
    }
    e
}

/// Marker type implementing [`PagingArch`] for i686.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct X86Paging;

impl PagingArch for X86Paging {
    type Root = usize; // physical address of the page directory

    fn active_root() -> usize {
        super::cr3() as usize
    }

    fn switch_to(root: usize) {
        unsafe {
            asm!("mov cr3, eax", in("eax") root as u32, options(nomem, nostack));
        }
    }

    fn map(root: usize, va: usize, pa: usize, flags: PageFlags) -> Result<(), MapError> {
        unsafe {
            let pdi = (va >> 22) & 0x3ff;
            let pti = (va >> 12) & 0x3ff;
            debug_assert_eq!(va & 0xfff, 0);
            debug_assert_eq!(pa & 0xfff, 0);
            debug_assert_eq!(root & 0xfff, 0);

            let pd = phys_to_kernel(root);
            let mut pde = ld(pd + pdi * 4);

            // True when the target slot comes straight from a just-split huge
            // page.  Splitting reproduces the huge page in a page table, so
            // the slot is a synthetic copy: we must be allowed to replace it.
            let mut from_split = false;

            if pde & PTE_PRESENT == 0 {
                // Need a new page table.  Its PDE gets the requested access so
                // that user pages are reachable from ring 3 later on.
                let pt = alloc_zero_frame().ok_or(MapError::OutOfMemory)?;
                pde = pt as u32 | entry_flags(flags);
                st(pd + pdi * 4, pde);
            } else if pde & PDE_PSE != 0 {
                // The 4 MiB region is a huge page; split it into a page table.
                split_huge(pd, pdi, pde)?;
                from_split = true;
                pde = ld(pd + pdi * 4);
            }

            let pt_va = phys_to_kernel((pde & PTE_PHYS_MASK) as usize);
            let old = ld(pt_va + pti * 4);
            if !from_split && old & PTE_PRESENT != 0 && (old & PTE_PHYS_MASK) as usize != pa {
                return Err(MapError::AlreadyMapped);
            }

            st(pt_va + pti * 4, pa as u32 | entry_flags(flags));
            flush_tlb(va);
            Ok(())
        }
    }

    fn unmap(root: usize, va: usize) -> Result<usize, MapError> {
        unsafe {
            let pdi = (va >> 22) & 0x3ff;
            let pti = (va >> 12) & 0x3ff;
            let pd = phys_to_kernel(root);
            let pde = ld(pd + pdi * 4);
            if pde & PTE_PRESENT == 0 {
                return Err(MapError::NotMapped);
            }
            if pde & PDE_PSE != 0 {
                split_huge(pd, pdi, pde)?;
            }
            let pde = ld(pd + pdi * 4);
            let pt_va = phys_to_kernel((pde & PTE_PHYS_MASK) as usize);
            let pte = ld(pt_va + pti * 4);
            if pte & PTE_PRESENT == 0 {
                return Err(MapError::NotMapped);
            }
            st(pt_va + pti * 4, 0);
            flush_tlb(va);
            Ok((pte & PTE_PHYS_MASK) as usize)
        }
    }

    fn translate(root: usize, va: usize) -> Option<usize> {
        unsafe {
            let pdi = (va >> 22) & 0x3ff;
            let pti = (va >> 12) & 0x3ff;
            let pd = phys_to_kernel(root);
            let pde = ld(pd + pdi * 4);
            if pde & PTE_PRESENT == 0 {
                return None;
            }
            if pde & PDE_PSE != 0 {
                // 4 MiB page: physical base is the top 10 bits of the PDE.
                return Some(((pde & 0xffc0_0000) as usize) + (va & 0x3f_ffff));
            }
            let pt_va = phys_to_kernel((pde & PTE_PHYS_MASK) as usize);
            let pte = ld(pt_va + pti * 4);
            if pte & PTE_PRESENT == 0 {
                return None;
            }
            Some((pte & PTE_PHYS_MASK) as usize)
        }
    }
}

/// Turn a present 4 MiB huge-page PDE into a page table that reproduces it at
/// 4 KiB granularity, then returns.
///
/// # Safety
/// `pd` is the kernel virtual address of an active-or-not page directory and
/// `pdi` indexes a present PDE with the PSE bit set.
unsafe fn split_huge(pd: usize, pdi: usize, pde: u32) -> Result<(), MapError> {
    let base = (pde & 0xffc0_0000) as usize;
    let flags = pde & (PTE_PRESENT | PTE_WRITABLE | PTE_USER);
    let pt = alloc_zero_frame().ok_or(MapError::OutOfMemory)?;
    let pt_va = phys_to_kernel(pt);

    for i in 0..PTE_COUNT {
        st(pt_va + i * 4, (base + i * PAGE_SIZE) as u32 | flags);
    }
    st(pd + pdi * 4, pt as u32 | flags);
    Ok(())
}

fn flush_tlb(va: usize) {
    unsafe {
        asm!("invlpg [{0}]", in(reg) va, options(nomem, nostack, preserves_flags));
    }
}

/// Allocate a fresh address space whose kernel region mirrors the currently
/// active one (so switching to it keeps kernel code/data reachable).  User
/// region PDEs are left empty.
///
/// Returns the physical address of the new page directory, or `None` if a
/// frame could not be allocated.
pub fn create_kernel_address_space() -> Option<usize> {
    unsafe {
        let pd = alloc_zero_frame()?;
        let dst = phys_to_kernel(pd);
        let src = phys_to_kernel(X86Paging::active_root());
        for i in 0..PDE_COUNT {
            let e = ld(src + i * 4);
            if e != 0 {
                st(dst + i * 4, e);
            }
        }
        Some(pd)
    }
}

/// Allocate one physical frame and map it at kernel virtual address `va` in
/// the *active* address space.  Used by the kernel heap to grow into its
/// reserved virtual region.  Returns true on success.
pub fn map_kernel_frame(va: usize) -> bool {
    if va & 0xfff != 0 {
        return false;
    }
    let pa = match alloc_zero_frame() {
        Some(pa) => pa,
        None => return false,
    };
    if X86Paging::map(X86Paging::active_root(), va, pa, PAGE_KERNEL_RW).is_err() {
        free_frame(pa);
        return false;
    }
    true
}

/// Destroy an address space created by [`create_kernel_address_space`].
///
/// Only page tables in the user half (PDE index < 768) are reclaimed.  Kernel
/// region entries (identity + mirror + heap, PDE >= 768 or huge pages) are
/// shared with the kernel address space and must stay alive.
pub fn destroy_address_space(root: usize) {
    unsafe {
        let pd = phys_to_kernel(root);
        for pdi in 0..768 {
            let pde = ld(pd + pdi * 4);
            if pde & PTE_PRESENT != 0 && pde & PDE_PSE == 0 {
                // A private 4 KiB page table (kernel regions are huge 4 MiB
                // pages or live at PDE >= 768 and are skipped above).
                free_frame((pde & PTE_PHYS_MASK) as usize);
            }
        }
        free_frame(root);
    }
}
