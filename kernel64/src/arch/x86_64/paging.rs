//! x86-64 4-level paging backend — M0.2.
//!
//! This is the x86-64 replacement for `kernel/src/arch/x86/paging.rs`.  It
//! implements the same [`PagingArch`] trait, so the architecture-neutral VMM
//! in `mm/vm.rs` did not have to change; what did change:
//!
//! ```text
//!   PML4[va 47:39] -> PDPT[va 38:30] -> PD[va 29:21] -> PT[va 20:12] -> page
//! ```
//!
//! * entries are `u64`, the address mask is 52 bits wide;
//! * **NX** (bit 63) is real, so `PageFlags::executable = false` actually
//!   prevents execution — W^X, impossible on i686 without PAE;
//! * **PCD/PWT** (bits 4/3) carry the `CachePolicy`, needed for the GOP
//!   framebuffer and other MMIO;
//! * huge pages exist at *two* levels (1 GiB in a PDPT entry, 2 MiB in a PD
//!   entry), so `map`/`unmap` may have to split twice.
//!
//! All page tables are reached through the **physmap**
//! (`phys_to_virt(pa) = 0xFFFF_8000_0000_0000 + pa`), which the boot
//! trampoline installs with 1 GiB pages for the first 4 GiB of RAM.  That
//! makes a non-active address space just as easy to edit as the active one.
//!
//! The boot trampoline maps the kernel image linearly at
//! `0xFFFF_FFFF_8000_0000 + pa` and the physmap at `0xFFFF_8000_0000_0000 + pa`.
//! Both aliases exist for low physical memory; **frames are always addressed
//! through the physmap**.

#![allow(dead_code)]

use crate::mm::page_alloc;
use crate::mm::vm::{CachePolicy, MapError, PageFlags, PagingArch, PAGE_SIZE};

/// Base of the physmap window (`phys_to_virt`).
pub const PHYS_MAP_BASE: usize = 0xFFFF_8000_0000_0000;
/// Base the kernel image is linked at (`VA = KERNEL_VIRT_BASE + PA`).
pub const KERNEL_VIRT_BASE: usize = 0xFFFF_FFFF_8000_0000;
/// First VA of the kernel half: everything below belongs to user space.
pub const USER_VA_LIMIT: usize = 0x0000_8000_0000_0000;

/// True when `[va, va+len)` is a legal user-space range.
///
/// This is a **security boundary**, not a sanity check: a process's PML4 kernel
/// half is *shared* with the kernel root, so mapping a user page at a kernel VA
/// would rewrite the kernel's own page tables (and vice versa).  Every path
/// that takes a VA from user-controlled input (ELF `p_vaddr`, `mmap` cursors,
/// stack placement) must go through this.
pub fn valid_user_range(va: usize, len: usize) -> bool {
    len != 0 && va < USER_VA_LIMIT && va.checked_add(len).is_some_and(|end| end <= USER_VA_LIMIT)
}
/// How much RAM the boot trampoline mapped in the physmap (4 x 1 GiB pages).
pub const PHYS_MAP_LIMIT: usize = 0x1_0000_0000;

pub const HUGE_1G: usize = 1 << 30;
pub const HUGE_2M: usize = 1 << 21;

const ENTRIES: usize = 512;

const PTE_PRESENT: u64 = 1 << 0;
const PTE_WRITABLE: u64 = 1 << 1;
const PTE_USER: u64 = 1 << 2;
const PTE_PWT: u64 = 1 << 3;
const PTE_PCD: u64 = 1 << 4;
const PTE_HUGE: u64 = 1 << 7;
const PTE_NX: u64 = 1 << 63;

const PTE_PHYS_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const PDPT_1G_MASK: u64 = 0x000F_FFFF_C000_0000;
const PD_2M_MASK: u64 = 0x000F_FFFF_FFE0_0000;

#[inline]
pub fn phys_to_virt(pa: usize) -> usize {
    PHYS_MAP_BASE + pa
}

#[inline]
pub fn virt_to_phys(va: usize) -> usize {
    va - PHYS_MAP_BASE
}

