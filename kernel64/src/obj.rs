//! Kernel objects that outlive a process and can be shared between them — P2.
//!
//! Two kinds so far:
//!
//! * [`MemObj`] — a page-backed memory object.  `sys_mem_map` creates one and
//!   maps it into the caller; `sys_mem_share` (or handing the handle to a child
//!   via `sys_spawn`/`sys_chan_send`) gives another process a handle, and the
//!   kernel maps the same frames there.  Reference-counted: the frames are
//!   returned to the allocator when the last handle goes away.
//! * [`ChanObj`] — a bounded message queue with two ends.  Messages are byte
//!   buffers (handle passing is the next step); send/recv are non-blocking at
//!   this layer and the syscall layer sleeps and retries.
//!
//! Both tables are fixed-size and `const`-constructed into `.bss`, like the
//! process and thread tables.

#![allow(dead_code)]

use core::cell::UnsafeCell;

use crate::arch::x86_64::paging::{phys_to_virt, virt_to_phys};
use crate::mm::page_alloc;
use crate::mm::PAGE_SIZE;
use rondos_abi::Status;

pub const MAX_MEM_OBJS: usize = 16;
pub const MAX_MEM_PAGES: usize = 16;

pub const MAX_CHANS: usize = 8;
pub const CHAN_SLOTS: usize = 8;
pub const CHAN_MSG_BYTES: usize = 128;
/// Handles that can travel with one message.
pub const CHAN_MSG_HANDLES: usize = 2;

/// A portable object reference: everything needed to rebuild the handle in
/// another process's table (no process-local state such as a mapping VA).
#[derive(Clone, Copy, Debug, Default)]
pub struct ObjDesc {
    pub kind: u32,
    /// Object id (memory/chan/tmpfs slot) or tar offset (file).
    pub id: u32,
    /// Tar length for files, 0 otherwise.
    pub aux: u32,
    pub _pad0: u32,
    pub flags: u64,
    pub rights: u64,
}

// ------------------------------------------------------------ memory objects

#[derive(Clone, Copy)]
pub struct MemObj {
    pub used: bool,
    pub refs: u32,
    pub len: u64,
    pub flags: u64,
    /// Device mapping: the frames belong to hardware, never freed here.
    pub phys: bool,
    pub pages: [usize; MAX_MEM_PAGES],
}

impl MemObj {
    const fn new() -> Self {
        Self {
            used: false,
            refs: 0,
            len: 0,
            flags: 0,
            phys: false,
            pages: [0; MAX_MEM_PAGES],
        }
    }

    pub fn page_count(&self) -> usize {
        ((self.len as usize) + PAGE_SIZE - 1) / PAGE_SIZE
    }
}

pub struct MemTable {
    objs: [MemObj; MAX_MEM_OBJS],
}

impl MemTable {
    pub const fn new() -> Self {
        Self {
            objs: [const { MemObj::new() }; MAX_MEM_OBJS],
        }
    }

    /// Allocate a zeroed anonymous memory object of `len` bytes.
    pub fn create(&mut self, len: u64, flags: u64) -> Option<u32> {
        if len == 0 || len as usize > MAX_MEM_PAGES * PAGE_SIZE {
            return None;
        }
        let slot = self.objs.iter().position(|o| !o.used)?;
        let pages = ((len as usize) + PAGE_SIZE - 1) / PAGE_SIZE;
        let obj = &mut self.objs[slot];
        *obj = MemObj::new();
        for i in 0..pages {
            let frame = match page_alloc().get_page(1) {
                Some(f) => f,
                None => {
                    // Roll back what we already took.
                    for p in obj.pages[..i].iter() {
                        page_alloc().free_page(phys_to_virt(*p) as *mut u8, 1);
                    }
                    return None;
                }
            };
            unsafe { core::ptr::write_bytes(frame, 0, PAGE_SIZE) };
            // Store *physical* addresses: the allocator hands out physmap VAs.
            obj.pages[i] = virt_to_phys(frame as usize);
        }
        obj.used = true;
        obj.refs = 1;
        obj.len = len;
        obj.flags = flags;
        Some(slot as u32)
    }

