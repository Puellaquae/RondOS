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
    let r = abi::open(dir, path, abi::open_flags::READ);
    if r.status.is_ok() {
        Ok(Handle(r.value))
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
        let _ = write!(::rondos_rt::Logger, $($arg)*);
    }};
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {{
        $crate::print!($($arg)*);
        $crate::print!("\n");
    }};
}

/// Minimal `core::fmt::Write` sink that pushes each chunk through `sys_log`.
pub struct Logger;

impl core::fmt::Write for Logger {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let mut off = 0;
        while off < bytes.len() {
            let n = (bytes.len() - off).min(256);
            let _ = log(LogLevel::Info, &bytes[off..off + n]);
            off += n;
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
    let mut logger = Logger;
    let _ = core::fmt::write(&mut logger, format_args!("panic: {}\n", info.message()));
    exit(101)
}
