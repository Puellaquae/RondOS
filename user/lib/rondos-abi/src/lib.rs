//! The RondOS kernel ⇄ user ABI — the single source of truth.
//!
//! Both sides depend on this crate (`default-features = false`; the kernel
//! enables `kernel`, user programs enable `user`), so syscall numbers, status
//! codes, handle encoding and struct layouts cannot drift apart.
//!
//! Rules from `docs/user-mode-design.md` §6.8, applied literally here:
//!
//! * addresses/lengths/pointers are `u64`, enums/flags/counts are `u32`;
//! * no `bool`, no `char`, no implicit `repr` on enums, no implicit padding;
//! * every struct starts with a [`StructHeader`] and only grows at the tail;
//! * syscall ids, `Status` values, `Rights` bits and the handle encoding are
//!   **frozen**: they are never reused, removed values become tombstones.
//!
//! The layout assertions at the bottom of this file are the ABI test: they run
//! at compile time on both sides of the boundary.

#![no_std]
#![allow(dead_code)]

/// Major version of the syscall ABI.  Bumped only for a breaking change.
pub const ABI_VERSION: u32 = 1;

/// `StructHeader.version` value every v1 structure is built with.
pub const STRUCT_VERSION: u32 = 1;

/// Every ABI structure starts with this.  `size` is the writer's full struct
/// size, `version` its layout version; a reader consumes `min(size, its own)`.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct StructHeader {
    /// Size of the *writer's* structure in bytes.
    pub size: u32,
    /// Layout version of the writer's structure.
    pub version: u32,
}

impl StructHeader {
    pub const fn new(size: u32) -> Self {
        Self {
            size,
            version: STRUCT_VERSION,
        }
    }

    /// True when the caller's buffer is at least as large as this struct.
    pub const fn covers<T>(&self) -> bool {
        self.size as usize >= core::mem::size_of::<T>()
    }
}

// ------------------------------------------------------------------ syscalls

/// `rax` on entry to `int 0x80`.  Values are frozen forever.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SyscallId {
    // --- 0x00 system information
    Info = 0x00,

    // --- 0x10 thread / process control
    Exit = 0x10,
    ThreadExit = 0x11,
    ThreadSpawn = 0x12,
    Yield = 0x13,
    SleepNs = 0x14,
    ClockGettime = 0x15,
    Log = 0x16,

    // --- 0x20 process lifecycle (P1)
    Spawn = 0x20,
    Wait = 0x21,
    ProcStatus = 0x22,
    Kill = 0x23,

    // --- 0x30 memory (P2)
    MemMap = 0x30,
    MemUnmap = 0x31,
    MemShare = 0x32,
    MemMapPhys = 0x33,

    // --- 0x40 channels / IPC (P2)
    ChanCreate = 0x40,
    ChanSend = 0x41,
    ChanRecv = 0x42,
    ChanClose = 0x43,

    // --- 0x50 files (P2)
    Open = 0x50,
    Read = 0x51,
    Write = 0x52,
    Seek = 0x53,
    Stat = 0x54,
    Readdir = 0x55,
    Close = 0x56,
}

impl SyscallId {
    /// Decode `rax`.  Unknown values must become `Status::Unsupported`, never a
    /// panic (design §6.8: "枚举只增不减").
    pub const fn from_raw(raw: u64) -> Option<SyscallId> {
        Some(match raw {
            0x00 => SyscallId::Info,
            0x10 => SyscallId::Exit,
            0x11 => SyscallId::ThreadExit,
            0x12 => SyscallId::ThreadSpawn,
            0x13 => SyscallId::Yield,
            0x14 => SyscallId::SleepNs,
            0x15 => SyscallId::ClockGettime,
            0x16 => SyscallId::Log,
            0x20 => SyscallId::Spawn,
            0x21 => SyscallId::Wait,
            0x22 => SyscallId::ProcStatus,
            0x23 => SyscallId::Kill,
            0x30 => SyscallId::MemMap,
            0x31 => SyscallId::MemUnmap,
            0x32 => SyscallId::MemShare,
            0x33 => SyscallId::MemMapPhys,
            0x40 => SyscallId::ChanCreate,
            0x41 => SyscallId::ChanSend,
            0x42 => SyscallId::ChanRecv,
            0x43 => SyscallId::ChanClose,
            0x50 => SyscallId::Open,
            0x51 => SyscallId::Read,
            0x52 => SyscallId::Write,
            0x53 => SyscallId::Seek,
            0x54 => SyscallId::Stat,
            0x55 => SyscallId::Readdir,
            0x56 => SyscallId::Close,
            _ => return None,
        })
    }

