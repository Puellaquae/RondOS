//! `rondos-rt` — the tiny user-space runtime.
//!
//! A RondOS program is a `no_std` binary whose entry point is the runtime's
//! `_start`; the program provides
//!
//! ```ignore
//! #[no_mangle]
//! pub extern "C" fn app_main() -> i32 { ... }
//! ```
//!
//! `_start` aligns the stack, calls `app_main`, and turns the return value into
//! `sys_exit(status)`.  There is no libc, no environment and no argv yet: the
//! kernel enters user code with `rdi` = the `StartupBlock` pointer (design
//! §5.3), which the runtime will hand to `app_main` once the ELF loader passes
//! one.

#![no_std]

use rondos_abi::{log, syscall, ExitStatus, Handle, LogLevel, ObjKind, StartupBlock, Status,
                 SyscallId, WaitResult};

pub use rondos_abi as abi;

// The program's real entry point, provided by the binary.
extern "C" {
    fn app_main(block: &StartupBlock) -> i32;
}

/// `rdi` on entry, saved by `_start` so helpers can find it.
#[no_mangle]
pub static mut STARTUP_PTR: u64 = 0;

/// The `StartupBlock` the kernel wrote at the top of the stack.
pub fn startup() -> Option<&'static StartupBlock> {
    let p = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(STARTUP_PTR)) };
    if p == 0 {
        None
    } else {
        Some(unsafe { &*(p as *const StartupBlock) })
    }
}

/// The process's root directory capability.
pub fn root_dir() -> Option<Handle> {
    startup()
        .and_then(|b| b.cap(ObjKind::Dir))
        .map(|c| Handle(c.handle))
}

/// Leave the process with `status`.  Never returns.
pub fn exit(status: u32) -> ! {
    abi::exit(status)
}

/// Ask the kernel to power the machine off (ACPI S5).
///
/// Only returns when the kernel could not shut down; use [`exit`] to leave a
/// process instead.
pub fn shutdown() -> Result<(), Status> {
    let r = abi::shutdown();
    if r.status.is_ok() {
        Ok(())
    } else {
        Err(r.status)
    }
}

/// Write raw bytes to the kernel log (`sys_log`).
pub fn print_bytes(bytes: &[u8]) {
    let _ = log(LogLevel::Info, bytes);
}

pub fn sleep_ns(ns: u64) {
    unsafe {
        syscall(SyscallId::SleepNs, ns, 0, 0, 0, 0, 0);
    }
}

// ------------------------------------------------------- typed syscall sugar

pub fn open_file(dir: Handle, path: &[u8]) -> Result<Handle, Status> {
    open_file_flags(dir, path, abi::open_flags::READ)
}

pub fn open_file_flags(dir: Handle, path: &[u8], flags: u64) -> Result<Handle, Status> {
    let r = abi::open(dir, path, flags);
    if r.status.is_ok() {
        Ok(Handle(r.value))
    } else {
        Err(r.status)
    }
}

/// Read one directory entry; `Err(NotFound)` means end of directory.
pub fn readdir(dir: Handle, index: u32) -> Result<abi::DirEntry, Status> {
    let mut out = abi::DirEntry::default();
    out.hdr.size = core::mem::size_of::<abi::DirEntry>() as u32;
    out.hdr.version = abi::STRUCT_VERSION;
    let r = abi::readdir(dir, index, &mut out);
    if r.status.is_ok() {
        Ok(out)
    } else {
        Err(r.status)
    }
}

pub fn read(handle: Handle, buf: &mut [u8]) -> Result<usize, Status> {
    let r = abi::read(handle, buf);
    if r.status.is_ok() {
        Ok(r.value as usize)
    } else {
        Err(r.status)
    }
}

pub fn write_file(handle: Handle, buf: &[u8]) -> Result<usize, Status> {
    let r = abi::write(handle, buf);
    if r.status.is_ok() {
        Ok(r.value as usize)
    } else {
        Err(r.status)
    }
}

pub fn close(handle: Handle) -> Result<(), Status> {
    abi::close(handle).status.is_ok().then_some(()).ok_or(Status::Broken)
}

pub fn spawn(image: Handle) -> Result<Handle, Status> {
    let r = abi::spawn(image);
    if r.status.is_ok() {
        Ok(Handle(r.value))
    } else {
        Err(r.status)
    }
}

/// Wait for the first of `handles` to leave `Running`.
pub fn wait(handles: &[Handle], timeout_ns: u64) -> Result<WaitResult, Status> {
    let r = abi::wait(handles, timeout_ns);
    if r.status.is_ok() {
        Ok(WaitResult::unpack(r.value))
    } else {
        Err(r.status)
    }
}

