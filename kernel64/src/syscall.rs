//! `int 0x80` dispatch — ABI v1, the P0 subset.
//!
//! Entry contract (design §6.2): `rax` = [`SyscallId`], `rdi/rsi/rdx/r10/r8/r9`
//! = arg0..arg5, return `rax` = [`Status`] and `rdx` = value.  The stub saves
//! and restores every other register, so only `rax`/`rdx` are observable.
//!
//! Every handler follows two rules:
//!
//! * **never trust a user pointer** — go through [`proc::copy_from_user`] /
//!   [`proc::copy_to_user`], which check the VMA table;
//! * **never resume a frame you killed** — `sys_exit` marks the thread dying
//!   and calls `intr::request_resched()`; `isr_dispatch` then hands the frame
//!   to the scheduler instead of returning it.

#![allow(dead_code)]

use core::mem::size_of;

use crate::arch::x86_64::intr::{self, TrapFrame};
use crate::exec::{self, PendingCap};
use crate::fs;
use crate::obj;
use crate::proc::{self, ExitStatus, ObjRef};
use crate::thread;
use rondos_abi::{
    feature, mem_flags, open_flags, rights, wait_reason, CapDesc, Clock, Handle, Info, LogLevel,
    DirEntry, ObjKind, Slice, Stat, Status, StructHeader, SyscallId, WaitResult, ABI_VERSION,
};

/// Largest `sys_log` payload accepted in one call (keeps the copy on-stack).
const MAX_LOG_LEN: usize = 512;

/// `sys_wait` accepts at most this many handles in one call.
const MAX_WAIT_HANDLES: usize = 16;

/// `sys_spawn` accepts at most this many delegated capabilities.
const MAX_SPAWN_CAPS: usize = 4;

fn as_bytes_mut<T>(v: &mut T) -> &mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(v as *mut T as *mut u8, size_of::<T>()) }
}

static LOGGED_BYTES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Bytes accepted by `sys_log` so far (smoke-test observable).
pub fn logged_bytes() -> usize {
    LOGGED_BYTES.load(core::sync::atomic::Ordering::Relaxed)
}

/// Most recent `sys_log` payload, so the smoke test can assert on user output
/// without parsing the serial stream.
struct LastLogCell(core::cell::UnsafeCell<([u8; 64], usize)>);

unsafe impl Sync for LastLogCell {}

static LAST_LOG: LastLogCell = LastLogCell(core::cell::UnsafeCell::new(([0; 64], 0)));

pub fn last_log() -> ([u8; 64], usize) {
    unsafe { *LAST_LOG.0.get() }
}

fn remember_log(bytes: &[u8]) {
    let cell = unsafe { &mut *LAST_LOG.0.get() };
    let n = bytes.len().min(cell.0.len());
    cell.0[..n].copy_from_slice(&bytes[..n]);
    cell.1 = n;
}

pub fn dispatch(f: &mut TrapFrame) {
    let Some(id) = SyscallId::from_raw(f.rax) else {
        f.set_result(Status::Unsupported as u64, 0);
        return;
    };

    match id {
        SyscallId::Info => sys_info(f),
        SyscallId::Log => sys_log(f),
        SyscallId::Exit => sys_exit(f),
        SyscallId::ThreadExit => sys_thread_exit(f),
        SyscallId::Yield => sys_yield(f),
        SyscallId::SleepNs => sys_sleep_ns(f),
        SyscallId::ClockGettime => sys_clock_gettime(f),
        SyscallId::Open => sys_open(f),
        SyscallId::Read => sys_read(f),
        SyscallId::Write => sys_write(f),
        SyscallId::Close => sys_close(f),
        SyscallId::Spawn => sys_spawn(f),
        SyscallId::Wait => sys_wait(f),
        SyscallId::ProcStatus => sys_proc_status(f),
        SyscallId::Kill => sys_kill(f),
        SyscallId::MemMap => sys_mem_map(f),
        SyscallId::MemUnmap => sys_mem_unmap(f),
        SyscallId::MemShare => sys_mem_share(f),
        SyscallId::MemMapPhys => sys_mem_map_phys(f),
        SyscallId::ChanCreate => sys_chan_create(f),
        SyscallId::ChanSend => sys_chan_send(f),
        SyscallId::ChanRecv => sys_chan_recv(f),
        SyscallId::Stat => sys_stat(f),
        SyscallId::Readdir => sys_readdir(f),
        SyscallId::Seek => sys_seek(f),
        SyscallId::Unlink => sys_unlink(f),
        // Everything else is declared in the frozen v1 table but lands in P1+.
        _ => f.set_result(Status::Unsupported as u64, 0),
    }
}

fn ok(f: &mut TrapFrame, value: u64) {
    f.set_result(Status::Ok as u64, value);
}

fn err(f: &mut TrapFrame, status: Status) {
    f.set_result(status as u64, 0);
}