#[inline]
fn pml4_idx(va: usize) -> usize {
    (va >> 39) & 0x1ff
}

#[inline]
fn pdpt_idx(va: usize) -> usize {
    (va >> 30) & 0x1ff
}

#[inline]
fn pd_idx(va: usize) -> usize {
    (va >> 21) & 0x1ff
}

#[inline]
fn pt_idx(va: usize) -> usize {
    (va >> 12) & 0x1ff
}

/// Read/write a table entry.  `table_pa` is the physical address of the table.
#[inline]
unsafe fn ld(table_pa: usize, idx: usize) -> u64 {
    core::ptr::read_volatile((phys_to_virt(table_pa) + idx * 8) as *const u64)
}

#[inline]
unsafe fn st(table_pa: usize, idx: usize, val: u64) {
    core::ptr::write_volatile((phys_to_virt(table_pa) + idx * 8) as *mut u64, val);
}

#[inline]
fn present(e: u64) -> bool {
    (e & PTE_PRESENT) != 0
}

/// Allocate one zeroed physical frame, returning its physical address.
fn alloc_zero_frame() -> Option<usize> {
    let ptr = page_alloc().get_page(1)?;
    unsafe { core::ptr::write_bytes(ptr, 0, PAGE_SIZE) };
    Some(virt_to_phys(ptr as usize))
}

fn free_frame(pa: usize) {
    page_alloc().free_page(phys_to_virt(pa) as *mut u8, 1);
}

fn entry_flags(flags: PageFlags) -> u64 {
    let mut e = PTE_PRESENT;
    if flags.writable {
        e |= PTE_WRITABLE;
    }
    if flags.user {
        e |= PTE_USER;
    }
    if !flags.executable {
        e |= PTE_NX;
    }
    match flags.cache {
        CachePolicy::WriteBack => {}
        CachePolicy::WriteThrough => e |= PTE_PWT,
        CachePolicy::Uncached => e |= PTE_PCD,
        CachePolicy::WriteCombining => e |= PTE_PWT | PTE_PCD,
    }
    e
}

/// Flags for intermediate tables: permissive on purpose (the leaf PTE is what
/// decides access), but they must carry U/S so user pages are reachable.
fn table_flags(flags: PageFlags) -> u64 {
    let mut e = PTE_PRESENT | PTE_WRITABLE;
    if flags.user {
        e |= PTE_USER;
    }
    e
}

/// Access/attribute bits worth preserving when splitting a huge page.
const SPLIT_KEEP: u64 = PTE_WRITABLE | PTE_USER | PTE_PWT | PTE_PCD | PTE_NX;

/// Split a 1 GiB PDPT entry into a page directory of 2 MiB pages.
unsafe fn split_1g(pdpt_pa: usize, idx: usize, entry: u64) -> Result<(), MapError> {
    let base = (entry & PDPT_1G_MASK) as usize;
    let keep = entry & SPLIT_KEEP;
    let pd = alloc_zero_frame().ok_or(MapError::OutOfMemory)?;
    for i in 0..ENTRIES {
        st(pd, i, (base + i * HUGE_2M) as u64 | keep | PTE_PRESENT | PTE_HUGE);
    }
    st(pdpt_pa, idx, pd as u64 | keep | PTE_PRESENT);
    Ok(())
}

/// Split a 2 MiB PD entry into a page table of 4 KiB pages.
unsafe fn split_2m(pd_pa: usize, idx: usize, entry: u64) -> Result<(), MapError> {
    let base = (entry & PD_2M_MASK) as usize;
    let keep = entry & SPLIT_KEEP;
    let pt = alloc_zero_frame().ok_or(MapError::OutOfMemory)?;
    for i in 0..ENTRIES {
        st(pt, i, (base + i * PAGE_SIZE) as u64 | keep | PTE_PRESENT);
    }
    st(pd_pa, idx, pt as u64 | keep | PTE_PRESENT);
    Ok(())
}

#[inline]
fn flush_for(root: usize, va: usize) {
    if root == X86_64Paging::active_root() {
        super::invlpg(va);
    }
}