    /// `(id, arg0, arg1, arg2)` helper for the user-side stub.
    #[cfg(feature = "user")]
    pub const fn raw(self) -> u64 {
        self as u64
    }
}

// ------------------------------------------------------------------- status

/// `rax` on return from `int 0x80`; `rdx` carries the value.  Frozen values.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Ok = 0,
    /// The syscall id or a flag is unknown to this kernel.
    Unsupported = 1,
    /// A pointer/length argument is malformed.
    InvalidArgument = 2,
    /// A user pointer does not lie inside a single writable/readable VMA.
    BadAddress = 3,
    /// The handle is closed, forged or from another process.
    BadHandle = 4,
    /// The handle exists but lacks the required rights.
    Permission = 5,
    NotFound = 6,
    OutOfMemory = 7,
    /// Would block, but the caller asked for a non-blocking operation.
    NotReady = 8,
    /// The operation was cancelled (`sys_kill`).
    Cancelled = 9,
    /// The peer/object is gone.
    Broken = 10,
    /// The current context was terminated by a fault (never returned).
    Fault = 11,
}

impl Status {
    pub const fn is_ok(self) -> bool {
        matches!(self, Status::Ok)
    }
}

/// Result pair exactly as it crosses the boundary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SyscallResult {
    pub status: Status,
    pub value: u64,
}

// ------------------------------------------------------------------ handles

/// `index:32 | generation:32` — frozen encoding (design §6.4).
///
/// The low 32 bits index a process-private slot, the high 32 bits are the
/// slot's generation.  Closing a handle bumps the generation, so a dangling
/// handle can never be confused with a freshly reused slot.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Handle(pub u64);

impl Handle {
    /// Value that is never a valid handle.
    pub const INVALID: Handle = Handle(u64::MAX);

    pub const fn new(index: u32, generation: u32) -> Self {
        Self((index as u64) | ((generation as u64) << 32))
    }

    pub const fn index(self) -> u32 {
        self.0 as u32
    }

    pub const fn generation(self) -> u32 {
        (self.0 >> 32) as u32
    }

    pub const fn is_valid(self) -> bool {
        self.0 != u64::MAX
    }
}

/// Kernel object classes a handle can point at.  Frozen.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ObjKind {
    None = 0,
    Process = 1,
    Thread = 2,
    Memory = 3,
    Chan = 4,
    File = 5,
    Dir = 6,
    Device = 7,
}

/// Rights bits carried by every handle.  Frozen.
pub mod rights {
    pub const NONE: u64 = 0;
    pub const READ: u64 = 1 << 0;
    pub const WRITE: u64 = 1 << 1;
    pub const EXEC: u64 = 1 << 2;
    pub const MAP: u64 = 1 << 3;
    pub const SHARE: u64 = 1 << 4;
    pub const KILL: u64 = 1 << 5;
    pub const WAIT: u64 = 1 << 6;
    pub const ALL: u64 = u64::MAX;
}

// ---------------------------------------------------------------- info/clock

/// Feature bits reported by `sys_info`.  Frozen; unknown bits are ignored.
pub mod feature {
    pub const SYSCALL_FAST: u64 = 1 << 0;
    pub const DEMAND_PAGING: u64 = 1 << 1;
    pub const ASLR: u64 = 1 << 2;
    pub const DEVICE_MAP: u64 = 1 << 3;
    pub const SSE: u64 = 1 << 4;
    pub const NUMA_HINT: u64 = 1 << 5;
}

#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Clock {
    /// Time since boot.
    Monotonic = 0,
    /// Wall-clock time (not implemented in v1; returns monotonic).
    Realtime = 1,
}

#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogLevel {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

/// Pixel layout of the linear framebuffer, as handed to user space.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FramebufferInfo {
    pub phys: u64,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bpp: u8,
    /// 1 = BGRx (GOP's usual BGRA), 2 = RGBx.
    pub format: u8,
    pub _pad0: [u8; 2],
    pub _reserved: [u32; 2],
}