/// `0x00 sys_info(&mut Info)`: ABI version, feature bits, timer and framebuffer.
fn sys_info(f: &mut TrapFrame) {
    let dst = f.rdi;
    let mut hdr_bytes = [0u8; size_of::<StructHeader>()];
    if let Err(s) = proc::copy_from_user(&mut hdr_bytes, dst) {
        return err(f, s);
    }
    let user_hdr = StructHeader {
        size: u32::from_ne_bytes(hdr_bytes[0..4].try_into().unwrap()),
        version: u32::from_ne_bytes(hdr_bytes[4..8].try_into().unwrap()),
    };
    if (user_hdr.size as usize) < size_of::<StructHeader>() {
        return err(f, Status::InvalidArgument);
    }

    let boot = crate::bootinfo::get();
    let mut info = Info::default();
    let full = size_of::<Info>();
    info.hdr = StructHeader {
        size: (user_hdr.size as usize).min(full) as u32,
        version: user_hdr.version,
    };
    info.abi_version = ABI_VERSION;
    info.fb_present = boot.fb_present;
    info.features = feature::SSE | feature::DEVICE_MAP;
    info.page_size = crate::mm::PAGE_SIZE as u32;
    info.tick_ms = thread::TICK_MS as u32;
    info.ticks = thread::ticks();
    info.ns_per_tick = thread::TICK_MS * 1_000_000;
    info.fb = rondos_abi::FramebufferInfo {
        phys: boot.fb.phys,
        width: boot.fb.width,
        height: boot.fb.height,
        pitch: boot.fb.pitch,
        bpp: boot.fb.bpp,
        format: boot.fb.format,
        _pad0: [0; 2],
        _reserved: [0; 2],
    };

    let want = info.hdr.size as usize;
    let bytes = unsafe { core::slice::from_raw_parts(&info as *const Info as *const u8, want) };
    match proc::copy_to_user(dst, bytes) {
        Ok(()) => ok(f, want as u64),
        Err(s) => err(f, s),
    }
}

/// `0x16 sys_log(level, buf, len) -> n`
fn sys_log(f: &mut TrapFrame) {
    let _level = match f.rdi {
        0 => LogLevel::Error,
        1 => LogLevel::Warn,
        2 => LogLevel::Info,
        _ => LogLevel::Debug,
    };
    let len = f.rdx as usize;
    if len == 0 || len > MAX_LOG_LEN {
        return err(f, Status::InvalidArgument);
    }
    let mut buf = [0u8; MAX_LOG_LEN];
    if let Err(s) = proc::copy_from_user(&mut buf[..len], f.rsi) {
        return err(f, s);
    }
    log_bytes(&buf[..len]);
    ok(f, len as u64)
}

/// The single sink behind `sys_log` and `sys_write` on the console handle.
fn log_bytes(bytes: &[u8]) {
    let text = core::str::from_utf8(bytes).unwrap_or("<non-utf8>");
    crate::serial_println!("user: {}", text.trim_end_matches('\n'));
    remember_log(bytes);
    LOGGED_BYTES.fetch_add(bytes.len(), core::sync::atomic::Ordering::Relaxed);
}

/// `0x10 sys_exit(status) -> !`
fn sys_exit(f: &mut TrapFrame) {
    let status = f.rdi as u32;
    proc::exit_current(ExitStatus::Exited(status));
    // The frame is dead; the scheduler is about to replace it.
}

/// `0x11 sys_thread_exit(status) -> !` — P0: one thread per process.
fn sys_thread_exit(f: &mut TrapFrame) {
    let status = f.rdi as u32;
    proc::exit_current(ExitStatus::Exited(status));
}

/// `0x13 sys_yield()`: give up the CPU, then return to the caller.
fn sys_yield(f: &mut TrapFrame) {
    intr::request_resched();
    ok(f, 0);
}

// ------------------------------------------------------- P1b: files & procs

/// `0x50 sys_open(dir, path, flags) -> Handle<File>`
///
/// v1 has no path walk beyond the flat boot tar: the path is looked up in the
/// image, and the directory handle must be the root capability handed over in
/// the `StartupBlock`.
fn sys_open(f: &mut TrapFrame) {
    let dir = Handle(f.rdi);
    let path_ptr = f.rsi;
    let path_len = f.rdx as usize;
    let flags = f.r10;

    if path_len == 0 || path_len > 128 {
        return err(f, Status::InvalidArgument);
    }
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    match p.handles().resolve(dir, rights::READ) {
        Ok(slot) if matches!(slot.obj, ObjRef::Dir { .. }) => {}
        Ok(_) => return err(f, Status::InvalidArgument),
        Err(s) => return err(f, s),
    }
    let mut path = [0u8; 128];
    if let Err(s) = p.copy_from_user(&mut path[..path_len], path_ptr) {
        return err(f, s);
    }
    let want_write = flags & open_flags::WRITE != 0;
    let create = flags & open_flags::CREATE != 0;

    // tmpfs shadows the tar for names it holds; otherwise the boot tar is the
    // read-only fallback.
    if let Some(id) = fs::tmp().find(&path[..path_len]) {
        let mut r = rights::READ;
        if want_write {
            r |= rights::WRITE;
        }
        return match p.handles_mut().insert(ObjRef::TmpFile { id, pos: 0 }, r) {
            Some(h) => ok(f, h.0),
            None => err(f, Status::OutOfMemory),
        };
    }
    if create {
        let id = match fs::tmp().create(&path[..path_len]) {
            Ok(id) => id,
            Err(s) => return err(f, s),
        };
        let mut r = rights::READ;
        if want_write {
            r |= rights::WRITE;
        }
        return match p.handles_mut().insert(ObjRef::TmpFile { id, pos: 0 }, r) {
            Some(h) => ok(f, h.0),
            None => err(f, Status::OutOfMemory),
        };
    }
    if want_write {
        return err(f, Status::Permission); // the boot tar is read-only
    }
    let Some(entry) = fs::TarFs::root().and_then(|t| t.find(&path[..path_len])) else {
        return err(f, Status::NotFound);
    };
    let obj = ObjRef::File {
        off: entry.off,
        len: entry.len,
        pos: 0,
    };
    match p.handles_mut().insert(obj, rights::READ) {
        Some(h) => ok(f, h.0),
        None => err(f, Status::OutOfMemory),
    }
}

