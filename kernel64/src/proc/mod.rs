//! Processes, address-space ownership, VMAs and handle tables — P0.
//!
//! Until M0.5 "a user context" was a one-shot blob entered by the smoke test.
//! P0 makes it a first-class object: a [`Process`] owns
//!
//! * an address space root (a PML4 whose kernel half mirrors the kernel's),
//! * a list of [`Vma`]s — the *only* authority for "may this user pointer be
//!   dereferenced?", which is what `copy_from_user`/`copy_to_user` consult,
//! * a [`HandleTable`] of capability handles (`index:32 | generation:32`),
//! * an exit status, so a parent (P1) can wait for it.
//!
//! P0 deliberately keeps it minimal: one user thread per process, no demand
//! paging (every VMA is backed at map time), no handle objects yet beyond the
//! table skeleton.  What is real is the *lifetime*: when the thread dies, the
//! process's pages and page tables are returned to the frame allocator, and a
//! faulting user context takes down only itself.

#![allow(dead_code)]

use core::cell::UnsafeCell;

use crate::arch::x86_64::paging::{destroy_address_space, phys_to_virt, X86_64Paging};
use crate::mm::vm::{PageFlags, PagingArch, PAGE_USER_RW};
use crate::mm::{page_alloc, PAGE_SIZE};
use rondos_abi::{Handle, ObjKind, Status};

pub const MAX_PROCS: usize = 16;
pub const MAX_VMAS: usize = 32;
pub const MAX_HANDLES: usize = 64;

const NO_PID: u32 = u32::MAX;

// ---------------------------------------------------------------------- VMA

/// One contiguous virtual range with uniform permissions.
#[derive(Clone, Copy, Debug)]
pub struct Vma {
    pub start: u64,
    pub end: u64,
    pub flags: PageFlags,
}

impl Vma {
    pub const fn new(start: u64, end: u64, flags: PageFlags) -> Self {
        Self { start, end, flags }
    }

    /// Page-aligned size in bytes.
    pub const fn len(&self) -> u64 {
        self.end - self.start
    }

    #[inline]
    pub const fn contains_range(&self, va: u64, len: u64) -> bool {
        match va.checked_add(len) {
            Some(end) => va >= self.start && end <= self.end,
            None => false,
        }
    }
}

/// Fixed-capacity, insertion-ordered VMA list (no heap in P0).
#[derive(Debug)]
pub struct VmaList {
    slots: [Option<Vma>; MAX_VMAS],
    len: usize,
}

impl VmaList {
    /// `const` so the process table can live in `.bss` with no stack temporary
    /// (a `Default` impl would materialise the whole table on the stack first).
    pub const fn new() -> Self {
        Self {
            slots: [None; MAX_VMAS],
            len: 0,
        }
    }

    pub fn insert(&mut self, vma: Vma) -> Result<(), Status> {
        if vma.end <= vma.start || vma.start % PAGE_SIZE as u64 != 0 {
            return Err(Status::InvalidArgument);
        }
        for v in self.iter() {
            if vma.start < v.end && v.start < vma.end {
                return Err(Status::InvalidArgument); // overlap
            }
        }
        let slot = self.slots.iter_mut().find(|s| s.is_none());
        match slot {
            Some(s) => {
                *s = Some(vma);
                self.len += 1;
                Ok(())
            }
            None => Err(Status::OutOfMemory),
        }
    }

    pub fn remove(&mut self, start: u64) -> Option<Vma> {
        for s in self.slots.iter_mut() {
            if let Some(v) = s {
                if v.start == start {
                    let v = *v;
                    *s = None;
                    self.len -= 1;
                    return Some(v);
                }
            }
        }
        None
    }

    pub fn iter(&self) -> impl Iterator<Item = &Vma> {
        self.slots.iter().filter_map(|s| s.as_ref())
    }