pub fn proc_status(handle: Handle) -> Result<ExitStatus, Status> {
    let mut out = ExitStatus::default();
    out.hdr.size = core::mem::size_of::<ExitStatus>() as u32;
    out.hdr.version = abi::STRUCT_VERSION;
    let r = abi::proc_status(handle, &mut out);
    if r.status.is_ok() {
        Ok(out)
    } else {
        Err(r.status)
    }
}

pub fn kill(handle: Handle) -> Result<(), Status> {
    abi::kill(handle).status.is_ok().then_some(()).ok_or(Status::Broken)
}

// ------------------------------------------------------- P2: memory, channels

/// Create and map an anonymous memory object; returns `(handle, va)`.
pub fn mem_map(len_bytes: u64, flags: u64) -> Result<(Handle, u64), Status> {
    let r = abi::mem_map(len_bytes, flags);
    if !r.status.is_ok() {
        return Err(r.status);
    }
    let handle = Handle(r.value);
    let st = stat(handle)?;
    Ok((handle, st.va))
}

pub fn mem_unmap(handle: Handle) -> Result<(), Status> {
    abi::mem_unmap(handle).status.is_ok().then_some(()).ok_or(Status::Broken)
}

pub fn mem_share(handle: Handle, rights: u64) -> Result<Handle, Status> {
    let r = abi::mem_share(handle, rights);
    if r.status.is_ok() {
        Ok(Handle(r.value))
    } else {
        Err(r.status)
    }
}

pub fn mem_map_phys(pa: u64, len_bytes: u64, cache: u64) -> Result<(Handle, u64), Status> {
    let r = abi::mem_map_phys(pa, len_bytes, cache);
    if !r.status.is_ok() {
        return Err(r.status);
    }
    let handle = Handle(r.value);
    let st = stat(handle)?;
    Ok((handle, st.va))
}

pub fn stat(handle: Handle) -> Result<abi::Stat, Status> {
    let mut out = abi::Stat::default();
    // The kernel honours the caller's declared size, so fill the header.
    out.hdr.size = core::mem::size_of::<abi::Stat>() as u32;
    out.hdr.version = abi::STRUCT_VERSION;
    let r = abi::stat(handle, &mut out);
    if r.status.is_ok() {
        Ok(out)
    } else {
        Err(r.status)
    }
}

/// Create a channel pair `(a, b)`; both ends can send and receive.
pub fn chan_create() -> Result<(Handle, Handle), Status> {
    let mut pair = [Handle::INVALID; 2];
    let r = abi::chan_create(&mut pair);
    if r.status.is_ok() {
        Ok((pair[0], pair[1]))
    } else {
        Err(r.status)
    }
}

pub fn chan_send(handle: Handle, buf: &[u8]) -> Result<usize, Status> {
    chan_send_with(handle, buf, &[])
}

/// Send a message carrying capabilities.
pub fn chan_send_with(handle: Handle, buf: &[u8], handles: &[Handle]) -> Result<usize, Status> {
    let r = abi::chan_send(handle, buf, handles);
    if r.status.is_ok() {
        Ok(r.value as usize)
    } else {
        Err(r.status)
    }
}

pub fn chan_recv(handle: Handle, buf: &mut [u8]) -> Result<usize, Status> {
    let mut none: [Handle; 0] = [];
    chan_recv_with(handle, buf, &mut none).map(|(n, _)| n)
}

/// Receive a message plus up to `out.len()` handles.  Returns
/// `(bytes, handles_received)`.
pub fn chan_recv_with(
    handle: Handle,
    buf: &mut [u8],
    out: &mut [Handle],
) -> Result<(usize, usize), Status> {
    let r = abi::chan_recv(handle, buf, out);
    if r.status.is_ok() {
        Ok((r.value as u32 as usize, (r.value >> 32) as usize))
    } else {
        Err(r.status)
    }
}

pub fn seek(handle: Handle, offset: i64, whence: u32) -> Result<u64, Status> {
    let r = abi::seek(handle, offset, whence);
    if r.status.is_ok() {
        Ok(r.value)
    } else {
        Err(r.status)
    }
}

pub fn unlink(dir: Handle, path: &[u8]) -> Result<(), Status> {
    abi::unlink(dir, path).status.is_ok().then_some(()).ok_or(Status::Broken)
}


/// `sys_spawn` with delegated capabilities.
pub fn spawn_with_caps(image: Handle, caps: &[abi::CapDesc]) -> Result<Handle, Status> {
    let r = abi::spawn_with_caps(image, caps);
    if r.status.is_ok() {
        Ok(Handle(r.value))
    } else {
        Err(r.status)
    }
}

