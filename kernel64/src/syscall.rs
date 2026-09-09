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
use crate::exec;
use crate::fs;
use crate::proc::{self, ExitStatus, ObjRef};
use crate::thread;
use rondos_abi::{
    feature, open_flags, rights, wait_reason, Clock, Handle, Info, LogLevel, Status,
    StructHeader, SyscallId, WaitResult, ABI_VERSION,
};

/// Largest `sys_log` payload accepted in one call (keeps the copy on-stack).
const MAX_LOG_LEN: usize = 512;

/// `sys_wait` accepts at most this many handles in one call.
const MAX_WAIT_HANDLES: usize = 16;

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
    if flags & open_flags::WRITE != 0 {
        return err(f, Status::Permission); // tarfs is read-only
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
    match p.handles().resolve(h, rights::WRITE) {
        Ok(slot) => match slot.obj {
            ObjRef::Device { node: 0 } => {}
            ObjRef::Device { .. } => return err(f, Status::NotFound),
            _ => return err(f, Status::Permission),
        },
        Err(s) => return err(f, s),
    }
    let mut buf = [0u8; MAX_LOG_LEN];
    if let Err(s) = p.copy_from_user(&mut buf[..len], f.rsi) {
        return err(f, s);
    }
    log_bytes(&buf[..len]);
    ok(f, len as u64)
}

/// `0x56 sys_close(handle)`
fn sys_close(f: &mut TrapFrame) {
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
    match p.handles_mut().close(Handle(f.rdi)) {
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
    let Some(p) = proc::current() else {
        return err(f, Status::BadAddress);
    };
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
    let pid = match exec::spawn_entry(&entry) {
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

/// `0x15 sys_clock_gettime(kind) -> ns`
fn sys_clock_gettime(f: &mut TrapFrame) {
    match f.rdi {
        k if k == Clock::Monotonic as u64 || k == Clock::Realtime as u64 => {
            ok(f, thread::ticks() * thread::TICK_MS * 1_000_000)
        }
        _ => err(f, Status::Unsupported),
    }
}
