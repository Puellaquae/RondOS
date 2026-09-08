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

use rondos_abi::{log, syscall, LogLevel, SyscallId};

pub use rondos_abi as abi;

// The program's real entry point, provided by the binary.
extern "C" {
    fn app_main() -> i32;
}

/// Leave the process with `status`.  Never returns.
pub fn exit(status: u32) -> ! {
    abi::exit(status)
}

pub fn write(bytes: &[u8]) {
    let _ = log(LogLevel::Info, bytes);
}

pub fn sleep_ns(ns: u64) {
    unsafe {
        syscall(SyscallId::SleepNs, ns, 0, 0, 0, 0, 0);
    }
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
        "xor rbp, rbp",
        "and rsp, -16",
        "call {main}",
        "mov edi, eax",
        "call {exit}",
        "ud2",
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