/// `0x51 sys_read(handle, buf, len) -> n`
fn sys_read(f: &mut TrapFrame) {
    let h = Handle(f.rdi);
    let dst = f.rsi;
    let len = f.rdx as usize;
    if len == 0 {
        return ok(f, 0);
    }
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    // tmpfs first: a different object, but the same read contract.
    let tmp_src = match p.handles().resolve(h, rights::READ) {
        Ok(slot) => match slot.obj {
            ObjRef::TmpFile { id, pos } => Some((id, pos)),
            _ => None,
        },
        Err(s) => return err(f, s),
    };
    if let Some((id, pos)) = tmp_src {
        let mut chunk = [0u8; 256];
        let mut total = 0usize;
        while total < len {
            let want = (len - total).min(chunk.len());
            let n = fs::tmp().read(id, pos + total as u64, &mut chunk[..want]);
            if n == 0 {
                break;
            }
            if let Err(s) = p.copy_to_user(dst + total as u64, &chunk[..n]) {
                return err(f, s);
            }
            total += n;
        }
        let _ = p.handles_mut().with_mut(h, |slot| {
            if let ObjRef::TmpFile { pos, .. } = &mut slot.obj {
                *pos += total as u64;
            }
        });
        return ok(f, total as u64);
    }

    let (off, file_len, pos) = match p.handles().resolve(h, rights::READ) {
        Ok(slot) => match slot.obj {
            ObjRef::File { off, len, pos } => (off, len, pos),
            _ => return err(f, Status::InvalidArgument),
        },
        Err(s) => return err(f, s),
    };
    let Some(tar) = fs::TarFs::root() else {
        return err(f, Status::NotFound);
    };
    let entry = fs::Entry {
        off,
        len: file_len,
        name: b"",
    };

    let mut total = 0usize;
    let mut chunk = [0u8; 256];
    while total < len {
        let want = (len - total).min(chunk.len());
        let n = tar.read(&entry, pos + total as u64, &mut chunk[..want]);
        if n == 0 {
            break;
        }
        if let Err(s) = p.copy_to_user(dst + total as u64, &chunk[..n]) {
            return err(f, s);
        }
        total += n;
    }
    let _ = p.handles_mut().with_mut(h, |slot| {
        if let ObjRef::File { pos, .. } = &mut slot.obj {
            *pos += total as u64;
        }
    });
    ok(f, total as u64)
}

/// `0x52 sys_write(handle, buf, len) -> n`
///
/// The only writable object in v1 is the console device (`Device { node: 0 }`),
/// which is `sys_log` under a handle; the tar file system is immutable.
fn sys_write(f: &mut TrapFrame) {
    let h = Handle(f.rdi);
    let len = f.rdx as usize;
    if len == 0 || len > MAX_LOG_LEN {
        return err(f, Status::InvalidArgument);
    }
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    let tmp_dst = match p.handles().resolve(h, rights::WRITE) {
        Ok(slot) => match slot.obj {
            ObjRef::TmpFile { id, pos } => Some((id, pos)),
            ObjRef::Device { node: 0 } => None,
            ObjRef::Device { .. } => return err(f, Status::NotFound),
            _ => return err(f, Status::Permission),
        },
        Err(s) => return err(f, s),
    };

    let mut buf = [0u8; MAX_LOG_LEN];
    if let Err(s) = p.copy_from_user(&mut buf[..len], f.rsi) {
        return err(f, s);
    }
    if let Some((id, pos)) = tmp_dst {
        return match fs::tmp().write(id, pos, &buf[..len]) {
            Ok(n) => {
                let _ = p.handles_mut().with_mut(h, |slot| {
                    if let ObjRef::TmpFile { pos, .. } = &mut slot.obj {
                        *pos += n as u64;
                    }
                });
                ok(f, n as u64)
            }
            Err(s) => err(f, s),
        };
    }
    log_bytes(&buf[..len]);
    ok(f, len as u64)
}

