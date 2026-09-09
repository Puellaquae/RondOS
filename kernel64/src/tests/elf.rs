//! ELF-loader rejection tests.

use crate::exec;
use crate::proc;

use super::{fail, Case, Verdict};

pub static CASES: &[Case] = &[Case {
    name: "elf-reject",
    run: test_elf_reject,
}];

fn pass(ok: bool) -> Verdict {
    if ok {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}

/// A crafted ELF must not be able to name a kernel VA, wrap its segment size,
/// start at data, or ask for W+X.
fn test_elf_reject() -> Verdict {
    // Minimal ELF64 header + one PT_LOAD, patched per case.
    let mut img = [0u8; 64 + 56];
    img[0..4].copy_from_slice(b"\x7fELF");
    img[4] = 2; // ELF64
    img[5] = 1; // little endian
    img[6] = 1; // EV_CURRENT
    img[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    img[18..20].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
    img[24..32].copy_from_slice(&0x40_0000u64.to_le_bytes()); // e_entry
    img[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
    img[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
    img[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum

    let set_ph = |img: &mut [u8], flags: u32, vaddr: u64, filesz: u64, memsz: u64, align: u64| {
        let ph = &mut img[64..120];
        ph[0..4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        ph[4..8].copy_from_slice(&flags.to_le_bytes());
        ph[8..16].copy_from_slice(&0u64.to_le_bytes()); // p_offset
        ph[16..24].copy_from_slice(&vaddr.to_le_bytes());
        ph[32..40].copy_from_slice(&filesz.to_le_bytes());
        ph[40..48].copy_from_slice(&memsz.to_le_bytes());
        ph[48..56].copy_from_slice(&align.to_le_bytes());
    };

    let pid = match proc::table().create(0) {
        Some(p) => p,
        None => { fail("elf-reject", "no process"); return Verdict::Fail }
    };
    let mut ok = true;
    {
        let p = match proc::table().get(pid) {
            Some(p) => p,
            None => { fail("elf-reject", "no process"); return Verdict::Fail }
        };
        // 1. kernel-half vaddr
        set_ph(&mut img, 5, 0xFFFF_FFFF_8020_0000, 0, 0x1000, 0x1000);
        ok &= exec::load_elf(p, &img).is_err();
        // 2. vaddr + memsz wraps past the user half
        set_ph(&mut img, 5, 0x0000_7FFF_FFFF_F000, 0, 0x2000, 0x1000);
        ok &= exec::load_elf(p, &img).is_err();
        // 3. W+X segment
        set_ph(&mut img, 7, 0x40_0000, 0, 0x1000, 0x1000);
        ok &= exec::load_elf(p, &img).is_err();
        // 4. entry outside any executable segment (entry stays 0x400000)
        set_ph(&mut img, 4, 0x80_0000, 0, 0x1000, 0x1000);
        ok &= exec::load_elf(p, &img).is_err();
        // 5. program header table outside the file
        let mut bad = img;
        bad[56..58].copy_from_slice(&200u16.to_le_bytes());
        ok &= exec::load_elf(p, &bad).is_err();
        // 6. a well-formed image still loads
        set_ph(&mut img, 5, 0x40_0000, 0, 0x1000, 0x1000);
        ok &= exec::load_elf(p, &img).is_ok();
    }
    proc::table().reap(pid);
    pass(ok)
}
