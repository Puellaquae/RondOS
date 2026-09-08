//! RondOS x86-64 kernel — M0.1 / M0.2 scaffold.
//!
//! This is the migration target described in `docs/user-mode-design.md` §7:
//! the arch layer (M0.1) and the 4-level paging backend (M0.2), booted through
//! a temporary multiboot trampoline so both can be verified in QEMU before the
//! UEFI stub (M0.7) exists.
//!
//! `_start` is entered in long mode by `boot.rs` with `rdi` = the physical
//! address of the multiboot info block.
//!
//! Smoke tests below verify exactly the properties the design depends on:
//! physmap translation, huge-page splitting, per-address-space isolation, NX
//! (W^X) and device mappings with a cache policy.

#![no_std]
#![no_main]

mod arch;
mod io;
mod mm;
mod multiboot;
mod utils;

use arch::x86_64::paging::{
    create_kernel_address_space, destroy_address_space, phys_to_virt, virt_to_phys, X86_64Paging,
    KERNEL_VIRT_BASE, PHYS_MAP_BASE,
};
use arch::x86_64::{cpuid, halt_loop, has_nx};
use mm::vm::{AddressSpace, CachePolicy, PagingArch, PAGE_KERNEL_RW, PAGE_KERNEL_RX, PAGE_USER_RW};

#[no_mangle]
pub extern "C" fn _start(mbi_phys: u32) -> ! {
    serial_println!();
    serial_println!("==============================================");
    serial_println!("RondOS x86-64 — M0.1/M0.2 scaffold");
    serial_println!("==============================================");

    banner_cpu();

    multiboot::parse(mbi_phys);
    if let Some(name) = multiboot::boot_loader(mbi_phys) {
        serial_println!("bootloader: {}", name);
    }
    serial_println!(
        "memory: {} MiB usable, top {:#x}",
        mm::available_mem_size() / 1024 / 1024,
        mm::usable_end()
    );

    serial_println!("cr3 {:#x}", arch::x86_64::cr3());
    serial_println!("physmap base {:#x}", PHYS_MAP_BASE);
    serial_println!("kernel base  {:#x}", KERNEL_VIRT_BASE);

    let mut failures = 0usize;
    failures += !test_physmap() as usize;
    failures += !test_huge_split() as usize;
    failures += !test_address_space() as usize;
    failures += !test_wx() as usize;
    failures += !test_device_map() as usize;
    failures += !test_allocator() as usize;

    serial_println!("----------------------------------------------");
    if failures == 0 {
        serial_println!("smoke: ALL PASS (6/6)");
    } else {
        serial_println!("smoke: {} FAILURE(S)", failures);
    }

    halt_loop()
}

fn banner_cpu() {
    let v = cpuid(0, 0);
    let mut vendor = [0u8; 12];
    vendor[0..4].copy_from_slice(&v.ebx.to_le_bytes());
    vendor[4..8].copy_from_slice(&v.edx.to_le_bytes());
    vendor[8..12].copy_from_slice(&v.ecx.to_le_bytes());
    let vendor = core::str::from_utf8(&vendor).unwrap_or("?");
    let f = cpuid(1, 0);
    let ext = cpuid(0x8000_0001, 0);
    serial_println!(
        "cpu: {} family {} model {} stepping {}",
        vendor,
        (f.eax >> 8) & 0xf,
        (f.eax >> 4) & 0xf,
        f.eax & 0xf
    );
    serial_println!(
        "features: NX {} | ext.edx {:#010x} | rsp {:#x} | rflags {:#x}",
        if has_nx() { "yes" } else { "NO" },
        ext.edx,
        arch::x86_64::read_rsp(),
        arch::x86_64::read_rflags()
    );
}

/// The physmap must translate `PHYS_MAP_BASE + pa -> pa` for the whole mapped
/// window, and the kernel's linear alias must agree.
fn test_physmap() -> bool {
    let root = X86_64Paging::active_root();
    let probes = [0x0usize, 0x1000, 0x200000, 0x1000000, 0x4000_0000];
    let mut ok = true;
    for pa in probes {
        let va = phys_to_virt(pa);
        let got = X86_64Paging::translate(root, va);
        if got != Some(pa) {
            serial_println!("physmap: {:#x} -> {:?}, want {:#x}", va, got, pa);
            ok = false;
        }
    }
    // kernel image linear alias (VA = KERNEL_VIRT_BASE + PA)
    let kva = KERNEL_VIRT_BASE + 0x200000;
    if X86_64Paging::translate(root, kva) != Some(0x200000) {
        serial_println!("physmap: kernel alias {:#x} broken", kva);
        ok = false;
    }
    // 1 GiB huge page should be reported as huge
    if let Some(info) = X86_64Paging::query(root, phys_to_virt(0x4000_0000)) {
        if !info.huge {
            serial_println!("physmap: expected a huge mapping at 4 GiB");
            ok = false;
        }
    }
    report("physmap", ok);
    ok
}

