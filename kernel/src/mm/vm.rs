//! Architecture-neutral virtual memory interface.
//!
//! Kernel code that deals with address spaces (per-process isolation, heap
//! growth, DMA buffers, ...) goes through [`PagingArch`] + [`AddressSpace`]
//! and never touches page-table formats directly.  Each architecture provides
//! one implementation of [`PagingArch`]:
//!
//! - i686:   `crate::arch::x86::paging::X86Paging`
//! - x86-64 / RISC-V: add a backend module behind a `cfg` in `arch/mod.rs`
//!
//! Addresses are plain `usize` for now (32-bit kernel).  On a 64-bit port the
//! implementations simply use wider ranges; the API does not change.

#![allow(dead_code)]

pub const PAGE_SIZE: usize = 4096;

/// Access permissions for a mapped page.  Semantics are arch-mapped:
/// `user` controls user/supervisor, `writable` the read/write bit, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFlags {
    pub writable: bool,
    pub user: bool,
}

/// Supervisor read/write (the common kernel mapping).
pub const PAGE_KERNEL_RW: PageFlags = PageFlags {
    writable: true,
    user: false,
};

/// User read/write.
pub const PAGE_USER_RW: PageFlags = PageFlags {
    writable: true,
    user: true,
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
/// `Root` is an opaque handle to a page-table root (for i686: the physical
/// address of the page directory).
pub trait PagingArch: Sized {
    type Root: Copy + PartialEq + core::fmt::Debug;

    /// The page-table root currently active on this CPU.
    fn active_root() -> Self::Root;

    /// Activate `root` (i.e. load CR3 / satp / TTBR).
    fn switch_to(root: Self::Root);

    /// Map the 4 KiB page at `va` to the physical page `pa`.
    ///
    /// Fails with `AlreadyMapped` if `va` is already mapped to a different
    /// physical page.
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