    pub fn find(&self, va: u64) -> Option<&Vma> {
        self.iter().find(|v| va >= v.start && va < v.end)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// The user-pointer check behind `copy_from_user`/`copy_to_user`: the whole
    /// `[va, va+len)` range must live in **one** VMA, and a write must land in
    /// a writable one.  No partial copies across VMA boundaries.
    pub fn check(&self, va: u64, len: u64, need_write: bool) -> Result<&Vma, Status> {
        if len == 0 {
            return Err(Status::InvalidArgument);
        }
        let vma = self.find(va).ok_or(Status::BadAddress)?;
        if !vma.contains_range(va, len) {
            return Err(Status::BadAddress);
        }
        if need_write && !vma.flags.writable {
            return Err(Status::Permission);
        }
        Ok(vma)
    }
}

// -------------------------------------------------------------- handle table

#[derive(Clone, Copy, Debug)]
pub struct HandleSlot {
    pub kind: ObjKind,
    pub id: u32,
    pub rights: u64,
    pub generation: u32,
    used: bool,
}

impl HandleSlot {
    const fn empty() -> Self {
        Self {
            kind: ObjKind::None,
            id: 0,
            rights: 0,
            generation: 0,
            used: false,
        }
    }

    pub fn allows(&self, rights: u64) -> bool {
        self.rights & rights == rights
    }
}

/// Per-process capability table.  Handles are unforgeable because the
/// generation is checked: closing bumps it, so a stale `Handle` from another
/// process (or from before a `close`) fails with `Status::BadHandle`.
#[derive(Debug)]
pub struct HandleTable {
    slots: [HandleSlot; MAX_HANDLES],
}

impl HandleTable {
    pub const fn new() -> Self {
        Self {
            slots: [HandleSlot::empty(); MAX_HANDLES],
        }
    }

    pub fn insert(&mut self, kind: ObjKind, id: u32, rights: u64) -> Option<Handle> {
        for (i, s) in self.slots.iter_mut().enumerate() {
            if !s.used {
                s.used = true;
                s.kind = kind;
                s.id = id;
                s.rights = rights;
                return Some(Handle::new(i as u32, s.generation));
            }
        }
        None
    }

    pub fn get(&self, h: Handle) -> Result<&HandleSlot, Status> {
        let slot = self
            .slots
            .get(h.index() as usize)
            .ok_or(Status::BadHandle)?;
        if !slot.used || slot.generation != h.generation() {
            return Err(Status::BadHandle);
        }
        Ok(slot)
    }

    /// Resolve and check rights in one step — the only way kernel code should
    /// turn a raw handle into an object.
    pub fn resolve(&self, h: Handle, rights: u64) -> Result<&HandleSlot, Status> {
        let slot = self.get(h)?;
        if !slot.allows(rights) {
            return Err(Status::Permission);
        }
        Ok(slot)
    }

    pub fn close(&mut self, h: Handle) -> Result<(), Status> {
        let slot = self
            .slots
            .get_mut(h.index() as usize)
            .ok_or(Status::BadHandle)?;
        if !slot.used || slot.generation != h.generation() {
            return Err(Status::BadHandle);
        }
        // Bump the generation so the closed handle value can never resolve.
        let gen = slot.generation.wrapping_add(1);
        *slot = HandleSlot {
            generation: gen,
            ..HandleSlot::empty()
        };
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.slots.iter().filter(|s| s.used).count()
    }

