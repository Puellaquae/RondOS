//! `init` — the first user process (P1).
//!
//! The kernel starts it with a `StartupBlock` whose capabilities contain the
//! root directory handle.  From there everything is ordinary user space: open
//! a program from the boot tar, `sys_spawn` it, `sys_wait` for it, read its
//! `ExitStatus`, and report.  The kernel has no process policy of its own.

#![no_std]
#![no_main]

use rondos_abi::{exit_kind, mem_flags, wait_reason, CapDesc, Handle, ObjKind, StartupBlock};
use rondos_rt::{
    chan_create, chan_recv, chan_send, close, kill, mem_map, open_file, open_file_flags, print,
    println, proc_status, readdir, sleep_ns, spawn, spawn_with_caps, wait, write_file,
};

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

/// Channel round-trip: delegate one end to `bin/echo`, send, receive it back.
fn check_echo_child(root: Handle) -> i32 {
    let (a, b) = step!(30, chan_create());
    let image = step!(31, open_file(root, b"/bin/echo"));
    let child = step!(
        32,
        spawn_with_caps(
            image,
            &[CapDesc {
                kind: ObjKind::Chan as u32,
                _pad0: 0,
                rights: rondos_abi::rights::READ | rondos_abi::rights::WRITE,
                handle: b.0,
            }]
        )
    );
    let _ = close(image);
    let _ = close(b);

    let msg = b"ping through a channel";
    step!(33, chan_send(a, msg));
    let mut buf = [0u8; 64];
    let n = step!(34, chan_recv(a, &mut buf));
    if &buf[..n] != msg {
        println!("init: channel echoed the wrong bytes");
        return 35;
    }
    println!("init: channel echoed {} bytes", n);

    let w = step!(36, wait(&[child], 5_000_000_000));
    if w.reason != wait_reason::EXITED {
        println!("init: echo child ended with reason {}", w.reason);
        return 37;
    }
    let _ = close(a);
    let _ = close(child);
    0
}

/// tmpfs: create a file, write it, reopen and read it back, then list the
/// directory (tar entries first, then tmpfs files).
fn check_tmpfs(root: Handle) -> i32 {
    let path = b"/tmp/note.txt";
    let flags = rondos_abi::open_flags::READ
        | rondos_abi::open_flags::WRITE
        | rondos_abi::open_flags::CREATE;
    let h = step!(60, open_file_flags(root, path, flags));
    let msg = b"tmpfs says hi";
    step!(61, write_file(h, msg));
    step!(62, close(h));

    // A fresh open starts at offset 0.
    let h = step!(63, open_file(root, path));
    let mut buf = [0u8; 32];
    let n = step!(64, rondos_rt::read(h, &mut buf));
    step!(65, close(h));
    if &buf[..n] != msg {
        println!("init: tmpfs read back {} bytes, expected {}", n, msg.len());
        return 66;
    }
    println!("init: tmpfs file round-trips ({} bytes)", n);

    let mut count = 0u32;
    while let Ok(e) = readdir(root, count) {
        if count == 0 {
            println!(
                "init: readdir[0] = {}",
                core::str::from_utf8(e.name()).unwrap_or("?")
            );
        }
        count += 1;
    }
    println!("init: readdir found {} entries", count);
    if count < 6 {
        return 67;
    }
    0
}

/// Run the C program (`bin/chello`, built by the host gcc from `user/c/`).
fn check_c_program(root: Handle) -> i32 {
    let image = step!(50, open_file(root, b"/bin/chello"));
    let child = step!(51, spawn(image));
    let _ = close(image);
    let w = step!(52, wait(&[child], 5_000_000_000));
    if w.reason != wait_reason::EXITED {
        println!("init: chello ended with reason {}", w.reason);
        return 53;
    }
    let st = step!(54, proc_status(child));
    if st.kind != exit_kind::EXITED || st.code != 0 {
        println!("init: chello exit kind {} code {}", st.kind, st.code);
        return 55;
    }
    println!("init: C program exited 0");
    let _ = close(child);
    0
}

/// Shared memory: map an object, write a pattern, read it back.
fn check_shared_memory() -> i32 {
    let (h, va) = step!(40, mem_map(4096, mem_flags::READ | mem_flags::WRITE));
    if va == 0 {
        println!("init: mem_map returned no address");
        return 41;
    }
    let p = va as *mut u64;
    unsafe {
        p.write_volatile(0xfeed_face_cafe_1234);
        if p.read_volatile() != 0xfeed_face_cafe_1234 {
            println!("init: shared memory did not round-trip");
            return 42;
        }
    }
    println!("init: shared memory at {:#x} round-trips", va);
    let _ = rondos_rt::mem_unmap(h);
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
    let rc = check_shared_memory();
    if rc != 0 {
        return rc;
    }
    let rc = check_echo_child(root);
    if rc != 0 {
        return rc;
    }
    let rc = check_c_program(root);
    if rc != 0 {
        return rc;
    }
    let rc = check_tmpfs(root);
    if rc != 0 {
        return rc;
    }

    println!("init: all children reaped, exiting");
    0
}