/// `sys_info`: ABI version, feature bits, timer and framebuffer description.
pub fn info() -> Result<abi::Info, Status> {
    let mut out = abi::Info::default();
    out.hdr.size = core::mem::size_of::<abi::Info>() as u32;
    out.hdr.version = abi::STRUCT_VERSION;
    let r = unsafe {
        abi::syscall(
            abi::SyscallId::Info,
            &mut out as *mut abi::Info as u64,
            0,
            0,
            0,
            0,
            0,
        )
    };
    if r.status.is_ok() {
        Ok(out)
    } else {
        Err(r.status)
    }
}

/// The first capability of `kind` in the `StartupBlock`.
pub fn cap(kind: abi::ObjKind) -> Option<Handle> {
    startup().and_then(|b| b.cap(kind)).map(|c| Handle(c.handle))
}

/// The first capability of `kind` that carries at least `rights`.
pub fn cap_with(kind: abi::ObjKind, rights: u64) -> Option<Handle> {
    startup().and_then(|b| {
        b.caps()
            .iter()
            .find(|c| c.kind == kind as u32 && c.rights & rights == rights)
            .map(|c| Handle(c.handle))
    })
}

pub fn yield_now() {
    unsafe {
        syscall(SyscallId::Yield, 0, 0, 0, 0, 0, 0);
    }
}

/// Nanoseconds since boot.
pub fn now_ns() -> u64 {
    unsafe { syscall(SyscallId::ClockGettime, 0, 0, 0, 0, 0, 0).value }
}

/// Write a marker into `xmm0` (FPU-state test helper).
///
/// `xmm0` is declared as an explicit *output* operand (a clobber) while the
/// template writes it: that is the only way to pin a fixed FPU register without
/// the compiler materialising an input into it first.
pub fn set_xmm0(v: u64) {
    unsafe {
        core::arch::asm!(
            "movq xmm0, {}",
            in(reg) v,
            out("xmm0") _,
            options(nostack, nomem),
        );
    }
}

/// Read the marker back from `xmm0`.
pub fn xmm0() -> u64 {
    let out: u64;
    unsafe {
        core::arch::asm!(
            "movq {}, xmm0",
            out(reg) out,
            out("xmm0") _,
            options(nostack, nomem),
        );
    }
    out
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let mut logger = ::rondos_rt::Logger::new();
        let _ = write!(logger, $($arg)*);
        logger.finish();
    }};
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        // One logger for the whole line: two `print!` calls would flush the
        // text and the newline separately, producing a stray empty log line.
        let mut logger = ::rondos_rt::Logger::new();
        let _ = write!(logger, $($arg)*);
        let _ = write!(logger, "\n");
        logger.finish();
    }};
}

/// `core::fmt::Write` sink that buffers until a newline.
///
/// Without buffering every format fragment became its own `sys_log` call, so
/// `println!("x = {}", n)` produced three separate `user: ...` lines and a
/// stray empty one.  One line in, one line out (or a 200-byte flush).
pub struct Logger {
    buf: [u8; 200],
    len: usize,
}

impl Logger {
    pub const fn new() -> Self {
        Self {
            buf: [0; 200],
            len: 0,
        }
    }

    fn flush(&mut self) {
        if self.len > 0 {
            let _ = log(LogLevel::Info, &self.buf[..self.len]);
            self.len = 0;
        }
    }

    /// Flush whatever is left (no trailing newline).
    pub fn finish(&mut self) {
        self.flush();
    }
}

impl core::fmt::Write for Logger {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len == self.buf.len() {
                self.flush();
            }
            self.buf[self.len] = b;
            self.len += 1;
            if b == b'\n' {
                self.flush();
            }
        }
        Ok(())
    }
}

#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        // The kernel enters with rsp = stack_top - 16; SysV wants rsp % 16 == 8
        // *inside* the callee, i.e. rsp % 16 == 0 at the call.
        // rdi already holds &StartupBlock; keep it for `startup()`.
        "mov qword ptr [rip + {slot}], rdi",
        "xor rbp, rbp",
        "and rsp, -16",
        "call {main}",
        "mov edi, eax",
        "call {exit}",
        "ud2",
        slot = sym STARTUP_PTR,
        main = sym app_main,
        exit = sym exit_trampoline,
    )
}

