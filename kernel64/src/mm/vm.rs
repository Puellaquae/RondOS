//! Architecture-neutral virtual memory interface (ported from the i686 kernel).
//!
//! Kernel code that deals with address spaces (per-process isolation, heap
//! growth, DMA buffers, device mappings, ...) goes through [`PagingArch`] +
//! [`AddressSpace`] and never touches page-table formats directly.
//!
//! Compared to the i686 version the flags grew two dimensions that only exist
//! (or only became usable) on x86-64:
//!
//! * `executable` — the NX bit.  `executable: false` means NX=1.  This is what
//!   makes W^X real: data/stack pages are never executable.
//! * `cache` — PTE PCD/PWT.  Needed to map the GOP framebuffer and other MMIO
//!   write-combined / uncached.

#![allow(dead_code)]

pub const PAGE_SIZE: usize = 4096;

/// Memory type requested for a mapping (PTE `PWT`/`PCD` bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePolicy {
    /// Normal cacheable memory (default).
    WriteBack,
    /// Write-through.
    WriteThrough,
    /// Uncached — for MMIO registers.
    Uncached,
    /// Write-combining — for framebuffers.
    WriteCombining,
}

/// Access permissions for a mapped page.  Semantics are arch-mapped:
/// `user` controls user/supervisor, `writable` the read/write bit,
/// `executable` the NX bit, `cache` the memory type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFlags {
    pub writable: bool,
    pub user: bool,
    pub executable: bool,
    pub cache: CachePolicy,
}

pub const PAGE_KERNEL_RW: PageFlags = PageFlags {
    writable: true,
    user: false,
    executable: false,
    cache: CachePolicy::WriteBack,
};

pub const PAGE_KERNEL_RX: PageFlags = PageFlags {
    writable: false,
    user: false,
    executable: true,
    cache: CachePolicy::WriteBack,
};

pub const PAGE_USER_RW: PageFlags = PageFlags {
    writable: true,
    user: true,
    executable: false,
    cache: CachePolicy::WriteBack,
};

pub const PAGE_USER_RX: PageFlags = PageFlags {
    writable: false,
    user: true,
    executable: true,
    cache: CachePolicy::WriteBack,
};

/// Errors returned by mapping operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    /// Could not allocate a frame (page table / page) for the operation.
    OutOfMemory,
    /// The virtual address was already mapped to a different page.
    AlreadyMapped,
    /// The virtual address was not mapped.
    NotMapped,
}

/// Interface implemented by each architecture's paging hardware.
///
/// `Root` is an opaque handle to a page-table root (for x86-64: the physical
/// address of the PML4).
pub trait PagingArch: Sized {
    type Root: Copy + PartialEq + core::fmt::Debug;

    /// The page-table root currently active on this CPU.
    fn active_root() -> Self::Root;

    /// Activate `root` (i.e. load CR3 / satp / TTBR).
    fn switch_to(root: Self::Root);

    /// Map the 4 KiB page at `va` to the physical page `pa`.
    ///
    /// Fails with `AlreadyMapped` if `va` is already mapped to a different
    /// physical page.  Splits huge pages transparently if needed.
    fn map(root: Self::Root, va: usize, pa: usize, flags: PageFlags) -> Result<(), MapError>;

    /// Remove the mapping of `va`, returning the physical page it pointed to.
    fn unmap(root: Self::Root, va: usize) -> Result<usize, MapError>;

    /// Translate `va` through `root`; returns the physical address if mapped.
    fn translate(root: Self::Root, va: usize) -> Option<usize>;
}

/// A wrapper around one page-table root exposing a convenient API.
pub struct AddressSpace<A: PagingArch> {
    root: A::Root,
}

impl<A: PagingArch> AddressSpace<A> {
    pub const fn new(root: A::Root) -> Self {
        Self { root }
    }

    pub fn root(&self) -> A::Root {
        self.root
    }

    pub fn activate(&self) {
        A::switch_to(self.root);
    }

    pub fn map(&mut self, va: usize, pa: usize, flags: PageFlags) -> Result<(), MapError> {
        A::map(self.root, va, pa, flags)
    }

    pub fn unmap(&mut self, va: usize) -> Result<usize, MapError> {
        A::unmap(self.root, va)
    }

    pub fn translate(&self, va: usize) -> Option<usize> {
        A::translate(self.root, va)
    }
}