#[inline]
fn flush_all_for(root: usize) {
    if root == X86_64Paging::active_root() {
        super::flush_tlb();
    }
}

/// Marker type implementing [`PagingArch`] for x86-64.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct X86_64Paging;

impl PagingArch for X86_64Paging {
    type Root = usize; // physical address of the PML4

    fn active_root() -> usize {
        super::cr3() as usize
    }

    fn switch_to(root: usize) {
        super::set_cr3(root as u64);
    }

    fn map(root: usize, va: usize, pa: usize, flags: PageFlags) -> Result<(), MapError> {
        debug_assert_eq!(va & 0xfff, 0);
        debug_assert_eq!(pa & 0xfff, 0);
        debug_assert_eq!(root & 0xfff, 0);

        unsafe {
            let i4 = pml4_idx(va);
            let mut e4 = ld(root, i4);
            let pdpt = if present(e4) {
                (e4 & PTE_PHYS_MASK) as usize
            } else {
                let t = alloc_zero_frame().ok_or(MapError::OutOfMemory)?;
                e4 = t as u64 | table_flags(flags);
                st(root, i4, e4);
                t
            };

            let i3 = pdpt_idx(va);
            let e3 = ld(pdpt, i3);
            let pd = if present(e3) {
                if (e3 & PTE_HUGE) != 0 {
                    split_1g(pdpt, i3, e3)?;
                    flush_all_for(root);
                    (ld(pdpt, i3) & PTE_PHYS_MASK) as usize
                } else {
                    (e3 & PTE_PHYS_MASK) as usize
                }
            } else {
                let t = alloc_zero_frame().ok_or(MapError::OutOfMemory)?;
                st(pdpt, i3, t as u64 | table_flags(flags));
                t
            };

            let i2 = pd_idx(va);
            let e2 = ld(pd, i2);
            // True when this page table was just synthesized by splitting a
            // 2 MiB page: its entries are copies of the huge page, so we are
            // allowed to replace the one we are mapping (same idea as the
            // i686 backend's `from_split`).
            let mut from_split = false;
            let pt = if present(e2) {
                if (e2 & PTE_HUGE) != 0 {
                    split_2m(pd, i2, e2)?;
                    flush_all_for(root);
                    from_split = true;
                    (ld(pd, i2) & PTE_PHYS_MASK) as usize
                } else {
                    (e2 & PTE_PHYS_MASK) as usize
                }
            } else {
                let t = alloc_zero_frame().ok_or(MapError::OutOfMemory)?;
                st(pd, i2, t as u64 | table_flags(flags));
                t
            };

            let i1 = pt_idx(va);
            let old = ld(pt, i1);
            if !from_split && present(old) && (old & PTE_PHYS_MASK) as usize != pa {
                return Err(MapError::AlreadyMapped);
            }
            st(pt, i1, pa as u64 | entry_flags(flags));
            flush_for(root, va);
            Ok(())
        }
    }

    fn unmap(root: usize, va: usize) -> Result<usize, MapError> {
        debug_assert_eq!(va & 0xfff, 0);
        unsafe {
            let e4 = ld(root, pml4_idx(va));
            if !present(e4) {
                return Err(MapError::NotMapped);
            }
            let pdpt = (e4 & PTE_PHYS_MASK) as usize;

            let e3 = ld(pdpt, pdpt_idx(va));
            if !present(e3) {
                return Err(MapError::NotMapped);
            }
            let pd = if (e3 & PTE_HUGE) != 0 {
                split_1g(pdpt, pdpt_idx(va), e3)?;
                flush_all_for(root);
                (ld(pdpt, pdpt_idx(va)) & PTE_PHYS_MASK) as usize
            } else {
                (e3 & PTE_PHYS_MASK) as usize
            };

            let e2 = ld(pd, pd_idx(va));
            if !present(e2) {
                return Err(MapError::NotMapped);
            }
            let pt = if (e2 & PTE_HUGE) != 0 {
                split_2m(pd, pd_idx(va), e2)?;
                flush_all_for(root);
                (ld(pd, pd_idx(va)) & PTE_PHYS_MASK) as usize
            } else {
                (e2 & PTE_PHYS_MASK) as usize
            };

            let i1 = pt_idx(va);
            let e1 = ld(pt, i1);
            if !present(e1) {
                return Err(MapError::NotMapped);
            }
            st(pt, i1, 0);
            flush_for(root, va);
            Ok((e1 & PTE_PHYS_MASK) as usize)
        }
    }

