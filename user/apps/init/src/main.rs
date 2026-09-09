//! `init` — the first user process (P1).
//!
//! The kernel starts it with a `StartupBlock` whose capabilities contain the
//! root directory handle.  From there everything is ordinary user space: open
//! a program from the boot tar, `sys_spawn` it, `sys_wait` for it, read its
//! `ExitStatus`, and report.  The kernel has no process policy of its own.

#![no_std]
#![no_main]

use rondos_abi::{exit_kind, wait_reason, Handle, ObjKind, StartupBlock};
use rondos_rt::{close, kill, open_file, print, println, proc_status, sleep_ns, spawn, wait};

/// Touch .data so the image carries a writable, non-executable segment: the
/// loader must map it RW + NX while .text stays R + X.
static mut COUNTER: u64 = 41;

/// Report a failing step as a distinct exit code so the kernel log says where.
macro_rules! step {
    ($code:expr, $what:expr) => {
        match $what {
            Ok(v) => v,
            Err(e) => {
                println!("init: {} failed: {:?}", stringify!($what), e);
                return $code;
            }
        }
    };
}

fn check_fault_child(root: Handle) -> i32 {
    let image = step!(10, open_file(root, b"/bin/crash"));
    let child = step!(11, spawn(image));
    let _ = close(image);

    let w = step!(12, wait(&[child], 5_000_000_000));
    if w.reason != wait_reason::FAULT {
        println!("init: crash child ended with reason {}", w.reason);
        return 13;
    }
    let st = step!(14, proc_status(child));
    if st.kind != exit_kind::FAULT || st.vector != 14 || st.addr != 0xdead_beef {
        println!(
            "init: unexpected status kind {} vector {:#x} addr {:#x}",
            st.kind, st.vector, st.addr
        );
        return 15;
    }
    println!(
        "init: child crash faulted at {:#x} (vector {})",
        st.addr, st.vector
    );
    let _ = close(child);
    0
}

fn check_killed_child(root: Handle) -> i32 {
    // Put a marker in xmm0; the child below hammers xmm0 with a different
    // value, so getting ours back proves the kernel saves FPU state per thread.
    const MARKER: u64 = 0x600d_f00d_1234_5678;
    rondos_rt::set_xmm0(MARKER);

    let image = step!(20, open_file(root, b"/bin/spin"));
    let child = step!(21, spawn(image));
    let _ = close(image);

    // Let it run, then kill it: `sys_wait` must report Killed, not a timeout.
    sleep_ns(50_000_000);
    let xmm = rondos_rt::xmm0();
    if xmm != MARKER {
        println!("init: xmm0 lost across switches: {:#x}", xmm);
        return 25;
    }
    step!(22, kill(child));

    let w = step!(23, wait(&[child], 5_000_000_000));
    if w.reason != wait_reason::KILLED {
        println!("init: spin child ended with reason {}", w.reason);
        return 24;
    }
    println!("init: child spin killed after 50 ms (xmm0 preserved)");
    let _ = close(child);
    0
}

#[no_mangle]
pub extern "C" fn app_main(block: &StartupBlock) -> i32 {
    print!("init: hello from ring 3\n");
    unsafe { COUNTER += 1 };
    println!(
        "init: abi {} counter {} seed {:#x}",
        block.abi_version,
        unsafe { COUNTER },
        block.random_seed
    );

    let Some(root) = block.cap(ObjKind::Dir).map(|c| Handle(c.handle)) else {
        println!("init: no root capability in the StartupBlock");
        return 1;
    };
    println!("init: root capability handle {:#x}", root.0);

    let rc = check_fault_child(root);
    if rc != 0 {
        return rc;
    }
    let rc = check_killed_child(root);
    if rc != 0 {
        return rc;
    }

    println!("init: all children reaped, exiting");
    0
}
