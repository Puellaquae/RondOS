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

// -------------------------------------------------------------- startup block

/// A borrowed string inside the caller's address space (`ptr`/`len_bytes`).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct StrRef {
    pub ptr: u64,
    pub len_bytes: u64,
}

/// A borrowed array (`ptr`/`count`); element type depends on the field.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Slice {
    pub ptr: u64,
    pub count: u64,
}

/// One capability handed to a new process at startup.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CapDesc {
    pub kind: u32,
    pub _pad0: u32,
    pub rights: u64,
    pub handle: u64,
}

/// Written by the kernel at the top of a new process's stack; `rdi` points at
/// it on entry (design §5.3).  Starts with a [`StructHeader`] so fields can be
/// appended without breaking older programs.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct StartupBlock {
    pub hdr: StructHeader,
    pub abi_version: u32,
    pub _pad0: u32,
    pub feature_bits: u64,
    pub entry: u64,
    pub image_base: u64,
    /// `Slice<StrRef>` — empty in P1 (argv arrives with the manifest work).
    pub argv: Slice,
    pub envp: Slice,
    /// `Slice<CapDesc>` — the capabilities actually granted.
    pub caps: Slice,
    /// Reserved for a display-server-created window handle.
    pub window: Handle,
    pub random_seed: u64,
    pub _reserved: [u64; 4],
}

impl StartupBlock {
    pub const MAGIC: u32 = 0x524E_4432; // "RND2"

    /// The granted capabilities, if the block is big enough to contain them.
    pub fn caps(&self) -> &[CapDesc] {
        if self.caps.ptr == 0 || self.caps.count == 0 {
            return &[];
        }
        unsafe { core::slice::from_raw_parts(self.caps.ptr as *const CapDesc, self.caps.count as usize) }
    }

    /// The first capability of `kind`, if any.
    pub fn cap(&self, kind: ObjKind) -> Option<CapDesc> {
        self.caps().iter().copied().find(|c| c.kind == kind as u32)
    }
}

// --------------------------------------------------------------- exit status

/// How a process ended.  `kind` is one of the [`exit_kind`] values.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ExitStatus {
    pub hdr: StructHeader,
    /// [`exit_kind`] value.
    pub kind: u32,
    /// `sys_exit` status for `Exited`, otherwise 0.
    pub code: u32,
    /// Faulting vector for `Fault`, otherwise 0.
    pub vector: u32,
    pub _pad0: u32,
    /// Faulting instruction pointer for `Fault`.
    pub rip: u64,
    /// Faulting address (`CR2` for `#PF`).
    pub addr: u64,
    pub _reserved: [u64; 2],
}

pub mod exit_kind {
    pub const RUNNING: u32 = 0;
    pub const EXITED: u32 = 1;
    pub const FAULT: u32 = 2;
    pub const KILLED: u32 = 3;
}

/// `sys_wait` return value: `index | (reason << 32)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WaitResult {
    pub index: u32,
    pub reason: u32,
}

impl WaitResult {
    pub const fn pack(index: u32, reason: u32) -> u64 {
        (index as u64) | ((reason as u64) << 32)
    }

    pub const fn unpack(value: u64) -> Self {
        Self {
            index: value as u32,
            reason: (value >> 32) as u32,
        }
    }
}

/// Why a handle became ready (matches [`exit_kind`]).
pub mod wait_reason {
    pub const EXITED: u32 = 1;
    pub const FAULT: u32 = 2;
    pub const KILLED: u32 = 3;
}

/// Flags for `sys_mem_map` (and the permissions of the resulting mapping).
pub mod mem_flags {
    pub const READ: u64 = 1 << 0;
    pub const WRITE: u64 = 1 << 1;
    pub const EXEC: u64 = 1 << 2;
    /// Shareable with other processes.
    pub const SHARE: u64 = 1 << 3;
    pub const RW: u64 = READ | WRITE;
}

/// `sys_stat` output.  `va` is where the handle is mapped in *this* process
/// (memory objects), 0 otherwise.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Stat {
    pub hdr: StructHeader,
    /// [`ObjKind`] value.
    pub kind: u32,
    pub _pad0: u32,
    pub len_bytes: u64,
    pub va: u64,
    pub rights: u64,
    pub _reserved: [u64; 2],
}

