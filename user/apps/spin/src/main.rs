//! `spin` — a long-running child, killed by `init` to test `sys_kill`.

#![no_std]
#![no_main]

#[no_mangle]
pub extern "C" fn app_main(_block: &rondos_abi::StartupBlock) -> i32 {
    let mut n = 0u64;
    loop {
        n += 1;
        if n % 500_000 == 0 {
            rondos_rt::yield_now();
        }
    }
}
