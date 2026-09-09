//! ELF64 loading and process creation — P1.
//!
//! The kernel no longer carries user code: the UEFI stub loads `\rondos\boot.tar`
//! into memory (already reported as `BootInfo.initrd_*`), [`crate::fs::TarFs`]
//! finds `/bin/*` inside it, and this module turns an ELF64 image into a
//! running ring3 process.
//!
//! The loader is deliberately strict and small:
//!
//! * only `ET_EXEC`/`EM_X86_64`, no interpreter, no relocations, no PIE;
//! * every `PT_LOAD` must be page aligned in `p_vaddr` (`user.ld` guarantees
//!   it) and is mapped with the segment's own permissions — W^X comes from the
//!   ELF flags, not from a policy table;
//! * `p_filesz` bytes are copied, `[p_filesz, p_memsz)` is zeroed;
//! * a fixed user stack is mapped above the image, with the [`StartupBlock`]
//!   and the granted capabilities written at its top (design §5.3/§5.4).

#![allow(dead_code)]

use core::mem::size_of;

use crate::fs::{Entry, TarFs};
use crate::arch::x86_64::paging::valid_user_range;
use crate::mm::vm::{PageFlags, PAGE_USER_RW, PAGE_USER_RX};
use crate::mm::PAGE_SIZE;
use crate::proc::{ObjRef, Process};
use rondos_abi::{
    rights, CapDesc, ExitStatus, Handle, ObjKind, Slice, StartupBlock, Status, StructHeader,
    ABI_VERSION,
};

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;

/// Capabilities a parent can delegate in one `sys_spawn`.
pub const MAX_EXTRA_CAPS: usize = 4;

/// A capability to install in a new process, already resolved and rights-checked
/// by the caller (`sys_spawn`).
#[derive(Clone, Copy, Debug)]
pub struct PendingCap {
    pub kind: ObjKind,
    pub id: u32,
    /// Object length (memory objects) / channel end.
    pub len: u64,
    /// Object flags (memory objects) / 0.
    pub flags: u64,
    pub rights: u64,
}

impl PendingCap {
    pub const EMPTY: PendingCap = PendingCap {
        kind: ObjKind::None,
        id: 0,
        len: 0,
        flags: 0,
        rights: 0,
    };
}

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

fn as_bytes<T>(v: &T) -> &[u8] {
    unsafe { core::slice::from_raw_parts(v as *const T as *const u8, size_of::<T>()) }
}

fn bytes_of<T>(v: &[T]) -> &[u8] {
    unsafe { core::slice::from_raw_parts(v.as_ptr() as *const u8, size_of_val(v)) }
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
    if phentsize < 56 || phnum == 0 || phnum > 64 {
        return Err(Status::InvalidArgument);
    }
    // Program header table must lie inside the file (checked arithmetic: a
    // crafted e_phoff/e_phnum must not wrap into reading arbitrary memory).
    let table_end = phoff
        .checked_add(phnum.checked_mul(phentsize).ok_or(Status::InvalidArgument)?)
        .ok_or(Status::InvalidArgument)?;
    if table_end > elf.len() {
        return Err(Status::InvalidArgument);
    }

    let mut end = 0u64;
    let mut segments = 0u32;
    let mut entry_in_exec = false;
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        if u32_at(elf, ph)? != PT_LOAD {
            continue;
        }
        let flags = u32_at(elf, ph + 4)?;
        let offset = u64_at(elf, ph + 8)? as usize;
        let vaddr = u64_at(elf, ph + 16)?;
        let align = u64_at(elf, ph + 48)?;
        let filesz = u64_at(elf, ph + 32)? as usize;
        let memsz = u64_at(elf, ph + 40)? as usize;

        // `ld` emits an empty RW PT_LOAD (vaddr 0, memsz 0) when a program has
        // no .data/.bss at all — nothing to map, skip it.
        if memsz == 0 {
            continue;
        }
        if vaddr % PAGE_SIZE as u64 != 0 || (align != 0 && align < PAGE_SIZE as u64) {
            return Err(Status::InvalidArgument);
        }
        // The security boundary: a user process's kernel half is *shared* with
        // the kernel's own page tables, so an ELF must never name a VA there.
        if !valid_user_range(vaddr as usize, memsz) {
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
            // Honour PF_W: read-only data stays read-only (doc §5.1).
            PageFlags {
                writable: (flags & PF_W) != 0,
                ..PAGE_USER_RW
            }
        };
        let len = (memsz as u64 + PAGE_SIZE as u64 - 1) & !(PAGE_SIZE as u64 - 1);
        p.map_anon(vaddr, len, map_flags)?;
        if filesz > 0 {
            p.write_user(vaddr, &elf[offset..offset + filesz])?;
        }
        if (flags & PF_X) != 0 && entry >= vaddr && entry < vaddr + memsz as u64 {
            entry_in_exec = true;
        }
        end = end.max(vaddr + memsz as u64);
        segments += 1;
    }

    // The entry point must be inside an executable segment: jumping anywhere
    // else would let a crafted image start execution at data.
    if segments == 0 || entry == 0 || !entry_in_exec {
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
    if !valid_user_range(base as usize, len as usize) {
        return Err(Status::InvalidArgument);
    }
    p.map_anon(base, len, PAGE_USER_RW)?;
    Ok(base + len)
}

