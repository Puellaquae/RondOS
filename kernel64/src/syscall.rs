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
use crate::proc::{self, ExitStatus};
use crate::thread;
use rondos_abi::{
    feature, Clock, Info, LogLevel, Status, StructHeader, SyscallId, ABI_VERSION,
};

/// Largest `sys_log` payload accepted in one call (keeps the copy on-stack).
const MAX_LOG_LEN: usize = 512;

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
        SyscallId::ClockGettime => sys_clock_gettime(f),
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
    let text = core::str::from_utf8(&buf[..len]).unwrap_or("<non-utf8>");
    crate::serial_println!("user: {}", text.trim_end_matches('\n'));
    remember_log(&buf[..len]);
    LOGGED_BYTES.fetch_add(len, core::sync::atomic::Ordering::Relaxed);
    ok(f, len as u64);
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

/// `0x15 sys_clock_gettime(kind) -> ns`
fn sys_clock_gettime(f: &mut TrapFrame) {
    match f.rdi {
        k if k == Clock::Monotonic as u64 || k == Clock::Realtime as u64 => {
            ok(f, thread::ticks() * thread::TICK_MS * 1_000_000)
        }
        _ => err(f, Status::Unsupported),
    }
}
