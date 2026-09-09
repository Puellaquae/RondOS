//! `init` — PID 1.
//!
//! The kernel starts this after its boot self-checks (`make run`).  It holds
//! the root directory and device capabilities and will spawn the display
//! server, shell and progman once those exist; today it just announces itself
//! and stays alive, because PID 1 must never exit.

#![no_std]
#![no_main]

use rondos_abi::{CapDesc, ObjKind, StartupBlock};

#[no_mangle]
pub extern "C" fn app_main(block: &StartupBlock) -> i32 {
    rondos_rt::println!(
        "init: pid 1 up (abi {}, {} capabilities)",
        block.abi_version,
        block.caps().len()
    );
    let Some(root) = rondos_rt::root_dir() else {
        rondos_rt::println!("init: no root capability");
        return 1;
    };
    rondos_rt::println!("init: root directory capability {:#x}", root.0);

    // Start the shell with the console and keyboard capabilities delegated to
    // it, then reap it and start it again (PID 1 supervises).
    let con = rondos_rt::cap_with(ObjKind::Device, rondos_abi::rights::WRITE);
    let kbd = rondos_rt::cap_with(ObjKind::Device, rondos_abi::rights::READ);
    let mut caps = [CapDesc::default(); 2];
    let mut n = 0;
    if let Some(h) = con {
        caps[n] = CapDesc {
            kind: ObjKind::Device as u32,
            _pad0: 0,
            rights: rondos_abi::rights::WRITE | rondos_abi::rights::SHARE,
            handle: h.0,
        };
        n += 1;
    }
    if let Some(h) = kbd {
        caps[n] = CapDesc {
            kind: ObjKind::Device as u32,
            _pad0: 0,
            rights: rondos_abi::rights::READ | rondos_abi::rights::SHARE,
            handle: h.0,
        };
        n += 1;
    }

    loop {
        let child = match rondos_rt::open_file(root, b"/bin/shell")
            .and_then(|image| {
                let child = rondos_rt::spawn_with_caps(image, &caps[..n])?;
                let _ = rondos_rt::close(image);
                Ok(child)
            }) {
            Ok(child) => child,
            Err(e) => {
                rondos_rt::println!("init: cannot start shell: {:?}", e);
                rondos_rt::sleep_ns(1_000_000_000);
                continue;
            }
        };
        rondos_rt::println!("init: shell started");

        // Supervise: block until it exits, then start it again.
        loop {
            match rondos_rt::wait(&[child], 1_000_000_000) {
                Ok(_) => break,
                Err(rondos_abi::Status::NotReady) => continue,
                Err(e) => {
                    rondos_rt::println!("init: wait failed: {:?}", e);
                    break;
                }
            }
        }
        let _ = rondos_rt::close(child);
        rondos_rt::println!("init: shell exited, restarting");
    }
}