/// `0x56 sys_close(handle)`
///
/// Closing a memory handle also drops its mapping: the handle *is* the
/// capability, so revoking it must revoke access.  (Frames survive as long as
/// the object has another reference.)
fn sys_close(f: &mut TrapFrame) {
    let h = Handle(f.rdi);
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    if let Ok(slot) = p.handles().resolve(h, rights::NONE) {
        if let ObjRef::Memory { va, .. } = slot.obj {
            let _ = p.unmap_memobj(va);
        }
    }
    match p.handles_mut().close(h) {
        Ok(()) => ok(f, 0),
        Err(s) => err(f, s),
    }
}

/// `0x20 sys_spawn(image) -> Handle<Process>`
///
/// argv/envp/caps are accepted by the ABI but not yet implemented (P2): a v1
/// program gets the root directory capability automatically.
fn sys_spawn(f: &mut TrapFrame) {
    let h = Handle(f.rdi);
    let argv_ptr = f.rsi;
    let envp_ptr = f.rdx;
    let caps_ptr = f.r10;
    let _flags = f.r8;

    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    // argv/envp arrive in P3 with the manifest work.
    if argv_ptr != 0 || envp_ptr != 0 {
        return err(f, Status::Unsupported);
    }

    // Resolve the capabilities to delegate, checking that the caller really
    // holds them with at least the requested rights.
    let mut pending = [PendingCap::EMPTY; MAX_SPAWN_CAPS];
    let mut n_caps = 0usize;
    if caps_ptr != 0 {
        let mut slice = Slice::default();
        if let Err(s) = p.copy_from_user(as_bytes_mut(&mut slice), caps_ptr) {
            return err(f, s);
        }
        if slice.count as usize > MAX_SPAWN_CAPS {
            return err(f, Status::InvalidArgument);
        }
        for i in 0..slice.count as usize {
            let mut desc = CapDesc::default();
            let at = slice.ptr + i as u64 * size_of::<CapDesc>() as u64;
            if let Err(s) = p.copy_from_user(as_bytes_mut(&mut desc), at) {
                return err(f, s);
            }
            // Delegation needs the SHARE right: a capability that cannot be
            // passed on must not be passed on.
            let slot = match p.handles().resolve(Handle(desc.handle), rights::SHARE) {
                Ok(slot) => slot,
                Err(s) => return err(f, s),
            };
            if !slot.allows(desc.rights) {
                return err(f, Status::Permission);
            }
            match slot.obj {
                ObjRef::Memory { id, len, .. } => {
                    let flags = obj::mem().get(id).map(|o| o.flags).unwrap_or(0);
                    pending[n_caps] = PendingCap {
                        kind: ObjKind::Memory,
                        id,
                        len,
                        flags,
                        rights: desc.rights,
                    };
                }
                ObjRef::Chan { id, end } => {
                    pending[n_caps] = PendingCap {
                        kind: ObjKind::Chan,
                        id,
                        len: end as u64,
                        flags: 0,
                        rights: desc.rights,
                    };
                }
                _ => return err(f, Status::Unsupported),
            }
            n_caps += 1;
        }
    }

    let (off, len) = match p.handles().resolve(h, rights::READ) {
        Ok(slot) => match slot.obj {
            ObjRef::File { off, len, .. } => (off, len),
            _ => return err(f, Status::InvalidArgument),
        },
        Err(s) => return err(f, s),
    };
    let entry = fs::Entry {
        off,
        len,
        name: b"child",
    };
    let parent = p.pid();
    let pid = match exec::spawn_entry_with_caps(&entry, &pending[..n_caps], parent) {
        Ok(pid) => pid,
        Err(s) => return err(f, s),
    };
    let obj = ObjRef::Process { pid };
    match p
        .handles_mut()
        .insert(obj, rights::WAIT | rights::KILL | rights::READ)
    {
        Some(handle) => ok(f, handle.0),
        None => {
            proc::kill(pid).ok();
            err(f, Status::OutOfMemory)
        }
    }
}

/// `0x21 sys_wait(handles, n, timeout_ns) -> index | (reason << 32)`
///
/// Level-triggered: returns the first handle whose process is no longer
/// running.  `timeout_ns == 0` polls once; otherwise the calling thread blocks
/// in the kernel (a real block, not a spin) and retries each tick.
fn sys_wait(f: &mut TrapFrame) {
    let ptr = f.rdi;
    let n = f.rsi as usize;
    let timeout_ns = f.rdx;
    if n == 0 || n > MAX_WAIT_HANDLES {
        return err(f, Status::InvalidArgument);
    }
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    let mut handles = [Handle::INVALID; MAX_WAIT_HANDLES];
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(handles.as_mut_ptr() as *mut u8, n * 8)
    };
    if let Err(s) = p.copy_from_user(bytes, ptr) {
        return err(f, s);
    }

    let deadline = if timeout_ns == 0 {
        None
    } else {
        let ticks = (timeout_ns + thread::TICK_MS * 1_000_000 - 1) / (thread::TICK_MS * 1_000_000);
        Some(thread::ticks() + ticks)
    };

    loop {
        for (i, h) in handles[..n].iter().enumerate() {
            let pid = match p.handles().resolve(*h, rights::WAIT) {
                Ok(slot) => match slot.obj {
                    ObjRef::Process { pid } => pid,
                    _ => continue,
                },
                Err(_) => continue,
            };
            if let Some(st) = proc::table().status_of(pid) {
                if st != ExitStatus::Running {
                    let reason = match st {
                        ExitStatus::Exited(_) => wait_reason::EXITED,
                        ExitStatus::Fault { .. } => wait_reason::FAULT,
                        ExitStatus::Killed => wait_reason::KILLED,
                        ExitStatus::Running => unreachable!(),
                    };
                    return ok(f, WaitResult::pack(i as u32, reason));
                }
            }
        }
        match deadline {
            None => return err(f, Status::NotReady),
            Some(d) if thread::ticks() >= d => return err(f, Status::NotReady),
            Some(_) => thread::sleep(thread::TICK_MS),
        }
    }
}