/// Mapping a 4 KiB page inside the identity 1 GiB page forces
/// split_1g -> split_2m, and must leave the rest of the region intact.
fn test_huge_split() -> bool {
    let root = X86_64Paging::active_root();
    let frame = match mm::page_alloc().get_page(1) {
        Some(f) => f,
        None => return fail("huge-split", "no frame"),
    };
    let pa = virt_to_phys(frame as usize);
    unsafe { (frame as *mut u64).write_volatile(0xDEAD_BEEF_1234_5678) };

    let va = 0x0100_0000usize; // inside the identity 1 GiB page
    let mut ok = X86_64Paging::map(root, va, pa, PAGE_KERNEL_RW).is_ok();

    match X86_64Paging::query(root, va) {
        Some(info) => {
            if info.pa != pa || info.huge || info.flags.executable || !info.flags.writable {
                serial_println!(
                    "huge-split: pa {:#x} want {:#x} huge {} exec {} writable {}",
                    info.pa, pa, info.huge, info.flags.executable, info.flags.writable
                );
                ok = false;
            }
        }
        None => {
            serial_println!("huge-split: query returned None");
            ok = false;
        }
    }
    // neighbours must still be identity mapped after both splits
    if X86_64Paging::translate(root, va + 0x1000) != Some(va + 0x1000) {
        serial_println!("huge-split: neighbour {:#x} not identity", va + 0x1000);
        ok = false;
    }
    if X86_64Paging::translate(root, va + 0x20_0000) != Some(va + 0x20_0000) {
        serial_println!("huge-split: 2MiB neighbour {:#x} not identity", va + 0x20_0000);
        ok = false;
    }
    // and the mapping must actually be usable
    ok &= unsafe { (va as *const u64).read_volatile() } == 0xDEAD_BEEF_1234_5678;
    // re-mapping to a different frame must be rejected
    ok &= X86_64Paging::map(root, va, pa + 0x1000, PAGE_KERNEL_RW)
        == Err(mm::vm::MapError::AlreadyMapped);

    let back = X86_64Paging::unmap(root, va);
    ok &= back == Ok(pa);
    mm::page_alloc().free_page(frame, 1);

    report("huge-split", ok);
    ok
}

/// A fresh address space must not see the boot identity map, must map user
/// pages, and must survive an actual CR3 switch (stack included).
fn test_address_space() -> bool {
    let boot_root = X86_64Paging::active_root();
    let root = match create_kernel_address_space() {
        Some(r) => r,
        None => return fail("addr-space", "no PML4"),
    };
    let frame = match mm::page_alloc().get_page(1) {
        Some(f) => f,
        None => return fail("addr-space", "no frame"),
    };
    let pa = virt_to_phys(frame as usize);

    // 64 TiB: canonical, inside the user half, far away from the identity map
    let user_va = 0x0000_4000_0000_0000usize;
    let mut space = AddressSpace::<X86_64Paging>::new(root);
    let mut ok = space.map(user_va, pa, PAGE_USER_RW).is_ok();
    ok &= space.translate(user_va) == Some(pa);
    // the boot identity map must NOT have been copied into the user half
    ok &= X86_64Paging::translate(root, 0x0100_0000).is_none();

    // Give the test space a stack: map the pages currently holding RSP.
    let rsp = arch::x86_64::read_rsp();
    let stack_lo = (rsp & !0xfff) - 0x1000;
    for i in 0..3 {
        let va = stack_lo + i * 0x1000;
        match X86_64Paging::translate(boot_root, va) {
            Some(pa_stack) => {
                ok &= space.map(va, pa_stack, PAGE_KERNEL_RW).is_ok();
            }
            None => ok = false,
        }
    }

    space.activate();
    unsafe { (user_va as *mut u32).write_volatile(0xCAFE_F00D) };
    X86_64Paging::switch_to(boot_root);

    ok &= unsafe { (frame as *const u32).read_volatile() } == 0xCAFE_F00D;
    // the boot space must not have gained the user mapping
    ok &= X86_64Paging::translate(boot_root, user_va).is_none();

    mm::page_alloc().free_page(frame, 1);
    destroy_address_space(root);

    report("addr-space", ok);
    ok
}