    fn translate(root: usize, va: usize) -> Option<usize> {
        unsafe {
            let e4 = ld(root, pml4_idx(va));
            if !present(e4) {
                return None;
            }
            let pdpt = (e4 & PTE_PHYS_MASK) as usize;

            let e3 = ld(pdpt, pdpt_idx(va));
            if !present(e3) {
                return None;
            }
            if (e3 & PTE_HUGE) != 0 {
                return Some((e3 & PDPT_1G_MASK) as usize + (va & (HUGE_1G - 1)));
            }
            let pd = (e3 & PTE_PHYS_MASK) as usize;

            let e2 = ld(pd, pd_idx(va));
            if !present(e2) {
                return None;
            }
            if (e2 & PTE_HUGE) != 0 {
                return Some((e2 & PD_2M_MASK) as usize + (va & (HUGE_2M - 1)));
            }
            let pt = (e2 & PTE_PHYS_MASK) as usize;

            let e1 = ld(pt, pt_idx(va));
            if !present(e1) {
                return None;
            }
            Some((e1 & PTE_PHYS_MASK) as usize + (va & 0xfff))
        }
    }
}

/// What a translation actually found — used by tests and the fault handler.
#[derive(Debug, Clone, Copy)]
pub struct PageInfo {
    pub pa: usize,
    pub flags: PageFlags,
    /// True when the mapping is a 1 GiB or 2 MiB huge page.
    pub huge: bool,
}

fn flags_from_entry(e: u64) -> PageFlags {
    let cache = match ((e & PTE_PWT) != 0, (e & PTE_PCD) != 0) {
        (false, false) => CachePolicy::WriteBack,
        (true, false) => CachePolicy::WriteThrough,
        (false, true) => CachePolicy::Uncached,
        (true, true) => CachePolicy::WriteCombining,
    };
    PageFlags {
        writable: (e & PTE_WRITABLE) != 0,
        user: (e & PTE_USER) != 0,
        executable: (e & PTE_NX) == 0,
        cache,
    }
}

impl X86_64Paging {
    /// Full translation *including flags* (the trait only exposes the address).
    pub fn query(root: usize, va: usize) -> Option<PageInfo> {
        unsafe {
            let e4 = ld(root, pml4_idx(va));
            if !present(e4) {
                return None;
            }
            let pdpt = (e4 & PTE_PHYS_MASK) as usize;
            let e3 = ld(pdpt, pdpt_idx(va));
            if !present(e3) {
                return None;
            }
            if (e3 & PTE_HUGE) != 0 {
                return Some(PageInfo {
                    pa: (e3 & PDPT_1G_MASK) as usize + (va & (HUGE_1G - 1)),
                    flags: flags_from_entry(e3),
                    huge: true,
                });
            }
            let pd = (e3 & PTE_PHYS_MASK) as usize;
            let e2 = ld(pd, pd_idx(va));
            if !present(e2) {
                return None;
            }
            if (e2 & PTE_HUGE) != 0 {
                return Some(PageInfo {
                    pa: (e2 & PD_2M_MASK) as usize + (va & (HUGE_2M - 1)),
                    flags: flags_from_entry(e2),
                    huge: true,
                });
            }
            let pt = (e2 & PTE_PHYS_MASK) as usize;
            let e1 = ld(pt, pt_idx(va));
            if !present(e1) {
                return None;
            }
            Some(PageInfo {
                pa: (e1 & PTE_PHYS_MASK) as usize + (va & 0xfff),
                flags: flags_from_entry(e1),
                huge: false,
            })
        }
    }

