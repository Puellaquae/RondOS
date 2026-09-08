//! ELF64 loading and the boot tar image — P1.
//!
//! The kernel no longer carries user code: the UEFI stub loads `\rondos\boot.tar`
//! into memory (already reported as `BootInfo.initrd_*`) and the kernel finds
//! `/bin/*` inside it.  Programs are ordinary ELF64 `ET_EXEC` images built by
//! the `user/` workspace for the `x86_64-rondos` target: fixed-address, page
//! aligned `PT_LOAD`s, `.text` R+X and data RW+NX.
//!
//! The loader is deliberately strict and small:
//!
//! * only `ET_EXEC`/`EM_X86_64`, no interpreter, no relocations, no PIE;
//! * every `PT_LOAD` must be page aligned in `p_vaddr` (`user.ld` guarantees
//!   it) and is mapped with the segment's own permissions — W^X comes from the
//!   ELF flags, not from a policy table;
//! * `p_filesz` bytes are copied, `[p_filesz, p_memsz)` is zeroed;
//! * a fixed user stack is mapped above the image (the real one grows on
//!   demand in P5).

#![allow(dead_code)]

use crate::mm::vm::{PageFlags, PAGE_USER_RW, PAGE_USER_RX};
use crate::mm::PAGE_SIZE;
use crate::proc::Process;
use rondos_abi::Status;

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;

/// A loaded image: where to start and where its last byte lives.
#[derive(Debug, Clone, Copy)]
pub struct Image {
    pub entry: u64,
    pub end: u64,
    pub segments: u32,
}

fn u16_at(buf: &[u8], off: usize) -> Result<u16, Status> {
    buf.get(off..off + 2)
        .map(|s| u16::from_le_bytes(s.try_into().unwrap()))
        .ok_or(Status::InvalidArgument)
}

fn u32_at(buf: &[u8], off: usize) -> Result<u32, Status> {
    buf.get(off..off + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
        .ok_or(Status::InvalidArgument)
}

fn u64_at(buf: &[u8], off: usize) -> Result<u64, Status> {
    buf.get(off..off + 8)
        .map(|s| u64::from_le_bytes(s.try_into().unwrap()))
        .ok_or(Status::InvalidArgument)
}

/// Load `elf` into `p`'s address space.  On failure the caller tears the
/// process down (its pages are tracked by VMAs either way).
pub fn load_elf(p: &mut Process, elf: &[u8]) -> Result<Image, Status> {
    if elf.len() < 64 || elf[0..4] != ELF_MAGIC || elf[4] != 2 || elf[5] != 1 {
        return Err(Status::InvalidArgument); // ELF64, little endian
    }
    if u16_at(elf, 16)? != ET_EXEC || u16_at(elf, 18)? != EM_X86_64 {
        return Err(Status::Unsupported);
    }
    let entry = u64_at(elf, 24)?;
    let phoff = u64_at(elf, 32)? as usize;
    let phentsize = u16_at(elf, 54)? as usize;
    let phnum = u16_at(elf, 56)? as usize;
    if phentsize < 56 {
        return Err(Status::InvalidArgument);
    }

    let mut end = 0u64;
    let mut segments = 0u32;
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        if u32_at(elf, ph)? != PT_LOAD {
            continue;
        }
        let flags = u32_at(elf, ph + 4)?;
        let offset = u64_at(elf, ph + 8)? as usize;
        let vaddr = u64_at(elf, ph + 16)?;
        let filesz = u64_at(elf, ph + 32)? as usize;
        let memsz = u64_at(elf, ph + 40)? as usize;

        if vaddr % PAGE_SIZE as u64 != 0 || memsz == 0 {
            return Err(Status::InvalidArgument);
        }
        if offset.checked_add(filesz).map_or(true, |e| e > elf.len()) || filesz > memsz {
            return Err(Status::InvalidArgument);
        }

        let map_flags = if (flags & PF_X) != 0 {
            if (flags & PF_W) != 0 {
                return Err(Status::InvalidArgument); // no W+X
            }
            PAGE_USER_RX
        } else {
            PageFlags {
                writable: true,
                ..PAGE_USER_RW
            }
        };
        let len = (memsz as u64 + PAGE_SIZE as u64 - 1) & !(PAGE_SIZE as u64 - 1);
        p.map_anon(vaddr, len, map_flags)?;
        if filesz > 0 {
            p.write_user(vaddr, &elf[offset..offset + filesz])?;
        }
        end = end.max(vaddr + memsz as u64);
        segments += 1;
    }

    if segments == 0 || entry == 0 {
        return Err(Status::InvalidArgument);
    }
    Ok(Image {
        entry,
        end,
        segments,
    })
}