/// W^X: `executable: false` must set NX, `executable: true` must clear it.
fn test_wx() -> bool {
    let root = X86_64Paging::active_root();
    let frame = match mm::page_alloc().get_page(1) {
        Some(f) => f,
        None => return fail("w^x", "no frame"),
    };
    let pa = virt_to_phys(frame as usize);
    let va = 0x5100_0000usize;
    let mut ok = true;

    if X86_64Paging::map(root, va, pa, PAGE_KERNEL_RW).is_ok() {
        if let Some(i) = X86_64Paging::query(root, va) {
            ok &= !i.flags.executable && i.flags.writable && !i.flags.user;
        } else {
            ok = false;
        }
        ok &= X86_64Paging::unmap(root, va).is_ok();
    } else {
        ok = false;
    }

    if X86_64Paging::map(root, va, pa, PAGE_KERNEL_RX).is_ok() {
        if let Some(i) = X86_64Paging::query(root, va) {
            ok &= i.flags.executable && !i.flags.writable;
        } else {
            ok = false;
        }
        ok &= X86_64Paging::unmap(root, va).is_ok();
    } else {
        ok = false;
    }

    if X86_64Paging::map(root, va, pa, PAGE_USER_RW).is_ok() {
        if let Some(i) = X86_64Paging::query(root, va) {
            ok &= i.flags.user && !i.flags.executable;
        } else {
            ok = false;
        }
        ok &= X86_64Paging::unmap(root, va).is_ok();
    } else {
        ok = false;
    }

    mm::page_alloc().free_page(frame, 1);
    report("w^x", ok);
    ok
}

/// Device mapping with a cache policy — the GOP framebuffer path (§8.2).
fn test_device_map() -> bool {
    let root = X86_64Paging::active_root();
    let va = 0x5200_0000usize;
    let pa = 0xE000_0000usize; // pretend MMIO
    let mut ok =
        X86_64Paging::map_device(root, va, pa, 0x3000, CachePolicy::WriteCombining).is_ok();
    if let Some(i) = X86_64Paging::query(root, va) {
        ok &= i.pa == pa && i.flags.cache == CachePolicy::WriteCombining && !i.flags.executable;
    } else {
        ok = false;
    }
    if let Some(i) = X86_64Paging::query(root, va + 0x2000) {
        ok &= i.pa == pa + 0x2000;
    } else {
        ok = false;
    }
    for i in 0..3 {
        ok &= X86_64Paging::unmap(root, va + i * 0x1000).is_ok();
    }
    report("device-map", ok);
    ok
}

/// Frame allocator: allocate / write / read / free a run of pages.
fn test_allocator() -> bool {
    let mut ok = true;
    let a = mm::page_alloc().get_page(4);
    let b = mm::page_alloc().get_page(1);
    match (a, b) {
        (Some(a), Some(b)) => {
            ok &= (a as usize) % 4096 == 0 && (b as usize) % 4096 == 0;
            unsafe {
                for i in 0..4 * 4096 {
                    *a.add(i) = (i % 251) as u8;
                }
                let mut good = true;
                for i in 0..4 * 4096 {
                    if *a.add(i) != (i % 251) as u8 {
                        good = false;
                        break;
                    }
                }
                ok &= good;
            }
            mm::page_alloc().free_page(a, 4);
            mm::page_alloc().free_page(b, 1);
        }
        _ => ok = false,
    }
    serial_println!("allocator: {} KiB free", mm::page_alloc().free_pages() * 4);
    report("allocator", ok);
    ok
}

fn report(name: &str, ok: bool) {
    serial_println!("[{}] {}", if ok { " ok " } else { "FAIL" }, name);
}

fn fail(name: &str, why: &str) -> bool {
    serial_println!("[FAIL] {}: {}", name, why);
    false
}

#[panic_handler]
pub fn panic(info: &core::panic::PanicInfo) -> ! {
    serial_println!("PANIC: {}", info);
    halt_loop()
}