    pub fn clear(&mut self) {
        for s in self.slots.iter_mut() {
            *s = HandleSlot::empty();
        }
    }
}

// ------------------------------------------------------------------ process

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExitStatus {
    Running,
    Exited(u32),
    Fault {
        vector: u32,
        rip: u64,
        addr: u64,
    },
    Killed,
}

pub struct Process {
    used: bool,
    pid: u32,
    parent: u32,
    /// PML4 physical address (kernel half copied from the kernel root).
    root: usize,
    vmas: VmaList,
    handles: HandleTable,
    status: ExitStatus,
    /// P0: exactly one user thread; `NO_PID` when none is attached.
    thread: u32,
    /// Frames returned to the allocator when the process died (diagnostics).
    freed_pages: u64,
}

impl Process {
    pub const fn new() -> Self {
        Self {
            used: false,
            pid: NO_PID,
            parent: NO_PID,
            root: 0,
            vmas: VmaList::new(),
            handles: HandleTable::new(),
            status: ExitStatus::Running,
            thread: NO_PID,
            freed_pages: 0,
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn root(&self) -> usize {
        self.root
    }

    pub fn status(&self) -> ExitStatus {
        self.status
    }

    pub fn vmas(&self) -> &VmaList {
        &self.vmas
    }

    pub fn handles(&self) -> &HandleTable {
        &self.handles
    }

    pub fn handles_mut(&mut self) -> &mut HandleTable {
        &mut self.handles
    }

    pub fn thread(&self) -> Option<u32> {
        if self.thread == NO_PID {
            None
        } else {
            Some(self.thread)
        }
    }

    pub fn freed_pages(&self) -> u64 {
        self.freed_pages
    }

    /// Map `len` bytes of fresh zeroed frames at `va` with `flags`, recording a
    /// VMA.  This is the P0 "everything is backed eagerly" model; demand paging
    /// arrives in P5.
    pub fn map_anon(&mut self, va: u64, len: u64, flags: PageFlags) -> Result<(), Status> {
        if va % PAGE_SIZE as u64 != 0 || len == 0 || len % PAGE_SIZE as u64 != 0 {
            return Err(Status::InvalidArgument);
        }
        let pages = (len / PAGE_SIZE as u64) as usize;
        let mut mapped = 0usize;
        for i in 0..pages {
            let frame = match page_alloc().get_page(1) {
                Some(f) => f,
                None => {
                    self.unmap_range(va, (mapped * PAGE_SIZE) as u64);
                    return Err(Status::OutOfMemory);
                }
            };
            unsafe { core::ptr::write_bytes(frame, 0, PAGE_SIZE) };
            let pa = crate::arch::x86_64::paging::virt_to_phys(frame as usize);
            if X86_64Paging::map(self.root, (va as usize) + i * PAGE_SIZE, pa, flags).is_err() {
                page_alloc().free_page(frame, 1);
                self.unmap_range(va, (mapped * PAGE_SIZE) as u64);
                return Err(Status::OutOfMemory);
            }
            mapped += 1;
        }
        self.vmas.insert(Vma::new(va, va + len, flags))
    }

    /// Copy `src` into freshly mapped pages at `va` (code/data initialisation).
    pub fn map_blob(&mut self, va: u64, src: &[u8], flags: PageFlags) -> Result<(), Status> {
        let len = ((src.len() as u64) + PAGE_SIZE as u64 - 1) & !(PAGE_SIZE as u64 - 1);
        self.map_anon(va, len, flags)?;
        // The pages are mapped in this address space, but the kernel can write
        // them through the physmap instead of depending on being active.
        for (i, chunk) in src.chunks(PAGE_SIZE).enumerate() {
            if let Some(pa) = X86_64Paging::translate(self.root, (va as usize) + i * PAGE_SIZE) {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        chunk.as_ptr(),
                        phys_to_virt(pa) as *mut u8,
                        chunk.len(),
                    )
                };
            }
        }
        Ok(())
    }

    fn unmap_range(&mut self, va: u64, len: u64) {
        let mut off = 0;
        while off < len {
            if let Ok(pa) = X86_64Paging::unmap(self.root, (va + off) as usize) {
                page_alloc().free_page(phys_to_virt(pa) as *mut u8, 1);
            }
            off += PAGE_SIZE as u64;
        }
    }

    pub fn copy_from_user(&self, dst: &mut [u8], src_va: u64) -> Result<(), Status> {
        self.vmas.check(src_va, dst.len() as u64, false)?;
        unsafe {
            core::ptr::copy_nonoverlapping(src_va as *const u8, dst.as_mut_ptr(), dst.len())
        };
        Ok(())
    }

    pub fn copy_to_user(&self, dst_va: u64, src: &[u8]) -> Result<(), Status> {
        self.vmas.check(dst_va, src.len() as u64, true)?;
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), dst_va as *mut u8, src.len())
        };
        Ok(())
    }

    /// Mark the process dead.  Nothing is freed here: the caller may still be
    /// running on this address space.  [`ProcessTable::reap`] does the teardown
    /// once the scheduler has switched CR3 away.
    pub fn set_status(&mut self, status: ExitStatus) {
        if self.status == ExitStatus::Running {
            self.status = status;
        }
    }

    fn teardown(&mut self) {
        for vma in self.vmas.iter() {
            let (start, end) = (vma.start, vma.end);
            let mut va = start;
            while va < end {
                if let Ok(pa) = X86_64Paging::unmap(self.root, va as usize) {
                    page_alloc().free_page(phys_to_virt(pa) as *mut u8, 1);
                    self.freed_pages += 1;
                }
                va += PAGE_SIZE as u64;
            }
        }
        destroy_address_space(self.root);
        self.handles.clear();
        self.root = 0;
    }
}