/// `0x22 sys_proc_status(handle, &mut ExitStatus)`
fn sys_proc_status(f: &mut TrapFrame) {
    let h = Handle(f.rdi);
    let out_ptr = f.rsi;
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    let pid = match p.handles().resolve(h, rights::READ) {
        Ok(slot) => match slot.obj {
            ObjRef::Process { pid } => pid,
            _ => return err(f, Status::InvalidArgument),
        },
        Err(s) => return err(f, s),
    };
    let out = exec::exit_status(pid);
    let bytes = unsafe {
        core::slice::from_raw_parts(&out as *const _ as *const u8, size_of_val(&out))
    };
    match p.copy_to_user(out_ptr, bytes) {
        Ok(()) => ok(f, bytes.len() as u64),
        Err(s) => err(f, s),
    }
}

/// `0x23 sys_kill(handle)`
fn sys_kill(f: &mut TrapFrame) {
    let h = Handle(f.rdi);
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    let pid = match p.handles().resolve(h, rights::KILL) {
        Ok(slot) => match slot.obj {
            ObjRef::Process { pid } => pid,
            _ => return err(f, Status::InvalidArgument),
        },
        Err(s) => return err(f, s),
    };
    match proc::kill(pid) {
        Ok(()) => ok(f, 0),
        Err(s) => err(f, s),
    }
}

/// `0x14 sys_sleep_ns(ns)`: block the calling thread (the scheduler runs
/// somebody else) and return once at least `ns` have passed.
fn sys_sleep_ns(f: &mut TrapFrame) {
    let ns = f.rdi;
    if ns > 60_000_000_000 {
        return err(f, Status::InvalidArgument);
    }
    let ms = ns / 1_000_000;
    if ms > 0 {
        thread::sleep(ms);
    } else if ns > 0 {
        thread::yield_now();
    }
    ok(f, 0)
}

// --------------------------------------------------- P2: memory and channels

fn mem_rights(flags: u64) -> u64 {
    let mut r = rights::READ | rights::MAP;
    if flags & mem_flags::WRITE != 0 {
        r |= rights::WRITE;
    }
    if flags & mem_flags::SHARE != 0 {
        r |= rights::SHARE;
    }
    r
}

/// `0x30 sys_mem_map(len, flags) -> Handle<Memory>`
///
/// Creates an anonymous memory object, maps it into the caller and returns the
/// handle; `sys_stat` reports the mapping address.
fn sys_mem_map(f: &mut TrapFrame) {
    let len = f.rdi;
    let flags = f.rsi;
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    let Some(id) = obj::mem().create(len, flags) else {
        return err(f, Status::OutOfMemory);
    };
    let r = mem_rights(flags);
    let va = match p.map_memobj(id, r, 0) {
        Ok(va) => va,
        Err(s) => {
            obj::mem().release(id);
            return err(f, s);
        }
    };
    match p
        .handles_mut()
        .insert(ObjRef::Memory { id, va, len }, r)
    {
        Some(h) => ok(f, h.0),
        None => {
            let _ = p.unmap_memobj(va);
            obj::mem().release(id);
            err(f, Status::OutOfMemory)
        }
    }
}

/// `0x31 sys_mem_unmap(handle)`: unmap and close.
fn sys_mem_unmap(f: &mut TrapFrame) {
    let h = Handle(f.rdi);
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    let va = match p.handles().resolve(h, rights::MAP) {
        Ok(slot) => match slot.obj {
            ObjRef::Memory { va, .. } => va,
            _ => return err(f, Status::InvalidArgument),
        },
        Err(s) => return err(f, s),
    };
    let _ = p.unmap_memobj(va);
    let _ = p.handles_mut().close(h);
    ok(f, 0)
}

/// `0x32 sys_mem_share(handle, rights) -> Handle<Memory>`
fn sys_mem_share(f: &mut TrapFrame) {
    let h = Handle(f.rdi);
    let want = f.rsi;
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    let (id, va, len, have) = match p.handles().resolve(h, rights::SHARE) {
        Ok(slot) => match slot.obj {
            ObjRef::Memory { id, va, len } => (id, va, len, slot.rights),
            _ => return err(f, Status::InvalidArgument),
        },
        Err(s) => return err(f, s),
    };
    if want & !have != 0 {
        return err(f, Status::Permission);
    }
    if !obj::mem().retain(id) {
        return err(f, Status::BadHandle);
    }
    match p
        .handles_mut()
        .insert(ObjRef::Memory { id, va, len }, want)
    {
        Some(nh) => ok(f, nh.0),
        None => {
            obj::mem().release(id);
            err(f, Status::OutOfMemory)
        }
    }
}

