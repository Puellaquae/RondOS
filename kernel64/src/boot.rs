//! Boot self-checks and the normal (non-test) boot path.
//!
//! `make run` boots through here: a handful of *necessary* invariants are
//! verified, the scheduler is started, `/bin/init` is spawned and the boot
//! thread idles.  The full test suite lives in [`crate::tests`] and is compiled
//! in only with the `kernel-tests` feature (`make test`).

use crate::arch::x86_64::paging::{phys_to_virt, X86_64Paging, KERNEL_VIRT_BASE};
use crate::arch::x86_64::{pic, sti};
use crate::mm;
use crate::mm::vm::PagingArch;
use crate::thread;

/// PIT frequency: one tick every 5 ms.
pub const TIMER_HZ: u32 = 200;

/// The minimum the rest of the kernel assumes.  A failure here is fatal, so
/// panic with a precise message instead of limping on with a broken mapping.
pub fn self_check() {
    let root = X86_64Paging::active_root();

    // The physmap window must translate every page back to itself; everything
    // in `mm` walks memory through it.
    for pa in [0usize, 0x1000, 0x20_0000, 0x4000_0000] {
        assert_eq!(
            X86_64Paging::translate(root, phys_to_virt(pa)),
            Some(pa),
            "self-check: physmap broken at {:#x}",
            pa
        );
    }

    // The kernel image must be reachable through the kernel window.
    assert_eq!(
        X86_64Paging::translate(root, KERNEL_VIRT_BASE + 0x20_0000),
        Some(0x20_0000),
        "self-check: kernel window broken"
    );

    // The frame allocator must hand out and take back a frame.
    let before = mm::page_alloc().free_pages();
    let frame = mm::page_alloc()
        .get_page(1)
        .expect("self-check: page allocator is empty");
    unsafe { core::ptr::write_bytes(frame, 0xa5, mm::PAGE_SIZE) };
    mm::page_alloc().free_page(frame, 1);
    assert_eq!(
        mm::page_alloc().free_pages(),
        before,
        "self-check: frame leaked"
    );

    crate::serial_println!("self-check: physmap, kernel window, allocator ok");
}

/// Start the timer and the scheduler.  GDT/IDT must already be loaded.
pub fn bring_up_scheduler() {
    pic::init();
    pic::configure_pit(0, 2, TIMER_HZ);
    thread::init();
    sti();
}

/// Normal boot: scheduler up, `/bin/init` running, boot thread idle.
pub fn normal_boot() -> ! {
    bring_up_scheduler();
    match crate::exec::spawn_path(b"/bin/init") {
        Ok(pid) => crate::serial_println!("boot: /bin/init is pid {}", pid),
        Err(e) => crate::serial_println!("boot: cannot start /bin/init: {:?}", e),
    }
    loop {
        crate::arch::x86_64::hlt();
    }
}