/// Open flags for `sys_open`.
pub mod open_flags {
    pub const READ: u64 = 1 << 0;
    pub const WRITE: u64 = 1 << 1;
    /// Create the file if it does not exist (tmpfs layer).
    pub const CREATE: u64 = 1 << 2;
}

/// One `sys_readdir` entry.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DirEntry {
    pub hdr: StructHeader,
    /// [`ObjKind`] value (File for regular files).
    pub kind: u32,
    pub _pad0: u32,
    pub len_bytes: u64,
    pub name_len: u32,
    pub _pad1: u32,
    pub name: [u8; 64],
    pub _reserved: [u64; 2],
}

impl Default for DirEntry {
    fn default() -> Self {
        Self {
            hdr: StructHeader::default(),
            kind: 0,
            _pad0: 0,
            len_bytes: 0,
            name_len: 0,
            _pad1: 0,
            name: [0; 64],
            _reserved: [0; 2],
        }
    }
}

impl DirEntry {
    pub fn name(&self) -> &[u8] {
        &self.name[..(self.name_len as usize).min(self.name.len())]
    }
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
    assert!(core::mem::size_of::<StrRef>() == 16);
    assert!(core::mem::size_of::<Slice>() == 16);
    assert!(core::mem::size_of::<CapDesc>() == 24);
    assert!(core::mem::size_of::<StartupBlock>() == 136);
    assert!(core::mem::offset_of!(StartupBlock, argv) == 40);
    assert!(core::mem::offset_of!(StartupBlock, caps) == 72);
    assert!(core::mem::size_of::<ExitStatus>() == 56);
    assert!(core::mem::offset_of!(ExitStatus, rip) == 24);
    assert!(core::mem::size_of::<Stat>() == 56);
    assert!(core::mem::offset_of!(Stat, va) == 24);
    assert!(core::mem::size_of::<DirEntry>() == 112);
    assert!(core::mem::offset_of!(DirEntry, name) == 32);
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

/// `0x50 sys_open(dir, path, flags) -> Handle<File>`
#[cfg(feature = "user")]
pub fn open(dir: Handle, path: &[u8], flags: u64) -> SyscallResult {
    unsafe {
        syscall(
            SyscallId::Open,
            dir.0,
            path.as_ptr() as u64,
            path.len() as u64,
            flags,
            0,
            0,
        )
    }
}

/// `0x51 sys_read(handle, buf, len) -> n`
#[cfg(feature = "user")]
pub fn read(handle: Handle, buf: &mut [u8]) -> SyscallResult {
    unsafe {
        syscall(
            SyscallId::Read,
            handle.0,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
            0,
            0,
            0,
        )
    }
}

/// `0x52 sys_write(handle, buf, len) -> n`
#[cfg(feature = "user")]
pub fn write(handle: Handle, buf: &[u8]) -> SyscallResult {
    unsafe {
        syscall(
            SyscallId::Write,
            handle.0,
            buf.as_ptr() as u64,
            buf.len() as u64,
            0,
            0,
            0,
        )
    }
}

/// `0x56 sys_close(handle)`
#[cfg(feature = "user")]
pub fn close(handle: Handle) -> SyscallResult {
    unsafe { syscall(SyscallId::Close, handle.0, 0, 0, 0, 0, 0) }
}

/// `0x20 sys_spawn(image) -> Handle<Process>` (no extra capabilities).
#[cfg(feature = "user")]
pub fn spawn(image: Handle) -> SyscallResult {
    unsafe { syscall(SyscallId::Spawn, image.0, 0, 0, 0, 0, 0) }
}

/// `0x20 sys_spawn(image, argv, envp, caps, flags) -> Handle<Process>`.
///
/// `argv`/`envp` must be empty slices in v1; `caps` is a `Slice<CapDesc>`
/// naming capabilities of the *caller* to delegate to the child.
#[cfg(feature = "user")]
pub fn spawn_with_caps(image: Handle, caps: &[CapDesc]) -> SyscallResult {
    let slice = Slice {
        ptr: caps.as_ptr() as u64,
        count: caps.len() as u64,
    };
    unsafe {
        // rdi=image, rsi=argv, rdx=envp, r10=caps, r8=flags
        syscall(
            SyscallId::Spawn,
            image.0,
            0,
            0,
            core::ptr::addr_of!(slice) as u64,
            0,
            0,
        )
    }
}

/// `0x21 sys_wait(handles, timeout_ns) -> index | (reason << 32)`
#[cfg(feature = "user")]
pub fn wait(handles: &[Handle], timeout_ns: u64) -> SyscallResult {
    unsafe {
        syscall(
            SyscallId::Wait,
            handles.as_ptr() as u64,
            handles.len() as u64,
            timeout_ns,
            0,
            0,
            0,
        )
    }
}

/// `0x22 sys_proc_status(handle, &mut ExitStatus)`
#[cfg(feature = "user")]
pub fn proc_status(handle: Handle, out: &mut ExitStatus) -> SyscallResult {
    unsafe {
        syscall(
            SyscallId::ProcStatus,
            handle.0,
            out as *mut ExitStatus as u64,
            0,
            0,
            0,
            0,
        )
    }
}

/// `0x23 sys_kill(handle)`
#[cfg(feature = "user")]
pub fn kill(handle: Handle) -> SyscallResult {
    unsafe { syscall(SyscallId::Kill, handle.0, 0, 0, 0, 0, 0) }
}

/// `0x30 sys_mem_map(len, flags) -> Handle<Memory>`
#[cfg(feature = "user")]
pub fn mem_map(len_bytes: u64, flags: u64) -> SyscallResult {
    unsafe { syscall(SyscallId::MemMap, len_bytes, flags, 0, 0, 0, 0) }
}

/// `0x31 sys_mem_unmap(handle)`
#[cfg(feature = "user")]
pub fn mem_unmap(handle: Handle) -> SyscallResult {
    unsafe { syscall(SyscallId::MemUnmap, handle.0, 0, 0, 0, 0, 0) }
}

/// `0x32 sys_mem_share(handle, rights) -> Handle<Memory>`
#[cfg(feature = "user")]
pub fn mem_share(handle: Handle, rights: u64) -> SyscallResult {
    unsafe { syscall(SyscallId::MemShare, handle.0, rights, 0, 0, 0, 0) }
}

/// `0x33 sys_mem_map_phys(pa, len, cache) -> Handle<Memory>` (needs `DEVICE_MAP`)
#[cfg(feature = "user")]
pub fn mem_map_phys(pa: u64, len_bytes: u64, cache: u64) -> SyscallResult {
    unsafe { syscall(SyscallId::MemMapPhys, pa, len_bytes, cache, 0, 0, 0) }
}

/// `0x40 sys_chan_create(out: &mut [Handle; 2])`
#[cfg(feature = "user")]
pub fn chan_create(out: &mut [Handle; 2]) -> SyscallResult {
    unsafe { syscall(SyscallId::ChanCreate, out.as_mut_ptr() as u64, 0, 0, 0, 0, 0) }
}

/// `0x41 sys_chan_send(handle, buf, len) -> n`
#[cfg(feature = "user")]
pub fn chan_send(handle: Handle, buf: &[u8]) -> SyscallResult {
    unsafe {
        syscall(
            SyscallId::ChanSend,
            handle.0,
            buf.as_ptr() as u64,
            buf.len() as u64,
            0,
            0,
            0,
        )
    }
}

/// `0x42 sys_chan_recv(handle, buf, len) -> n`
#[cfg(feature = "user")]
pub fn chan_recv(handle: Handle, buf: &mut [u8]) -> SyscallResult {
    unsafe {
        syscall(
            SyscallId::ChanRecv,
            handle.0,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
            0,
            0,
            0,
        )
    }
}

/// `0x55 sys_readdir(dir, index, &mut DirEntry)`
#[cfg(feature = "user")]
pub fn readdir(dir: Handle, index: u32, out: &mut DirEntry) -> SyscallResult {
    unsafe {
        syscall(
            SyscallId::Readdir,
            dir.0,
            index as u64,
            out as *mut DirEntry as u64,
            0,
            0,
            0,
        )
    }
}

/// `0x54 sys_stat(handle, &mut Stat)`
#[cfg(feature = "user")]
pub fn stat(handle: Handle, out: &mut Stat) -> SyscallResult {
    unsafe {
        syscall(
            SyscallId::Stat,
            handle.0,
            out as *mut Stat as u64,
            0,
            0,
            0,
            0,
        )
    }
}