/// `0x33 sys_mem_map_phys(pa, len, cache) -> Handle<Memory>`
///
/// Maps device memory (the framebuffer) into the caller.  v1 has no
/// `Cap::DEVICE_MAP` check yet — P3 will add it together with the display
/// server.
fn sys_mem_map_phys(f: &mut TrapFrame) {
    let pa = f.rdi;
    let len = f.rsi;
    let cache = f.rdx;
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    // Device memory is a privileged capability (design §6.5): only a process
    // holding a device handle with MAP may reach physical memory.
    if !p.has_device_map() {
        return err(f, Status::Permission);
    }
    let Some(id) = obj::mem().create_phys(pa, len, cache) else {
        return err(f, Status::InvalidArgument);
    };
    let va = match p.map_memobj(id, rights::READ | rights::MAP, cache as u32) {
        Ok(va) => va,
        Err(s) => {
            obj::mem().release(id);
            return err(f, s);
        }
    };
    match p
        .handles_mut()
        .insert(ObjRef::Memory { id, va, len }, rights::READ | rights::MAP)
    {
        Some(h) => ok(f, h.0),
        None => {
            let _ = p.unmap_memobj(va);
            obj::mem().release(id);
            err(f, Status::OutOfMemory)
        }
    }
}

/// `0x40 sys_chan_create(out: &mut [Handle; 2])`
///
/// The two handles are two ends of the same message queue: either end may send
/// and receive.
fn sys_chan_create(f: &mut TrapFrame) {
    let out = f.rdi;
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    let Some(id) = obj::chans().create() else {
        return err(f, Status::OutOfMemory);
    };
    // SHARE so the ends can be delegated to children/services.
    let rw = rights::READ | rights::WRITE | rights::SHARE;
    let a = p.handles_mut().insert(ObjRef::Chan { id, end: 0 }, rw);
    let b = a.and_then(|_| {
        obj::chans().retain(id);
        p.handles_mut().insert(ObjRef::Chan { id, end: 1 }, rw)
    });
    match (a, b) {
        (Some(a), Some(b)) => {
            let pair = [a.0.to_ne_bytes(), b.0.to_ne_bytes()];
            let bytes = unsafe { core::slice::from_raw_parts(pair.as_ptr() as *const u8, 16) };
            match p.copy_to_user(out, bytes) {
                Ok(()) => ok(f, 2),
                Err(s) => {
                    let _ = p.handles_mut().close(a);
                    let _ = p.handles_mut().close(b);
                    err(f, s)
                }
            }
        }
        _ => {
            obj::chans().release(id);
            err(f, Status::OutOfMemory)
        }
    }
}

/// `0x41 sys_chan_send(handle, buf, len, handles: Slice<Handle>, 0) -> n`
///
/// Up to two handles travel with the message; the kernel takes over one
/// reference each and the receiver gets fresh handles.
fn sys_chan_send(f: &mut TrapFrame) {
    let h = Handle(f.rdi);
    let len = f.rdx as usize;
    let handles_ptr = f.r10;
    if len > obj::CHAN_MSG_BYTES {
        return err(f, Status::InvalidArgument);
    }
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    let (id, end) = match p.handles().resolve(h, rights::WRITE) {
        Ok(slot) => match slot.obj {
            ObjRef::Chan { id, end } => (id, end),
            _ => return err(f, Status::InvalidArgument),
        },
        Err(s) => return err(f, s),
    };

    let mut buf = [0u8; obj::CHAN_MSG_BYTES];
    if len > 0 {
        if let Err(s) = p.copy_from_user(&mut buf[..len], f.rsi) {
            return err(f, s);
        }
    }

    // Collect the object references that travel with the message.
    let mut descs = [obj::ObjDesc::default(); obj::CHAN_MSG_HANDLES];
    let mut n_desc = 0usize;
    if handles_ptr != 0 {
        let mut slice = Slice::default();
        if let Err(s) = p.copy_from_user(as_bytes_mut(&mut slice), handles_ptr) {
            return err(f, s);
        }
        if slice.count as usize > obj::CHAN_MSG_HANDLES {
            return err(f, Status::InvalidArgument);
        }
        for i in 0..slice.count as usize {
            let mut raw = [0u8; 8];
            if let Err(s) = p.copy_from_user(&mut raw, slice.ptr + i as u64 * 8) {
                return err(f, s);
            }
            let uh = Handle(u64::from_ne_bytes(raw));
            // Handles travel only with the SHARE right.
            let (desc, retain) = match p.handles().resolve(uh, rights::SHARE) {
                Ok(slot) => match slot.obj {
                    ObjRef::Memory { id, .. } => {
                        let flags = obj::mem().get(id).map(|o| o.flags).unwrap_or(0);
                        (
                            obj::ObjDesc {
                                kind: ObjKind::Memory as u32,
                                id,
                                aux: 0,
                                _pad0: 0,
                                flags,
                                rights: slot.rights,
                            },
                            Some((ObjKind::Memory, id)),
                        )
                    }
                    ObjRef::Chan { id, end } => (
                        obj::ObjDesc {
                            kind: ObjKind::Chan as u32,
                            id,
                            aux: end as u32,
                            _pad0: 0,
                            flags: 0,
                            rights: slot.rights,
                        },
                        Some((ObjKind::Chan, id)),
                    ),
                    ObjRef::File { off, len, .. } => (
                        obj::ObjDesc {
                            kind: ObjKind::File as u32,
                            id: off,
                            aux: len,
                            _pad0: 0,
                            flags: 0,
                            rights: slot.rights,
                        },
                        None,
                    ),
                    _ => return err(f, Status::Unsupported),
                },
                Err(s) => return err(f, s),
            };
            // The message holds its own reference.
            if let Some((k, id)) = retain {
                let ok = match k {
                    ObjKind::Memory => obj::mem().retain(id),
                    ObjKind::Chan => obj::chans().retain(id),
                    _ => true,
                };
                if !ok {
                    return err(f, Status::BadHandle);
                }
            }
            descs[n_desc] = desc;
            n_desc += 1;
        }
    }

    let deadline = thread::ticks() + 200 / thread::TICK_MS; // ~1 s
    loop {
        match obj::chans().send(id, end, &buf[..len], &descs[..n_desc]) {
            Ok(n) => return ok(f, n as u64),
            Err(Status::NotReady) if thread::ticks() < deadline => {
                thread::sleep(thread::TICK_MS);
            }
            Err(s) => {
                // Give the references back if the message never left.
                for d in descs[..n_desc].iter() {
                    match d.kind {
                        k if k == ObjKind::Memory as u32 => obj::mem().release(d.id),
                        k if k == ObjKind::Chan as u32 => obj::chans().release(d.id),
                        _ => {}
                    }
                }
                return err(f, s);
            }
        }
    }
}