/// `_start` cannot `call` a `-> !` function directly in naked asm, so bounce.
extern "C" fn exit_trampoline(status: i32) -> ! {
    exit(status as u32)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let mut logger = Logger::new();
    let _ = core::fmt::write(&mut logger, format_args!("panic: {}\n", info.message()));
    logger.finish();
    exit(101)
}

// ---------------------------------------------------------------- user heap

/// A first-fit free-list allocator over one `sys_mem_map` region.
///
/// v1 has **one thread per process**, so a process's heap is never touched
/// concurrently and needs no lock — an interrupt never re-enters user code.
/// When `sys_thread_spawn` lands, this must grow a real lock (or a lock-free
/// design), because spinning on a single CPU while preempted deadlocks.
pub mod heap {
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::UnsafeCell;
    use core::ptr;

    use crate::abi::{self, mem_flags};

    const HEAP_SIZE: u64 = 64 * 1024;
    /// Payload alignment; larger alignments are refused.
    const ALIGN: usize = 16;
    /// `FreeBlock { size, next }`.
    const HEADER: usize = 16;

    struct FreeBlock {
        /// Usable bytes after this header (allocated blocks: their own size).
        size: usize,
        next: *mut FreeBlock,
    }

    struct HeapState {
        head: *mut FreeBlock,
        /// Handle of the backing memory object, kept for the process's life.
        handle: u64,
    }

    struct Heap(UnsafeCell<HeapState>);

    unsafe impl Sync for Heap {}

    static HEAP: Heap = Heap(UnsafeCell::new(HeapState {
        head: ptr::null_mut(),
        handle: u64::MAX,
    }));

    /// `#[global_allocator]` for Rust user programs.
    pub struct RondosAlloc;

    unsafe fn init(st: &mut HeapState) -> bool {
        let r = abi::mem_map(HEAP_SIZE, mem_flags::READ | mem_flags::WRITE);
        if !r.status.is_ok() {
            return false;
        }
        // Use the typed wrapper: it fills hdr.size, which the kernel now
        // enforces (a zero size is an invalid caller).
        let stat = match crate::stat(abi::Handle(r.value)) {
            Ok(s) if s.va != 0 => s,
            _ => return false,
        };
        st.handle = r.value;
        st.head = stat.va as *mut FreeBlock;
        (*st.head).size = HEAP_SIZE as usize - HEADER;
        (*st.head).next = ptr::null_mut();
        true
    }

    unsafe impl GlobalAlloc for RondosAlloc {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if layout.align() > ALIGN || layout.size() == 0 {
                return ptr::null_mut();
            }
            let st = &mut *HEAP.0.get();
            if st.head.is_null() && !init(st) {
                return ptr::null_mut();
            }
            let need = (layout.size() + ALIGN - 1) & !(ALIGN - 1);

            let mut prev: *mut FreeBlock = ptr::null_mut();
            let mut cur = st.head;
            while !cur.is_null() {
                let b = &mut *cur;
                if b.size >= need {
                    if b.size >= need + HEADER {
                        // Split: the tail stays free.
                        let rest = (cur as usize + HEADER + need) as *mut FreeBlock;
                        (*rest).size = b.size - need - HEADER;
                        (*rest).next = b.next;
                        b.size = need;
                        b.next = rest;
                        if prev.is_null() {
                            st.head = rest;
                        } else {
                            (*prev).next = rest;
                        }
                    } else {
                        // Consume the whole block.
                        if prev.is_null() {
                            st.head = b.next;
                        } else {
                            (*prev).next = b.next;
                        }
                    }
                    return (cur as usize + HEADER) as *mut u8;
                }
                prev = cur;
                cur = b.next;
            }
            ptr::null_mut()
        }

        unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
            if ptr.is_null() {
                return;
            }
            let st = &mut *HEAP.0.get();
            let blk = (ptr as usize - HEADER) as *mut FreeBlock;
            let size = (*blk).size;

            // Insert in address order so neighbours can be coalesced.
            let mut prev: *mut FreeBlock = ptr::null_mut();
            let mut cur = st.head;
            while !cur.is_null() && (cur as usize) < (blk as usize) {
                prev = cur;
                cur = (*cur).next;
            }
            (*blk).next = cur;
            if prev.is_null() {
                st.head = blk;
            } else {
                (*prev).next = blk;
            }

            if !cur.is_null() && blk as usize + HEADER + size == cur as usize {
                (*blk).size += HEADER + (*cur).size;
                (*blk).next = (*cur).next;
            }
            if !prev.is_null() && prev as usize + HEADER + (*prev).size == blk as usize {
                (*prev).size += HEADER + (*blk).size;
                (*prev).next = (*blk).next;
            }
        }
    }
}