/// Write the `StartupBlock` + granted capabilities at the top of the user
/// stack and return the initial `rsp` (just below them).
///
/// The block carries the process's capabilities, which is how `init` learns
/// the root directory handle — there is no global namespace to look it up in.
fn write_startup(
    p: &mut Process,
    image: &Image,
    stack_top: u64,
    extra: &[PendingCap],
) -> Result<(u64, u64), Status> {
    // The root directory capability is implicit; everything else is delegated
    // by the parent and installed here.
    let mut caps = [CapDesc::default(); 1 + MAX_EXTRA_CAPS];
    let root_dir = p
        .handles_mut()
        .insert(ObjRef::Dir { node: 0 }, rights::ALL)
        .ok_or(Status::OutOfMemory)?;
    caps[0] = CapDesc {
        kind: ObjKind::Dir as u32,
        _pad0: 0,
        rights: rights::ALL,
        handle: root_dir.0,
    };
    let mut n_caps = 1usize;

    for c in extra.iter().take(MAX_EXTRA_CAPS) {
        let obj = match c.kind {
            ObjKind::Memory => {
                if !crate::obj::mem().retain(c.id) {
                    return Err(Status::BadHandle);
                }
                match p.map_memobj(c.id, c.rights, 0) {
                    Ok(va) => ObjRef::Memory {
                        id: c.id,
                        va,
                        len: c.len,
                    },
                    Err(e) => {
                        crate::obj::mem().release(c.id);
                        return Err(e);
                    }
                }
            }
            ObjKind::Chan => {
                if !crate::obj::chans().retain(c.id) {
                    return Err(Status::BadHandle);
                }
                ObjRef::Chan {
                    id: c.id,
                    end: c.len as u8,
                }
            }
            _ => return Err(Status::Unsupported),
        };
        let handle = match p.handles_mut().insert(obj, c.rights) {
            Some(h) => h,
            None => {
                // Unmap first: the frames must not be freed while mapped.
                if let ObjRef::Memory { va, .. } = obj {
                    let _ = p.unmap_memobj(va);
                }
                match obj {
                    ObjRef::Memory { id, .. } => crate::obj::mem().release(id),
                    ObjRef::Chan { id, .. } => crate::obj::chans().release(id),
                    _ => {}
                }
                return Err(Status::OutOfMemory);
            }
        };
        caps[n_caps] = CapDesc {
            kind: c.kind as u32,
            _pad0: 0,
            rights: c.rights,
            handle: handle.0,
        };
        n_caps += 1;
    }

    // The kernel is the root of trust: `init` (parent 0) additionally gets a
    // device capability so it can map the framebuffer; children must be given
    // one explicitly.
    if p.parent() == 0 {
        if let Some(dev) = p
            .handles_mut()
            .insert(ObjRef::Device { node: 0 }, rights::MAP)
        {
            caps[n_caps] = CapDesc {
                kind: ObjKind::Device as u32,
                _pad0: 0,
                rights: rights::MAP,
                handle: dev.0,
            };
            n_caps += 1;
        }
    }

    // Layout from the top of the stack downwards: StartupBlock, then the
    // capability array, then rsp.  The array is placed *below* the whole block
    // (block size + array size), otherwise a 2-capability array would overlap
    // the block and the block write would corrupt caps[1].
    let block_va = (stack_top - size_of::<StartupBlock>() as u64) & !0xf;
    let caps_va = (block_va - (n_caps as u64) * size_of::<CapDesc>() as u64) & !0xf;
    let rsp = caps_va - 16;

    p.write_user(caps_va, bytes_of(&caps[..n_caps]))?;
    let block = StartupBlock {
        hdr: StructHeader::new(size_of::<StartupBlock>() as u32),
        abi_version: ABI_VERSION,
        _pad0: 0,
        feature_bits: rondos_abi::feature::SSE | rondos_abi::feature::DEVICE_MAP,
        entry: image.entry,
        image_base: 0x40_0000,
        argv: Slice::default(),
        envp: Slice::default(),
        caps: Slice {
            ptr: caps_va,
            count: n_caps as u64,
        },
        window: Handle::INVALID,
        random_seed: crate::thread::ticks() ^ 0x9E37_79B9_7F4A_7C15,
        _reserved: [0; 4],
    };
    p.write_user(block_va, as_bytes(&block))?;
    Ok((rsp, block_va))
}

