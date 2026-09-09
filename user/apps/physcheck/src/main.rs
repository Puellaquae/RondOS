//! `physcheck` — must be denied physical-memory mapping (P2 capability test).

#![no_std]
#![no_main]

use rondos_abi::Status;

#[no_mangle]
pub extern "C" fn app_main(_block: &rondos_abi::StartupBlock) -> i32 {
    match rondos_rt::mem_map_phys(0, 4096, 0) {
        Err(Status::Permission) => {
            rondos_rt::println!("physcheck: mem_map_phys denied as expected");
            0
        }
        Err(e) => {
            rondos_rt::println!("physcheck: unexpected error {:?}", e);
            2
        }
        Ok(_) => {
            rondos_rt::println!("physcheck: MAPPED PHYSICAL MEMORY WITHOUT A CAPABILITY");
            1
        }
    }
}