/// Map a fixed user stack above the image and return its top.
pub fn map_stack(p: &mut Process, image: &Image) -> Result<u64, Status> {
    const STACK_PAGES: u64 = 2;
    let base = (image.end + 0xffff) & !0xffff; // 64 KiB above the image
    let len = STACK_PAGES * PAGE_SIZE as u64;
    p.map_anon(base, len, PAGE_USER_RW)?;
    Ok(base + len - 16)
}

/// Load `elf` into a fresh process and start its first thread.
pub fn spawn(name: &'static str, elf: &[u8]) -> Result<u32, Status> {
    let pid = crate::proc::table().create(0).ok_or(Status::OutOfMemory)?;

    // Everything that borrows the process happens in one scope.
    let prepared = {
        let Some(p) = crate::proc::table().get(pid) else {
            return Err(Status::NotFound);
        };
        load_elf(p, elf).and_then(|image| map_stack(p, &image).map(|top| (p.root(), image, top)))
    };
    let (root, image, stack_top) = match prepared {
        Ok(v) => v,
        Err(e) => {
            crate::proc::table().reap(pid);
            return Err(e);
        }
    };

    let tid = match crate::thread::thread_create_user(
        pid,
        root,
        name,
        image.entry,
        stack_top,
        0,
    ) {
        Some(t) => t,
        None => {
            crate::proc::table().reap(pid);
            return Err(Status::OutOfMemory);
        }
    };
    if crate::proc::table().attach_thread(pid, tid).is_err() {
        crate::proc::table().reap(pid);
        return Err(Status::NotReady);
    }

    crate::serial_println!(
        "exec: '{}' pid {} tid {} entry {:#x} {} segment(s) end {:#x} stack {:#x}",
        name,
        pid,
        tid,
        image.entry,
        image.segments,
        image.end,
        stack_top
    );
    Ok(pid)
}

// ------------------------------------------------------------------ tarfs

const TAR_BLOCK: usize = 512;

fn tar_octal(field: &[u8]) -> Option<usize> {
    let mut v = 0usize;
    let mut seen = false;
    for b in field {
        match b {
            b'0'..=b'7' => {
                v = v.checked_mul(8)?.checked_add((b - b'0') as usize)?;
                seen = true;
            }
            0 | b' ' if !seen => {}
            0 | b' ' => break,
            _ => return None,
        }
    }
    Some(v)
}

fn tar_name(field: &[u8]) -> &[u8] {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    &field[..end]
}

/// Find `name` in an (uncompressed, ustar) tar image.  Only regular files.
pub fn tar_find<'a>(tar: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let mut off = 0usize;
    while off + TAR_BLOCK <= tar.len() {
        let hdr = &tar[off..off + TAR_BLOCK];
        if hdr.iter().all(|b| *b == 0) {
            return None; // end-of-archive marker
        }
        let size = tar_octal(&hdr[124..136])?;
        let typeflag = hdr[156];
        let data = off + TAR_BLOCK;
        let data_end = data.checked_add(size)?;
        if data_end > tar.len() {
            return None;
        }
        if (typeflag == b'0' || typeflag == 0) && tar_name(&hdr[0..100]) == name {
            return Some(&tar[data..data_end]);
        }
        off = data + (size + TAR_BLOCK - 1) / TAR_BLOCK * TAR_BLOCK;
    }
    None
}

/// The boot tar as handed over by the UEFI stub, if present.
pub fn boot_tar() -> Option<&'static [u8]> {
    let bi = crate::bootinfo::get();
    if bi.initrd_phys == 0 || bi.initrd_len == 0 {
        return None;
    }
    let va = crate::arch::x86_64::paging::phys_to_virt(bi.initrd_phys as usize);
    Some(unsafe { core::slice::from_raw_parts(va as *const u8, bi.initrd_len as usize) })
}
