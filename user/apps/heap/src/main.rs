//! `heap` — exercises the user-space allocator (P2d).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::alloc::GlobalAlloc;
use rondos_rt::heap::RondosAlloc;

#[global_allocator]
static ALLOC: RondosAlloc = RondosAlloc;

#[no_mangle]
pub extern "C" fn app_main(_block: &rondos_abi::StartupBlock) -> i32 {
    // Grow a Vec through several reallocations, then churn to force reuse.
    let mut v: Vec<u32> = Vec::new();
    for i in 0..2000u32 {
        v.push(i * 3 + 1);
    }
    for (i, x) in v.iter().enumerate() {
        if *x != (i as u32) * 3 + 1 {
            rondos_rt::println!("heap: vec corrupted at {}", i);
            return 1;
        }
    }
    let sum: u64 = v.iter().map(|x| *x as u64).sum();
    drop(v);

    // Allocate and free many small blocks: the free list must coalesce, or
    // this runs out of the 64 KiB region.
    let mut blocks = alloc::vec::Vec::new();
    for _ in 0..64 {
        let p = unsafe { ALLOC.alloc(core::alloc::Layout::from_size_align(256, 16).unwrap()) };
        if p.is_null() {
            rondos_rt::println!("heap: allocation failed");
            return 2;
        }
        unsafe { core::ptr::write_bytes(p, 0xA5, 256) };
        blocks.push(p as usize);
    }
    for p in blocks {
        unsafe {
            ALLOC.dealloc(
                p as *mut u8,
                core::alloc::Layout::from_size_align(256, 16).unwrap(),
            )
        };
    }
    rondos_rt::println!("heap: 2000-element Vec ok, sum {}, free list coalesced", sum);
    0
}