    /// Wrap existing physical frames (device memory, e.g. the framebuffer).
    pub fn create_phys(&mut self, pa: u64, len: u64, flags: u64) -> Option<u32> {
        let pages = ((len as usize) + PAGE_SIZE - 1) / PAGE_SIZE;
        if len == 0 || pages > MAX_MEM_PAGES || pa % PAGE_SIZE as u64 != 0 {
            return None;
        }
        let slot = self.objs.iter().position(|o| !o.used)?;
        let obj = &mut self.objs[slot];
        *obj = MemObj::new();
        for i in 0..pages {
            obj.pages[i] = pa as usize + i * PAGE_SIZE;
        }
        obj.used = true;
        obj.refs = 1;
        obj.len = len;
        obj.flags = flags;
        obj.phys = true;
        Some(slot as u32)
    }

    pub fn get(&self, id: u32) -> Option<&MemObj> {
        self.objs.get(id as usize).filter(|o| o.used)
    }

    pub fn retain(&mut self, id: u32) -> bool {
        match self.objs.get_mut(id as usize) {
            Some(o) if o.used => {
                o.refs += 1;
                true
            }
            _ => false,
        }
    }

    /// Drop a reference; frees the frames when the last one goes away.
    pub fn release(&mut self, id: u32) {
        let Some(o) = self.objs.get_mut(id as usize) else {
            return;
        };
        if !o.used {
            return;
        }
        o.refs -= 1;
        if o.refs > 0 {
            return;
        }
        if !o.phys {
            for p in o.pages[..o.page_count()].iter() {
                page_alloc().free_page(phys_to_virt(*p) as *mut u8, 1);
            }
        }
        *o = MemObj::new();
    }
}

// ----------------------------------------------------------------- channels

#[derive(Clone, Copy)]
pub struct ChanMsg {
    pub len: u16,
    pub n_handles: u8,
    pub _pad0: u8,
    pub bytes: [u8; CHAN_MSG_BYTES],
    pub handles: [ObjDesc; CHAN_MSG_HANDLES],
}

impl ChanMsg {
    const fn empty() -> Self {
        Self {
            len: 0,
            n_handles: 0,
            _pad0: 0,
            bytes: [0; CHAN_MSG_BYTES],
            handles: [ObjDesc {
                kind: 0,
                id: 0,
                aux: 0,
                _pad0: 0,
                flags: 0,
                rights: 0,
            }; CHAN_MSG_HANDLES],
        }
    }
}

/// Drop the object references a queued message still holds.
fn release_msg(msg: &ChanMsg) {
    for d in msg.handles[..msg.n_handles as usize].iter() {
        match d.kind {
            3 => mem().release(d.id),      // ObjKind::Memory
            4 => chans().release(d.id),    // ObjKind::Chan
            _ => {}
        }
    }
}

/// A bounded FIFO of byte messages shared by every handle to it.
#[derive(Clone, Copy)]
pub struct ChanObj {
    pub used: bool,
    pub refs: u32,
    head: u8,
    tail: u8,
    count: u8,
    msgs: [ChanMsg; CHAN_SLOTS],
}

impl ChanObj {
    const fn new() -> Self {
        Self {
            used: false,
            refs: 0,
            head: 0,
            tail: 0,
            count: 0,
            msgs: [ChanMsg::empty(); CHAN_SLOTS],
        }
    }

