//! `crash` — a process that faults on purpose, to prove containment.

#![no_std]
#![no_main]

#[no_mangle]
pub extern "C" fn app_main(_block: &rondos_abi::StartupBlock) -> i32 {
    rondos_rt::println!("crash: about to touch unmapped memory");
    unsafe { core::ptr::write_volatile(0x0000_0000_dead_beef as *mut u8, 1) };
    rondos_rt::println!("crash: NOT REACHED");
    1
}
