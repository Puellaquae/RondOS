//! `spin` — a long-running child, killed by `init` to test `sys_kill`.

#![no_std]
#![no_main]

#[no_mangle]
pub extern "C" fn app_main(_block: &rondos_abi::StartupBlock) -> i32 {
    let mut n = 0u64;
    loop {
        // Keep clobbering the FPU state: if the kernel did not save/restore it
        // per thread, `init`'s marker would come back as this one.
        rondos_rt::set_xmm0(0xBAD0_BAD0_DEAD_DEAD);
        n += 1;
        if n % 500_000 == 0 {
            rondos_rt::yield_now();
        }
    }
}