/// Load `elf` into a fresh process and start its first thread.  `rdi` on entry
/// points at the `StartupBlock`.
pub fn spawn_bytes(name: &'static str, elf: &[u8], parent: u32) -> Result<u32, Status> {
    spawn_bytes_with_caps(name, elf, &[], parent)
}

/// As [`spawn_bytes`], additionally installing `caps` in the child.
pub fn spawn_bytes_with_caps(
    name: &'static str,
    elf: &[u8],
    extra: &[PendingCap],
    parent: u32,
) -> Result<u32, Status> {
    let pid = crate::proc::table().create(parent).ok_or(Status::OutOfMemory)?;

    let prepared = {
        let Some(p) = crate::proc::table().get(pid) else {
            return Err(Status::NotFound);
        };
        (|| -> Result<(usize, Image, u64, u64), Status> {
            let image = load_elf(p, elf)?;
            let stack_top = map_stack(p, &image)?;
            let (rsp, block) = write_startup(p, &image, stack_top, extra)?;
            Ok((p.root(), image, rsp, block))
        })()
    };
    let (root, image, rsp, block) = match prepared {
        Ok(v) => v,
        Err(e) => {
            crate::proc::table().reap(pid);
            return Err(e);
        }
    };

    // `rdi` = &StartupBlock (at the top of the stack, above rsp).
    let tid = match crate::thread::thread_create_user(pid, root, name, image.entry, rsp, block) {
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
        "exec: '{}' pid {} tid {} entry {:#x} {} segment(s) end {:#x} rsp {:#x} startup {:#x}",
        name,
        pid,
        tid,
        image.entry,
        image.segments,
        image.end,
        rsp,
        block
    );
    Ok(pid)
}

/// Read a tar entry into freshly allocated frames and spawn it.
pub fn spawn_entry(entry: &Entry, parent: u32) -> Result<u32, Status> {
    spawn_entry_with_caps(entry, &[], parent)
}

/// As [`spawn_entry`], delegating `caps` to the child.
pub fn spawn_entry_with_caps(
    entry: &Entry,
    caps: &[PendingCap],
    parent: u32,
) -> Result<u32, Status> {
    let fs = TarFs::root().ok_or(Status::NotFound)?;
    let len = entry.len as usize;
    if len == 0 || len > 16 * 1024 * 1024 {
        return Err(Status::InvalidArgument);
    }
    let pages = (len + PAGE_SIZE - 1) / PAGE_SIZE;
    let buf = crate::mm::page_alloc()
        .get_page(pages)
        .ok_or(Status::OutOfMemory)?;
    let result = {
        let dst = unsafe { core::slice::from_raw_parts_mut(buf, len) };
        fs.read_all(entry, dst).and_then(|n| {
            spawn_bytes_with_caps(
                core::str::from_utf8(entry.name).unwrap_or("image"),
                &dst[..n],
                caps,
                parent,
            )
        })
    };
    crate::mm::page_alloc().free_page(buf, pages);
    result
}

/// Look up `path` in the boot tar and spawn it.
pub fn spawn_path(path: &[u8]) -> Result<u32, Status> {
    let fs = TarFs::root().ok_or(Status::NotFound)?;
    let entry = fs.find(path).ok_or(Status::NotFound)?;
    spawn_entry(&entry, 0)
}

/// The `ExitStatus` of `pid` as an ABI struct (for `sys_proc_status`).
pub fn exit_status(pid: u32) -> ExitStatus {
    let mut out = ExitStatus {
        hdr: StructHeader::new(size_of::<ExitStatus>() as u32),
        ..Default::default()
    };
    match crate::proc::table().status_of(pid) {
        None | Some(crate::proc::ExitStatus::Running) => {
            out.kind = rondos_abi::exit_kind::RUNNING
        }
        Some(crate::proc::ExitStatus::Exited(code)) => {
            out.kind = rondos_abi::exit_kind::EXITED;
            out.code = code;
        }
        Some(crate::proc::ExitStatus::Killed) => out.kind = rondos_abi::exit_kind::KILLED,
        Some(crate::proc::ExitStatus::Fault { vector, rip, addr }) => {
            out.kind = rondos_abi::exit_kind::FAULT;
            out.vector = vector;
            out.rip = rip;
            out.addr = addr;
        }
    }
    out
}
