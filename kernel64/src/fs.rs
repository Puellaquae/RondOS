//! The boot tar as a read-only file system — P1b.
//!
//! `BootInfo.initrd_*` points at the ustar image the UEFI stub loaded from the
//! ESP.  That image *is* the v1 root file system: `bin/init`, `bin/crash`, ...
//! `tools/mktar.py` writes it with flat names and no GNU extensions, so this
//! reader is deliberately tiny (no hard links, no pax headers, no compression).
//!
//! A file handle stores `(offset, len, cursor)` — see [`crate::proc::ObjRef`] —
//! so the file system itself is stateless and immutable: opening a path is a
//! lookup, reading is a `memcpy` out of the tar, writing is `Permission`.

#![allow(dead_code)]

use rondos_abi::Status;

const TAR_BLOCK: usize = 512;
const NAME_MAX: usize = 100;

/// One regular file inside the image.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    /// Offset of the file's first byte inside the tar.
    pub off: u32,
    /// Length of the file in bytes.
    pub len: u32,
    /// Name as stored in the archive (borrows the tar itself).
    pub name: &'static [u8],
}

pub struct TarFs {
    tar: &'static [u8],
}

fn octal(field: &[u8]) -> Option<u32> {
    let mut v = 0u32;
    let mut seen = false;
    for b in field {
        match b {
            b'0'..=b'7' => {
                v = v.checked_mul(8)?.checked_add((b - b'0') as u32)?;
                seen = true;
            }
            0 | b' ' if !seen => {}
            0 | b' ' => break,
            _ => return None,
        }
    }
    Some(v)
}

fn name_of(hdr: &'static [u8]) -> &'static [u8] {
    let field = &hdr[0..NAME_MAX];
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    &field[..end]
}

impl TarFs {
    pub const fn new(tar: &'static [u8]) -> Self {
        Self { tar }
    }

    /// The boot image, if the loader handed one over.
    pub fn root() -> Option<TarFs> {
        let bi = crate::bootinfo::get();
        if bi.initrd_phys == 0 || bi.initrd_len < TAR_BLOCK as u64 {
            return None;
        }
        let va = crate::arch::x86_64::paging::phys_to_virt(bi.initrd_phys as usize);
        let tar =
            unsafe { core::slice::from_raw_parts(va as *const u8, bi.initrd_len as usize) };
        Some(TarFs::new(tar))
    }

    pub fn len(&self) -> usize {
        self.tar.len()
    }

    pub fn bytes(&self) -> &'static [u8] {
        self.tar
    }

    /// Walk the archive, yielding every regular file in order.
    pub fn entries(&self) -> impl Iterator<Item = Entry> + '_ {
        let mut off = 0usize;
        let mut done = false;
        core::iter::from_fn(move || {
            if done {
                return None;
            }
            loop {
                let hdr: &'static [u8] = self.tar.get(off..off + TAR_BLOCK)?;
                if hdr.iter().all(|b| *b == 0) {
                    done = true;
                    return None;
                }
                let size = octal(&hdr[124..136])? as usize;
                let typeflag = hdr[156];
                let data = off + TAR_BLOCK;
                if data.checked_add(size)? > self.tar.len() {
                    done = true;
                    return None;
                }
                off = data + (size + TAR_BLOCK - 1) / TAR_BLOCK * TAR_BLOCK;
                if typeflag == b'0' || typeflag == 0 {
                    return Some(Entry {
                        off: data as u32,
                        len: size as u32,
                        name: name_of(hdr),
                    });
                }
                // Not a regular file: keep walking.
            }
        })
    }

    pub fn entry(&self, index: usize) -> Option<Entry> {
        self.entries().nth(index)
    }

    pub fn count(&self) -> usize {
        self.entries().count()
    }

    /// Look up a path.  A leading `/` is optional, so both `/bin/init` and
    /// `bin/init` work; the tar stores names without a leading slash.
    pub fn find(&self, path: &[u8]) -> Option<Entry> {
        let path = path.strip_prefix(b"/").unwrap_or(path);
        if path.is_empty() || path.len() > NAME_MAX {
            return None;
        }
        let mut off = 0usize;
        loop {
            let hdr: &'static [u8] = self.tar.get(off..off + TAR_BLOCK)?;
            if hdr.iter().all(|b| *b == 0) {
                return None;
            }
            let size = octal(&hdr[124..136])? as usize;
            let typeflag = hdr[156];
            let data = off + TAR_BLOCK;
            if data.checked_add(size)? > self.tar.len() {
                return None;
            }
            let name = name_of(hdr);
            if (typeflag == b'0' || typeflag == 0) && name == path {
                return Some(Entry {
                    off: data as u32,
                    len: size as u32,
                    name,
                });
            }
            off = data + (size + TAR_BLOCK - 1) / TAR_BLOCK * TAR_BLOCK;
        }
    }

    /// Copy up to `buf.len()` bytes starting at `pos`; returns bytes copied.
    pub fn read(&self, entry: &Entry, pos: u64, buf: &mut [u8]) -> usize {
        if pos >= entry.len as u64 {
            return 0;
        }
        let start = entry.off as usize + pos as usize;
        let n = (entry.len as u64 - pos).min(buf.len() as u64) as usize;
        match self.tar.get(start..start + n) {
            Some(src) => {
                buf[..n].copy_from_slice(src);
                n
            }
            None => 0,
        }
    }

    /// Copy a whole file into a caller buffer (used by `sys_spawn`).
    pub fn read_all(&self, entry: &Entry, dst: &mut [u8]) -> Result<usize, Status> {
        let n = entry.len as usize;
        if n > dst.len() {
            return Err(Status::OutOfMemory);
        }
        let src = self
            .tar
            .get(entry.off as usize..entry.off as usize + n)
            .ok_or(Status::InvalidArgument)?;
        dst[..n].copy_from_slice(src);
        Ok(n)
    }
}