    pub fn is_full(&self) -> bool {
        self.count as usize == CHAN_SLOTS
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

pub struct ChanTable {
    chans: [ChanObj; MAX_CHANS],
}

impl ChanTable {
    pub const fn new() -> Self {
        Self {
            chans: [const { ChanObj::new() }; MAX_CHANS],
        }
    }

    pub fn create(&mut self) -> Option<u32> {
        let slot = self.chans.iter().position(|c| !c.used)?;
        let c = &mut self.chans[slot];
        *c = ChanObj::new();
        c.used = true;
        c.refs = 1;
        Some(slot as u32)
    }

    pub fn retain(&mut self, id: u32) -> bool {
        match self.chans.get_mut(id as usize) {
            Some(c) if c.used => {
                c.refs += 1;
                true
            }
            _ => false,
        }
    }

    pub fn release(&mut self, id: u32) {
        let Some(c) = self.chans.get_mut(id as usize) else {
            return;
        };
        if !c.used {
            return;
        }
        c.refs -= 1;
        if c.refs == 0 {
            // Undelivered messages still hold object references.
            for i in 0..CHAN_SLOTS {
                release_msg(&c.msgs[i]);
            }
            *c = ChanObj::new();
        }
    }

    /// Push one message (plus up to [`CHAN_MSG_HANDLES`] object references).
    /// `NotReady` when the queue is full (the caller sleeps and retries).
    ///
    /// The caller must already hold one reference per `handles` entry; the
    /// message takes it over.
    pub fn send(
        &mut self,
        id: u32,
        bytes: &[u8],
        handles: &[ObjDesc],
    ) -> Result<usize, Status> {
        if bytes.len() > CHAN_MSG_BYTES || handles.len() > CHAN_MSG_HANDLES {
            return Err(Status::InvalidArgument);
        }
        let c = match self.chans.get_mut(id as usize) {
            Some(c) if c.used => c,
            _ => return Err(Status::BadHandle),
        };
        if c.is_full() {
            return Err(Status::NotReady);
        }
        let idx = c.tail as usize;
        c.msgs[idx].len = bytes.len() as u16;
        c.msgs[idx].bytes[..bytes.len()].copy_from_slice(bytes);
        c.msgs[idx].n_handles = handles.len() as u8;
        c.msgs[idx].handles[..handles.len()].copy_from_slice(handles);
        c.tail = ((idx + 1) % CHAN_SLOTS) as u8;
        c.count += 1;
        Ok(bytes.len())
    }

    /// Pop one message: `(bytes copied, object references)`.  The references
    /// are handed to the caller, which installs them in the receiver's table.
    pub fn recv(
        &mut self,
        id: u32,
        dst: &mut [u8],
        out: &mut [ObjDesc; CHAN_MSG_HANDLES],
    ) -> Result<(usize, usize), Status> {
        let c = match self.chans.get_mut(id as usize) {
            Some(c) if c.used => c,
            _ => return Err(Status::BadHandle),
        };
        if c.is_empty() {
            return Err(Status::NotReady);
        }
        let idx = c.head as usize;
        let n = c.msgs[idx].len as usize;
        if n > dst.len() {
            return Err(Status::InvalidArgument); // leave the message in place
        }
        dst[..n].copy_from_slice(&c.msgs[idx].bytes[..n]);
        let nh = c.msgs[idx].n_handles as usize;
        out[..nh].copy_from_slice(&c.msgs[idx].handles[..nh]);
        c.msgs[idx].len = 0;
        c.msgs[idx].n_handles = 0;
        c.head = ((idx + 1) % CHAN_SLOTS) as u8;
        c.count -= 1;
        Ok((n, nh))
    }
}

// ------------------------------------------------------------------- tables

#[repr(transparent)]
struct MemCell(UnsafeCell<MemTable>);
unsafe impl Sync for MemCell {}
static MEM_OBJS: MemCell = MemCell(UnsafeCell::new(MemTable::new()));

#[repr(transparent)]
struct ChanCell(UnsafeCell<ChanTable>);
unsafe impl Sync for ChanCell {}
static CHANS: ChanCell = ChanCell(UnsafeCell::new(ChanTable::new()));

pub fn mem() -> &'static mut MemTable {
    unsafe { &mut *MEM_OBJS.0.get() }
}

pub fn chans() -> &'static mut ChanTable {
    unsafe { &mut *CHANS.0.get() }
}