/// `0x42 sys_chan_recv(handle, buf, len, out_handles: Slice<Handle>, 0)
///   -> n | (n_handles << 32)`
fn sys_chan_recv(f: &mut TrapFrame) {
    let h = Handle(f.rdi);
    let len = f.rdx as usize;
    let out_ptr = f.r10;
    if len == 0 {
        return err(f, Status::InvalidArgument);
    }
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    let (id, end) = match p.handles().resolve(h, rights::READ) {
        Ok(slot) => match slot.obj {
            ObjRef::Chan { id, end } => (id, end),
            _ => return err(f, Status::InvalidArgument),
        },
        Err(s) => return err(f, s),
    };
    // The caller passes a Slice<Handle>: `ptr` is where the received handles
    // go, `count` is how many it can take (0 = do not accept handles).
    let mut out_arr = 0u64;
    let mut capacity = 0usize;
    if out_ptr != 0 {
        let mut slice = Slice::default();
        if let Err(s) = p.copy_from_user(as_bytes_mut(&mut slice), out_ptr) {
            return err(f, s);
        }
        out_arr = slice.ptr;
        capacity = (slice.count as usize).min(obj::CHAN_MSG_HANDLES);
    }

    let mut buf = [0u8; obj::CHAN_MSG_BYTES];
    let want = len.min(obj::CHAN_MSG_BYTES);
    let mut descs = [obj::ObjDesc::default(); obj::CHAN_MSG_HANDLES];

    let deadline = thread::ticks() + 200 / thread::TICK_MS; // ~1 s
    loop {
        match obj::chans().recv(id, end, &mut buf[..want], &mut descs) {
            Ok((n, nh)) => {
                if let Err(s) = p.copy_to_user(f.rsi, &buf[..n]) {
                    return err(f, s);
                }
                let mut installed = 0usize;
                for d in descs[..nh].iter() {
                    if installed >= capacity {
                        // No room: drop the reference we just received.
                        match d.kind {
                            k if k == ObjKind::Memory as u32 => obj::mem().release(d.id),
                            k if k == ObjKind::Chan as u32 => obj::chans().release(d.id),
                            _ => {}
                        }
                        continue;
                    }
                    match p.install_obj(d) {
                        Ok(handle) => {
                            if out_arr != 0 {
                                let at = out_arr + installed as u64 * 8;
                                if let Err(s) = p.copy_to_user(at, &handle.0.to_ne_bytes()) {
                                    return err(f, s);
                                }
                            }
                            installed += 1;
                        }
                        Err(_) => {}
                    }
                }
                return ok(f, (n as u64) | ((installed as u64) << 32));
            }
            Err(Status::NotReady) if thread::ticks() < deadline => {
                thread::sleep(thread::TICK_MS);
            }
            Err(s) => return err(f, s),
        }
    }
}