// -------------------------------------------------------------- process table

/// The table is `const`-constructed straight into `.bss`: a `Default` impl
/// would build a ~37 KiB temporary on the kernel stack first, which overflows a
/// 16 KiB thread stack.
#[repr(transparent)]
struct TableCell(UnsafeCell<ProcessTable>);

unsafe impl Sync for TableCell {}

static PROCS: TableCell = TableCell(UnsafeCell::new(ProcessTable::new()));

pub fn table() -> &'static mut ProcessTable {
    unsafe { &mut *PROCS.0.get() }
}

/// Where a reaped process's outcome goes.  P0 has no zombie state yet: the
/// reaper frees the resources immediately but keeps the last `MAX_PROCS`
/// outcomes so a parent (the smoke test today, `sys_wait` in P1) can still ask.
#[derive(Clone, Copy, Debug)]
pub struct ExitRecord {
    pub pid: u32,
    pub status: ExitStatus,
    pub freed_pages: u64,
}

impl ExitRecord {
    const fn empty() -> Self {
        Self {
            pid: NO_PID,
            status: ExitStatus::Running,
            freed_pages: 0,
        }
    }
}

pub struct ProcessTable {
    procs: [Process; MAX_PROCS],
    exits: [ExitRecord; MAX_PROCS],
    exits_len: usize,
    live: u32,
    next_pid: u32,
}

impl ProcessTable {
    pub const fn new() -> Self {
        Self {
            procs: [const { Process::new() }; MAX_PROCS],
            exits: [ExitRecord::empty(); MAX_PROCS],
            exits_len: 0,
            live: 0,
            next_pid: 1,
        }
    }

    fn record_exit(&mut self, pid: u32, status: ExitStatus, freed_pages: u64) {
        if self.exits_len < MAX_PROCS {
            self.exits[self.exits_len] = ExitRecord {
                pid,
                status,
                freed_pages,
            };
            self.exits_len += 1;
            return;
        }
        // Ring: drop the oldest outcome.
        for i in 1..MAX_PROCS {
            self.exits[i - 1] = self.exits[i];
        }
        self.exits[MAX_PROCS - 1] = ExitRecord {
            pid,
            status,
            freed_pages,
        };
    }

    pub fn exit_record(&self, pid: u32) -> Option<ExitRecord> {
        self.exits[..self.exits_len]
            .iter()
            .rev()
            .find(|r| r.pid == pid)
            .copied()
    }

    /// Live status, or the recorded outcome if the process is already reaped.
    pub fn status_of(&self, pid: u32) -> Option<ExitStatus> {
        match self.get_ref(pid) {
            Some(p) => Some(p.status()),
            None => self.exit_record(pid).map(|r| r.status),
        }
    }

    /// Create a process with a fresh address space.  Returns its pid.
    pub fn create(&mut self, parent: u32) -> Option<u32> {
        let slot = self.procs.iter().position(|p| !p.used)?;
        let root = crate::arch::x86_64::paging::create_kernel_address_space()?;
        let pid = self.next_pid;
        self.next_pid += 1;
        let p = &mut self.procs[slot];
        *p = Process::new();
        p.used = true;
        p.pid = pid;
        p.parent = parent;
        p.root = root;
        self.live += 1;
        Some(pid)
    }

    pub fn get(&mut self, pid: u32) -> Option<&mut Process> {
        self.procs.iter_mut().find(|p| p.used && p.pid == pid)
    }

    pub fn get_ref(&self, pid: u32) -> Option<&Process> {
        self.procs.iter().find(|p| p.used && p.pid == pid)
    }

    pub fn live(&self) -> u32 {
        self.live
    }

    pub fn iter(&self) -> impl Iterator<Item = &Process> {
        self.procs.iter().filter(|p| p.used)
    }

    /// Attach the (single, in P0) user thread to a process.
    pub fn attach_thread(&mut self, pid: u32, tid: u32) -> Result<(), Status> {
        let p = self.get(pid).ok_or(Status::NotFound)?;
        if p.thread != NO_PID {
            return Err(Status::NotReady);
        }
        p.thread = tid;
        Ok(())
    }

