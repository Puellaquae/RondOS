//! `init` — PID 1.
//!
//! The kernel starts this after its boot self-checks (`make run`).  It holds
//! the root directory and device capabilities and will spawn the display
//! server, shell and progman once those exist; today it just announces itself
//! and stays alive, because PID 1 must never exit.

#![no_std]
#![no_main]

use rondos_abi::StartupBlock;

#[no_mangle]
pub extern "C" fn app_main(block: &StartupBlock) -> i32 {
    rondos_rt::println!(
        "init: pid 1 up (abi {}, {} capabilities)",
        block.abi_version,
        block.caps().len()
    );
    if let Some(root) = rondos_rt::root_dir() {
        rondos_rt::println!("init: root directory capability {:#x}", root.0);
    }

    // PID 1 idles until there is something to supervise.
    loop {
        rondos_rt::sleep_ns(1_000_000_000);
    }
}
