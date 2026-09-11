//! Memory-management test programs.
//!
//! Each `fn` is one kernel-mode test program: it runs to completion and returns
//! its verdict.  The harness (`tests::run_cases`) prints the name, so the bodies
//! contain no reporting code — only the check.

use crate::arch::x86_64::paging::{
    self, create_kernel_address_space, destroy_address_space, phys_to_virt, virt_to_phys,
    X86_64Paging, KERNEL_VIRT_BASE,
};
use crate::arch::x86_64;
use crate::mm::vm::{
    AddressSpace, CachePolicy, PagingArch, PAGE_KERNEL_RW, PAGE_KERNEL_RX, PAGE_USER_RW,
};
use crate::mm;
use crate::obj;
use crate::proc;

use super::{fail, Case, Verdict};
use crate::serial_println;

pub static CASES: &[Case] = &[
    Case { name: "physmap", run: test_physmap },
    Case { name: "huge-split", run: test_huge_split },
    Case { name: "user-va-guard", run: test_user_va_guard },
    Case { name: "addr-space", run: test_address_space },
    Case { name: "w^x", run: test_wx },
    Case { name: "device-map", run: test_device_map },
    Case { name: "allocator", run: test_allocator },
    Case { name: "allocator-bounds", run: test_allocator_bounds },
    Case { name: "memobj-refs", run: test_memobj_refs },
];