/// `0x53 sys_seek(handle, offset, whence) -> new offset`
fn sys_seek(f: &mut TrapFrame) {
    let h = Handle(f.rdi);
    let offset = f.rsi as i64;
    let whence = f.rdx;
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    let (cur, end) = match p.handles().resolve(h, rights::READ) {
        Ok(slot) => match slot.obj {
            ObjRef::File { pos, len, .. } => (pos, len as u64),
            ObjRef::TmpFile { id, pos } => (pos, fs::tmp().len(id)),
            _ => return err(f, Status::InvalidArgument),
        },
        Err(s) => return err(f, s),
    };
    let base = match whence {
        0 => 0i64,
        1 => cur as i64,
        2 => end as i64,
        _ => return err(f, Status::InvalidArgument),
    };
    let new = match base.checked_add(offset) {
        Some(v) if v >= 0 => v as u64,
        _ => return err(f, Status::InvalidArgument),
    };
    let _ = p.handles_mut().with_mut(h, |slot| match &mut slot.obj {
        ObjRef::File { pos, .. } => *pos = new,
        ObjRef::TmpFile { pos, .. } => *pos = new,
        _ => {}
    });
    ok(f, new)
}

/// `0x57 sys_unlink(dir, path, len)` — tmpfs only (v1 has no directories).
fn sys_unlink(f: &mut TrapFrame) {
    let dir = Handle(f.rdi);
    let path_len = f.rdx as usize;
    if path_len == 0 || path_len > 128 {
        return err(f, Status::InvalidArgument);
    }
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    match p.handles().resolve(dir, rights::READ) {
        Ok(slot) if matches!(slot.obj, ObjRef::Dir { .. }) => {}
        Ok(_) => return err(f, Status::InvalidArgument),
        Err(s) => return err(f, s),
    }
    let mut path = [0u8; 128];
    if let Err(s) = p.copy_from_user(&mut path[..path_len], f.rsi) {
        return err(f, s);
    }
    match fs::tmp().remove(&path[..path_len]) {
        Ok(()) => ok(f, 0),
        Err(s) => err(f, s),
    }
}

/// `0x54 sys_stat(handle, &mut Stat)`
fn sys_stat(f: &mut TrapFrame) {
    let h = Handle(f.rdi);
    let out_ptr = f.rsi;
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    let mut out = Stat {
        hdr: StructHeader::new(size_of::<Stat>() as u32),
        ..Default::default()
    };
    match p.handles().resolve(h, rights::NONE) {
        Ok(slot) => {
            out.kind = slot.kind() as u32;
            out.rights = slot.rights;
            match slot.obj {
                ObjRef::File { len, .. } => out.len_bytes = len as u64,
                ObjRef::TmpFile { id, .. } => out.len_bytes = fs::tmp().len(id),
                ObjRef::Memory { va, len, .. } => {
                    out.len_bytes = len;
                    out.va = va;
                }
                _ => {}
            }
        }
        Err(s) => return err(f, s),
    }
    let bytes = unsafe {
        core::slice::from_raw_parts(&out as *const Stat as *const u8, size_of::<Stat>())
    };
    match p.copy_to_user(out_ptr, bytes) {
        Ok(()) => ok(f, bytes.len() as u64),
        Err(s) => err(f, s),
    }
}

/// `0x55 sys_readdir(dir, index, &mut DirEntry)`
///
/// v1 has one flat namespace: the tar entries first, then tmpfs files.  Index
/// past the end returns `NotFound`, which the caller reads as end-of-directory.
fn sys_readdir(f: &mut TrapFrame) {
    let dir = Handle(f.rdi);
    let index = f.rsi as usize;
    let out_ptr = f.rdx;
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    match p.handles().resolve(dir, rights::READ) {
        Ok(slot) if matches!(slot.obj, ObjRef::Dir { .. }) => {}
        Ok(_) => return err(f, Status::InvalidArgument),
        Err(s) => return err(f, s),
    }

    let mut out = DirEntry {
        hdr: StructHeader::new(size_of::<DirEntry>() as u32),
        ..Default::default()
    };
    let mut found = false;
    if let Some(tar) = fs::TarFs::root() {
        if let Some(e) = tar.entry(index) {
            out.kind = ObjKind::File as u32;
            out.len_bytes = e.len as u64;
            let n = e.name.len().min(out.name.len());
            out.name[..n].copy_from_slice(&e.name[..n]);
            out.name_len = n as u32;
            found = true;
        }
    }
    if !found {
        let tar_count = fs::TarFs::root().map(|t| t.count()).unwrap_or(0);
        if let Some(fe) = fs::tmp().entry(index - tar_count.min(index)) {
            out.kind = ObjKind::File as u32;
            out.len_bytes = fe.len as u64;
            let name = fe.name();
            let n = name.len().min(out.name.len());
            out.name[..n].copy_from_slice(&name[..n]);
            out.name_len = n as u32;
            found = true;
        }
    }
    if !found {
        return err(f, Status::NotFound);
    }
    let bytes = unsafe {
        core::slice::from_raw_parts(&out as *const DirEntry as *const u8, size_of::<DirEntry>())
    };
    match p.copy_to_user(out_ptr, bytes) {
        Ok(()) => ok(f, bytes.len() as u64),
        Err(s) => err(f, s),
    }
}

/// `0x15 sys_clock_gettime(kind) -> ns`
fn sys_clock_gettime(f: &mut TrapFrame) {
    match f.rdi {
        k if k == Clock::Monotonic as u64 || k == Clock::Realtime as u64 => {
            ok(f, thread::ticks() * thread::TICK_MS * 1_000_000)
        }
        _ => err(f, Status::Unsupported),
    }
}
