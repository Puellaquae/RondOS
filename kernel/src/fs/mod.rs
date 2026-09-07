//! A thin VFS layer.
//!
//! Today there is a single mounted file system: the in-memory [`ramfs`],
//! populated at boot from a tar image read off the disk.  The public surface
//! (`ls` / `read_file` / ...) is deliberately small and path-based; it is the
//! shape the user-mode syscall layer will later build on, and a real on-disk
//! file system can be mounted behind the same interface.

#![allow(dead_code)]

pub mod ramfs;
pub mod tar;

use alloc::vec;
use alloc::vec::Vec;

use crate::utils::spinlock::SpinLock;

use ramfs::RamFs;

/// The mounted root file system (set up by [`init`]).
static ROOT_FS: SpinLock<Option<RamFs>> = SpinLock::new(None);

/// Mount an empty ramfs as `/`.
pub fn init() {
    ROOT_FS.with(|fs| *fs = Some(RamFs::new()));
}

/// Create a directory (and its parents) at `path`.
pub fn mkdir(path: &[u8]) -> bool {
    ROOT_FS.with(|fs| fs.as_mut().map(|f| f.mkdir(path)).unwrap_or(false))
}

/// Write `data` to `path` (creating parents).
pub fn write_file(path: &[u8], data: Vec<u8>) -> bool {
    ROOT_FS.with(|fs| fs.as_mut().map(|f| f.write_file(path, data)).unwrap_or(false))
}

/// Read a whole file into a fresh allocation.
pub fn read_file(path: &str) -> Option<Vec<u8>> {
    ROOT_FS.with(|fs| fs.as_ref().and_then(|f| f.read_file(path.as_bytes())).map(|d| d.to_vec()))
}

/// List a directory.  Each entry is `(name, is_directory)`.
pub fn list(path: &str) -> Option<Vec<(Vec<u8>, bool)>> {
    ROOT_FS.with(|fs| fs.as_ref().and_then(|f| f.list(path.as_bytes())))
}

/// Load a tar archive (already in memory) into the mounted ramfs.
/// Returns the number of entries imported.
pub fn load_tar_image(buf: &[u8]) -> usize {
    tar::load(buf)
}

/// LBA where the boot tar image lives on disk.img.  Must match the Makefile
/// (the kernel is padded to 512 KiB, then the tar archive is appended).
pub const TAR_OFFSET_LBA: u64 = 1024;

/// Tar region size in sectors read from disk (up to 512 KiB of archive).
const TAR_REGION_SECTORS: usize = 1024;

/// Read the boot tar image from disk and import it into the mounted ramfs.
pub fn load_tar_from_disk() -> Result<usize, crate::disk::DiskError> {
    let mut buf = vec![0u8; TAR_REGION_SECTORS * crate::disk::SECTOR_SIZE];
    crate::disk::read_sectors(TAR_OFFSET_LBA, &mut buf)?;
    Ok(tar::load(&buf))
}