    /// Free a dead process: user frames, page tables, handles, slot.
    ///
    /// **Must not run while `root` is the active CR3** — the caller (the
    /// scheduler's reaper) has already switched to another thread.
    pub fn reap(&mut self, pid: u32) -> bool {
        let Some(slot) = self.procs.iter().position(|p| p.used && p.pid == pid) else {
            return false;
        };
        let (status, freed) = {
            let p = &mut self.procs[slot];
            let status = p.status();
            p.teardown();
            let freed = p.freed_pages();
            *p = Process::new();
            (status, freed)
        };
        self.record_exit(pid, status, freed);
        self.live -= 1;
        true
    }
}

// ------------------------------------------------------------------ helpers

/// The process of the currently running thread, if it is a user thread.
pub fn current() -> Option<&'static mut Process> {
    let pid = crate::thread::current_pid()?;
    table().get(pid)
}

pub fn current_ref() -> Option<&'static Process> {
    let pid = crate::thread::current_pid()?;
    table().get_ref(pid)
}

/// Terminate the calling user thread's process.  The thread is marked dying and
/// a reschedule is requested; the scheduler reaps both one tick later.
pub fn exit_current(status: ExitStatus) {
    if let Some(pid) = crate::thread::current_pid() {
        if let Some(p) = table().get(pid) {
            p.set_status(status);
        }
    }
    crate::thread::kill_current();
}

/// `sys_exit` / a user fault both land here.
pub fn copy_from_user(dst: &mut [u8], src_va: u64) -> Result<(), Status> {
    current_ref()
        .ok_or(Status::BadAddress)?
        .copy_from_user(dst, src_va)
}

pub fn copy_to_user(dst_va: u64, src: &[u8]) -> Result<(), Status> {
    current_ref()
        .ok_or(Status::BadAddress)?
        .copy_to_user(dst_va, src)
}

// --------------------------------------------------------------------- tests

/// Self-test for the pieces that do not need a running user thread.
pub fn selftest_handles() -> bool {
    let mut t = HandleTable::new();
    let mut ok = true;

    let a = t.insert(ObjKind::Chan, 7, rondos_abi::rights::READ | rondos_abi::rights::WRITE);
    let a = match a {
        Some(a) => a,
        None => return false,
    };
    ok &= a.index() == 0 && a.generation() == 0;
    ok &= t.get(a).is_ok();
    ok &= t.resolve(a, rondos_abi::rights::READ).is_ok();
    ok &= t.resolve(a, rondos_abi::rights::KILL).err() == Some(Status::Permission);
    ok &= t.close(a).is_ok();
    // Same index, bumped generation: the old handle must be dead.
    ok &= t.get(a).err() == Some(Status::BadHandle);
    let b = t.insert(ObjKind::File, 9, rondos_abi::rights::READ).unwrap();
    ok &= b.index() == 0 && b.generation() == 1;
    ok &= t.get(b).is_ok() && t.get(a).is_err();
    ok &= t.resolve(Handle::new(0, 7), rondos_abi::rights::READ).err() == Some(Status::BadHandle);
    ok &= t.resolve(Handle::new(999, 0), rondos_abi::rights::READ).err() == Some(Status::BadHandle);

    // VMA list: overlap rejected, single-VMA range check enforced.
    let mut v = VmaList::new();
    ok &= v.insert(Vma::new(0x1000, 0x3000, PAGE_USER_RW)).is_ok();
    ok &= v.insert(Vma::new(0x2000, 0x4000, PAGE_USER_RW)).err() == Some(Status::InvalidArgument);
    ok &= v.insert(Vma::new(0x4000, 0x5000, PageFlags { writable: false, ..PAGE_USER_RW })).is_ok();
    ok &= v.check(0x1000, 0x2000, true).is_ok();
    ok &= v.check(0x2ff0, 0x20, true).err() == Some(Status::BadAddress); // crosses VMA end
    ok &= v.check(0x4000, 0x1000, true).err() == Some(Status::Permission); // read-only
    ok &= v.check(0x9000, 0x10, false).err() == Some(Status::BadAddress);
    ok &= v.check(0x1000, 0, false).err() == Some(Status::InvalidArgument);
    ok &= v.remove(0x1000).is_some();
    ok &= v.find(0x1000).is_none();

    ok
}
