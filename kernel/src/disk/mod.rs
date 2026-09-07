//! Block device layer.
//!
//! Arch-neutral disk I/O facade.  The concrete driver (today: QEMU IDE via
//! ATA PIO, see `ata`) registers itself here; kernel code talks to `read_sectors`
//! / `write_sectors` and stays ignorant of the bus.  A future file system
//! (planned for the user-mode milestone) will sit on top of this layer.

#![allow(dead_code)]

use crate::arch::InterruptGuard;
use crate::utils::spinlock::SpinLock;

pub mod ata;

pub use ata::AtaPio;

/// Logical block / sector size used by all drivers.
pub const SECTOR_SIZE: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskError {
    /// No disk is registered.
    NotReady,
    /// The device did not answer in time.
    Timeout,
    /// The controller reported an error.
    Io,
    /// Sector address or buffer length is out of range.
    BadAddress,
}

/// A block device.
pub trait BlockDevice {
    /// Total number of 512-byte sectors.
    fn sector_count(&self) -> u64;

    /// Read whole 512-byte sectors into `buf` starting at `lba`.
    /// `buf.len()` must be a multiple of [`SECTOR_SIZE`].
    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DiskError>;

    /// Write whole 512-byte sectors from `buf` starting at `lba`.
    /// `buf.len()` must be a multiple of [`SECTOR_SIZE`].
    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<(), DiskError>;
}

/// The registered system disk.
static DISK: SpinLock<Option<AtaPio>> = SpinLock::new(None);

/// Probe and register the system disk.  Returns true if a drive was found.
pub fn init() -> bool {
    let mut ata = AtaPio::new(0x1f0);
    if !ata.identify() {
        return false;
    }
    DISK.with(|d| *d = Some(ata));
    true
}

/// Number of sectors on the registered disk, if any.
pub fn sector_count() -> Option<u64> {
    DISK.with(|d| d.as_ref().map(|a| a.sector_count()))
}

/// Read sectors through the registered disk.
pub fn read_sectors(lba: u64, buf: &mut [u8]) -> Result<(), DiskError> {
    let _g = InterruptGuard::new();
    DISK.with(|d| d.as_mut().ok_or(DiskError::NotReady)?.read_sectors(lba, buf))
}

/// Write sectors through the registered disk.
pub fn write_sectors(lba: u64, buf: &[u8]) -> Result<(), DiskError> {
    let _g = InterruptGuard::new();
    DISK.with(|d| d.as_mut().ok_or(DiskError::NotReady)?.write_sectors(lba, buf))
}