/// `sys_info` output.  The caller fills `hdr` (size/version); the kernel writes
/// `min(hdr.size, size_of::<Info>())` bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Info {
    pub hdr: StructHeader,
    pub abi_version: u32,
    /// Non-zero when a framebuffer is present.
    pub fb_present: u32,
    pub features: u64,
    pub page_size: u32,
    pub tick_ms: u32,
    pub ticks: u64,
    /// Nanoseconds per tick (`tick_ms * 1_000_000`), for cheap arithmetic.
    pub ns_per_tick: u64,
    pub fb: FramebufferInfo,
    pub _reserved: [u64; 4],
}

// ------------------------------------------------------------------- layout

// These assertions are the ABI freeze test (design §6.8).  They compile on the
// kernel and user side alike, so a layout change cannot slip through.
const _: () = {
    assert!(core::mem::size_of::<StructHeader>() == 8);
    assert!(core::mem::align_of::<StructHeader>() == 4);
    assert!(core::mem::size_of::<Handle>() == 8);
    assert!(core::mem::size_of::<SyscallResult>() == 16);
    assert!(core::mem::size_of::<FramebufferInfo>() == 32);
    assert!(core::mem::offset_of!(FramebufferInfo, phys) == 0);
    assert!(core::mem::offset_of!(FramebufferInfo, pitch) == 16);
    assert!(core::mem::offset_of!(FramebufferInfo, bpp) == 20);
    assert!(core::mem::offset_of!(FramebufferInfo, format) == 21);
    assert!(core::mem::size_of::<Info>() == 112);
    assert!(core::mem::offset_of!(Info, abi_version) == 8);
    assert!(core::mem::offset_of!(Info, features) == 16);
    assert!(core::mem::offset_of!(Info, ticks) == 32);
    assert!(core::mem::offset_of!(Info, fb) == 48);
    assert!(core::mem::size_of::<SyscallId>() == 4);
    assert!(core::mem::size_of::<Status>() == 4);
};

// --------------------------------------------------------------- user stubs

/// Enter the kernel.  `rax` = syscall id, `rdi/rsi/rdx/r10/r8/r9` = arg0..arg5,
/// returns `(Status, value)` from `rax`/`rdx`.  Every other register survives
/// because the kernel stub saves and restores the whole frame.
#[cfg(feature = "user")]
#[inline]
pub unsafe fn syscall(
    id: SyscallId,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
) -> SyscallResult {
    let mut rax = id as u64;
    // `rdx` is both arg2 and the returned value.
    let mut rdx = a2;
    core::arch::asm!(
        "int 0x80",
        inout("rax") rax,
        inout("rdx") rdx,
        inlateout("rdi") a0 => _,
        inlateout("rsi") a1 => _,
        inlateout("r10") a3 => _,
        inlateout("r8") a4 => _,
        inlateout("r9") a5 => _,
        options(nostack),
    );
    SyscallResult {
        status: status_from_raw(rax),
        value: rdx,
    }
}

/// Decode a raw `rax`; unknown values are `Status::Broken` (a kernel that
/// returns garbage is a kernel bug, not an unsupported call).
#[cfg(feature = "user")]
pub const fn status_from_raw(raw: u64) -> Status {
    match raw {
        0 => Status::Ok,
        1 => Status::Unsupported,
        2 => Status::InvalidArgument,
        3 => Status::BadAddress,
        4 => Status::BadHandle,
        5 => Status::Permission,
        6 => Status::NotFound,
        7 => Status::OutOfMemory,
        8 => Status::NotReady,
        9 => Status::Cancelled,
        10 => Status::Broken,
        11 => Status::Fault,
        _ => Status::Broken,
    }
}

#[cfg(feature = "user")]
pub fn log(level: LogLevel, bytes: &[u8]) -> SyscallResult {
    unsafe { syscall(SyscallId::Log, level as u64, bytes.as_ptr() as u64, bytes.len() as u64, 0, 0, 0) }
}

#[cfg(feature = "user")]
pub fn exit(status: u32) -> ! {
    unsafe {
        syscall(SyscallId::Exit, status as u64, 0, 0, 0, 0, 0);
    }
    // A conforming kernel never returns from sys_exit.
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(feature = "user")]
pub fn yield_now() {
    unsafe {
        syscall(SyscallId::Yield, 0, 0, 0, 0, 0, 0);
    }
}