    /// Install a single 1 GiB huge page (`va`/`pa` must be 1 GiB aligned).
    ///
    /// The physmap is built with these; the smoke test uses it to exercise the
    /// two-level split below without depending on the bootloader's tables.
    /// The target PDPT entry must be free — this never splits implicitly.
    pub fn map_huge_1g(root: usize, va: usize, pa: usize, flags: PageFlags) -> Result<(), MapError> {
        debug_assert_eq!(va & (HUGE_1G - 1), 0);
        debug_assert_eq!(pa & (HUGE_1G - 1), 0);
        unsafe {
            let i4 = pml4_idx(va);
            let e4 = ld(root, i4);
            let pdpt = if present(e4) {
                (e4 & PTE_PHYS_MASK) as usize
            } else {
                let t = alloc_zero_frame().ok_or(MapError::OutOfMemory)?;
                st(root, i4, t as u64 | table_flags(flags));
                t
            };
            let i3 = pdpt_idx(va);
            if present(ld(pdpt, i3)) {
                return Err(MapError::AlreadyMapped);
            }
            st(pdpt, i3, pa as u64 | entry_flags(flags) | PTE_HUGE);
            flush_for(root, va);
            Ok(())
        }
    }

    /// Map `len` bytes of physical MMIO at `va` with the given cache policy.
    /// Used later for the GOP framebuffer (design §8.2).
    pub fn map_device(
        root: usize,
        va: usize,
        pa: usize,
        len: usize,
        policy: CachePolicy,
    ) -> Result<(), MapError> {
        let flags = PageFlags {
            writable: true,
            user: false,
            executable: false,
            cache: policy,
        };
        let pages = (len + PAGE_SIZE - 1) / PAGE_SIZE;
        for i in 0..pages {
            Self::map(root, va + i * PAGE_SIZE, pa + i * PAGE_SIZE, flags)?;
        }
        Ok(())
    }
}

/// Allocate a fresh address space whose kernel half mirrors the active one.
///
/// **Only PML4 entries ≥ 256 (the kernel half) are copied.**  This is the fix
/// for the i686 bug where the identity map of low memory was copied into every
/// process and blocked the user address space (design §7.1).  The kernel half
/// must never change afterwards, which is why the kernel's page tables are
/// pre-created at boot.
pub fn create_kernel_address_space() -> Option<usize> {
    unsafe {
        let pml4 = alloc_zero_frame()?;
        let src = X86_64Paging::active_root();
        for i in 256..ENTRIES {
            let e = ld(src, i);
            if e != 0 {
                st(pml4, i, e);
            }
        }
        Some(pml4)
    }
}

/// Free the page tables of the **user half** of `root`.  Mapped data frames are
/// the caller's responsibility (VMA teardown, M0.3+); this only returns the
/// page-table frames so the smoke test does not leak them.
pub fn destroy_address_space(root: usize) {
    unsafe {
        for i4 in 0..256 {
            let e4 = ld(root, i4);
            if !present(e4) {
                continue;
            }
            let pdpt = (e4 & PTE_PHYS_MASK) as usize;
            for i3 in 0..ENTRIES {
                let e3 = ld(pdpt, i3);
                if !present(e3) {
                    continue;
                }
                if (e3 & PTE_HUGE) != 0 {
                    // Cannot happen for user mappings (we never create them),
                    // but be defensive.
                    continue;
                }
                let pd = (e3 & PTE_PHYS_MASK) as usize;
                for i2 in 0..ENTRIES {
                    let e2 = ld(pd, i2);
                    if !present(e2) || (e2 & PTE_HUGE) != 0 {
                        continue;
                    }
                    free_frame((e2 & PTE_PHYS_MASK) as usize);
                }
                free_frame(pd);
            }
            free_frame(pdpt);
        }
        free_frame(root);
    }
}

/// Drop the low identity map (PML4[0]) from the active address space.
///
/// The boot trampoline needs it; the kernel does not — everything is reachable
/// through the physmap, and removing it closes an aliasing hole.
pub fn remove_identity_map() {
    unsafe {
        let root = X86_64Paging::active_root();
        st(root, 0, 0);
        super::flush_tlb();
    }
}
