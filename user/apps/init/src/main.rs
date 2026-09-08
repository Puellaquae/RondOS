//! `init` — the first user process (P1).
//!
//! For now it proves the whole chain: user workspace → target spec → user.ld →
//! ELF64 loader → ring3 → `int 0x80`.  P1b will make it spawn its children.

#![no_std]
#![no_main]

static mut COUNTER: u64 = 41;

#[no_mangle]
pub extern "C" fn app_main() -> i32 {
    // Touch .data so the image carries a writable, non-executable segment:
    // the loader has to map it RW + NX while .text stays R + X.
    unsafe { COUNTER += 1 };
    rondos_rt::println!("init: hello from ring 3");
    rondos_rt::println!("init: counter {}", unsafe { COUNTER });
    rondos_rt::println!("init: abi {} at t={} ns", rondos_abi::ABI_VERSION, rondos_rt::now_ns());
    0
}