fn pass(ok: bool) -> Verdict {
    if ok {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}

fn test_physmap() -> Verdict {
    let root = X86_64Paging::active_root();
    let probes = [0x0usize, 0x1000, 0x200000, 0x1000000, 0x4000_0000];
    let mut ok = true;
    for pa in probes {
        if X86_64Paging::translate(root, phys_to_virt(pa)) != Some(pa) {
            serial_println!("physmap: {:#x} broken", pa);
            ok = false;
        }
    }
    let kva = KERNEL_VIRT_BASE + 0x200000;
    if X86_64Paging::translate(root, kva) != Some(0x200000) {
        serial_println!("physmap: kernel alias {:#x} broken", kva);
        ok = false;
    }
    if let Some(info) = X86_64Paging::query(root, phys_to_virt(0x4000_0000)) {
        if !info.huge {
            serial_println!("physmap: expected a huge mapping at 4 GiB");
            ok = false;
        }
    }
    pass(ok)
}

fn test_huge_split() -> Verdict {
    // Self-contained: build a private root with one 1 GiB page in the user
    // half, then force the 1 GiB -> 2 MiB -> 4 KiB split.  Relying on the
    // bootloader's identity map (as this test used to) is exactly what the
    // kernel-owned address space removed.
    //
    // A CPU without `PDPE1GB` has no 1 GiB page to split -- and *creating* a
    // PS=1 PDPT entry would be a reserved-bit violation the moment it is used,
    // so on such a machine the only correct thing is to skip.  The loader uses
    // 2 MiB pages for the physmap there, and the framebuffer/device mappings
    // already exercise the 2 MiB -> 4 KiB split.
    if !crate::arch::x86_64::has_1g_pages() {
        crate::serial_println!("huge-split: no 1 GiB pages on this CPU, skipped");
        return Verdict::Pass;
    }
    let root = match create_kernel_address_space() {
        Some(r) => r,
        None => return Verdict::Fail,
    };
    let frame = match mm::page_alloc().get_page(1) {
        Some(f) => f,
        None => { fail("huge-split", "no frame"); return Verdict::Fail }
    };
    let pa = virt_to_phys(frame as usize);
    unsafe { (frame as *mut u64).write_volatile(0xDEAD_BEEF_1234_5678) };

    let base = 0x0000_6000_0000_0000usize; // 1 GiB aligned, user half
    let va = base + 0x0100_0000;
    let mut ok = X86_64Paging::map_huge_1g(root, base, 0, PAGE_USER_RW).is_ok();
    ok &= X86_64Paging::map(root, va, pa, PAGE_KERNEL_RW).is_ok();

    match X86_64Paging::query(root, va) {
        Some(info) => {
            if info.pa != pa || info.huge || info.flags.executable || !info.flags.writable {
                serial_println!("huge-split: unexpected flags");
                ok = false;
            }
        }
        None => {
            serial_println!("huge-split: query returned None");
            ok = false;
        }
    }
    // The rest of the split 1 GiB page must still map identity.
    ok &= X86_64Paging::translate(root, va + 0x1000) == Some(0x0100_1000);
    ok &= X86_64Paging::translate(root, va + 0x20_0000) == Some(0x0120_0000);

    let boot_root = X86_64Paging::active_root();
    X86_64Paging::switch_to(root);
    ok &= unsafe { (va as *const u64).read_volatile() } == 0xDEAD_BEEF_1234_5678;
    X86_64Paging::switch_to(boot_root);

    ok &= X86_64Paging::map(root, va, pa + 0x1000, PAGE_KERNEL_RW)
        == Err(mm::vm::MapError::AlreadyMapped);

    ok &= X86_64Paging::unmap(root, va) == Ok(pa);
    mm::page_alloc().free_page(frame, 1);
    destroy_address_space(root);

    pass(ok)
}

/// A user-supplied VA in the kernel half must be refused: the kernel half of a
/// process root is shared with the kernel's own tables, so mapping there would
/// rewrite kernel page tables.
fn test_user_va_guard() -> Verdict {
    let pid = match proc::table().create(0) {
        Some(p) => p,
        None => { fail("user-va-guard", "no process"); return Verdict::Fail }
    };
    let mut ok = true;
    {
        let p = match proc::table().get(pid) {
            Some(p) => p,
            None => { fail("user-va-guard", "no process"); return Verdict::Fail }
        };
        // Kernel image, physmap and the very top of the user half + 1 page.
        for va in [
            0xFFFF_FFFF_8020_0000u64,
            0xFFFF_8000_0000_0000,
            paging::USER_VA_LIMIT as u64,
            paging::USER_VA_LIMIT as u64 - 0x1000 + 1,
        ] {
            if p.map_anon(va, 0x1000, PAGE_USER_RW).is_ok() {
                serial_println!("user-va-guard: mapped {:#x}", va);
                ok = false;
            }
        }
        // A legal user VA still works.
        ok &= p.map_anon(0x0000_0000_5000_0000, 0x1000, PAGE_USER_RW).is_ok();
    }
    proc::table().reap(pid);
    pass(ok)
}

fn test_address_space() -> Verdict {
    let boot_root = X86_64Paging::active_root();
    let root = match create_kernel_address_space() {
        Some(r) => r,
        None => { fail("addr-space", "no PML4"); return Verdict::Fail }
    };
    let frame = match mm::page_alloc().get_page(1) {
        Some(f) => f,
        None => { fail("addr-space", "no frame"); return Verdict::Fail }
    };
    let pa = virt_to_phys(frame as usize);

    let user_va = 0x0000_4000_0000_0000usize;
    let mut space = AddressSpace::<X86_64Paging>::new(root);
    let mut ok = space.map(user_va, pa, PAGE_USER_RW).is_ok();
    ok &= space.translate(user_va) == Some(pa);
    ok &= X86_64Paging::translate(root, 0x0100_0000).is_none();

    let rsp = x86_64::read_rsp();
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
    ok &= X86_64Paging::translate(boot_root, user_va).is_none();

    mm::page_alloc().free_page(frame, 1);
    destroy_address_space(root);

    pass(ok)
}

fn test_wx() -> Verdict {
    let root = X86_64Paging::active_root();
    let frame = match mm::page_alloc().get_page(1) {
        Some(f) => f,
        None => { fail("w^x", "no frame"); return Verdict::Fail }
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
    pass(ok)
}

fn test_device_map() -> Verdict {
    let root = X86_64Paging::active_root();
    let va = 0x5200_0000usize;
    let pa = 0xE000_0000usize;
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
    pass(ok)
}

fn test_allocator() -> Verdict {
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
    pass(ok)
}

/// The frame allocator must return `None` (not panic) for impossible requests.
fn test_allocator_bounds() -> Verdict {
    let mut ok = true;
    ok &= mm::page_alloc().get_page(usize::MAX / 4096).is_none();
    ok &= mm::page_alloc().get_page(usize::MAX).is_none();
    // A normal single-page round trip still works afterwards.
    let before = mm::page_alloc().free_pages();
    match mm::page_alloc().get_page(1) {
        Some(p) => {
            mm::page_alloc().free_page(p, 1);
            ok &= mm::page_alloc().free_pages() == before;
        }
        None => ok = false,
    }
    pass(ok)
}

/// Closing a memory handle must unmap it; the frames survive until the last
/// reference (mapping or handle) is gone.
fn test_memobj_refs() -> Verdict {
    let pid = match proc::table().create(0) {
        Some(p) => p,
        None => { fail("memobj-refs", "no process"); return Verdict::Fail }
    };
    let mut ok = true;
    {
        let p = match proc::table().get(pid) {
            Some(p) => p,
            None => { fail("memobj-refs", "no process"); return Verdict::Fail }
        };
        let id = match obj::mem().create(0x1000, 0) {
            Some(id) => id,
            None => { fail("memobj-refs", "no object"); return Verdict::Fail }
        };
        // The mapping takes a reference of its own (page tables also cost
        // frames, so only deltas *after* the mapping are asserted).
        let va = match p.map_memobj(id, rondos_abi::rights::READ | rondos_abi::rights::WRITE, 0) {
            Ok(va) => va,
            Err(e) => {
                serial_println!("memobj-refs: map failed {:?}", e);
                proc::table().reap(pid);
                { fail("memobj-refs", "map"); return Verdict::Fail }
            }
        };
        ok &= X86_64Paging::translate(p.root(), va as usize).is_some();
        let free_mapped = mm::page_alloc().free_pages();

        // Drop the creator's reference: the mapping still holds one, so the
        // frame must stay alive and mapped.
        obj::mem().release(id);
        ok &= X86_64Paging::translate(p.root(), va as usize).is_some();
        ok &= mm::page_alloc().free_pages() == free_mapped;

        // Unmapping drops the last reference and returns exactly that frame.
        let _ = p.unmap_memobj(va);
        ok &= X86_64Paging::translate(p.root(), va as usize).is_none();
        ok &= mm::page_alloc().free_pages() == free_mapped + 1;
    }
    proc::table().reap(pid);
    pass(ok)
}
