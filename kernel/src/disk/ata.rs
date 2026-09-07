//! ATA PIO (polled IDE) driver for the primary controller in QEMU.
//!
//! Implements 28-bit LBA, master drive on the primary bus (ports 0x1f0..0x1f7).
//! Polling is used instead of IRQs, so it works with interrupts disabled and
//! keeps the driver self-contained.  Busy-waits are bounded so a missing
//! device returns an error instead of hanging the kernel.

#![allow(dead_code)]

use core::hint::spin_loop;

use crate::arch::x86::{inb, inw, outb, outw};

use super::{BlockDevice, DiskError, SECTOR_SIZE};

const STATUS_BSY: u8 = 0x80;
const STATUS_DRDY: u8 = 0x40;
const STATUS_DRQ: u8 = 0x08;
const STATUS_ERR: u8 = 0x01;

/// A bounded number of status polls before giving up.
const POLLS: u32 = 0x1000_0000;

pub struct AtaPio {
    base: u16,
    sectors: u64,
}

impl AtaPio {
    pub const fn new(base: u16) -> Self {
        Self { base, sectors: 0 }
    }

    /// Probe the master drive on this bus with the ATA IDENTIFY command.
    /// On success `sectors` holds the 28-bit LBA capacity.
    pub fn identify(&mut self) -> bool {
        // Select master.
        outb(self.base + 6, 0xa0);
        // Issue IDENTIFY.
        outb(self.base + 7, 0xec);

        if self.wait_not_busy().is_err() {
            return false;
        }
        let st = inb(self.base + 7);
        if st == 0 || st & STATUS_ERR != 0 {
            return false;
        }
        if self.wait_drq().is_err() {
            return false;
        }

        let mut ident = [0u16; 256];
        for word in ident.iter_mut() {
            *word = inw(self.base);
        }
        self.sectors = ((ident[61] as u64) << 16) | ident[60] as u64;
        true
    }

    /// Program the drive/head + sector-count + LBA registers for one sector.
    fn select_lba(&self, lba: u64) {
        debug_assert!(lba < (1 << 28));
        // Master, LBA mode.
        outb(self.base + 6, 0xe0 | (((lba >> 24) & 0x0f) as u8));
        outb(self.base + 2, 1); // one sector
        outb(self.base + 3, lba as u8);
        outb(self.base + 4, (lba >> 8) as u8);
        outb(self.base + 5, (lba >> 16) as u8);
    }

    fn read_sector(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DiskError> {
        self.wait_not_busy()?;
        self.select_lba(lba);
        outb(self.base + 7, 0x20); // READ SECTORS (with retry)
        self.wait_drq()?;

        let p = buf.as_mut_ptr() as *mut u16;
        unsafe {
            for i in 0..(SECTOR_SIZE / 2) {
                p.add(i).write(inw(self.base));
            }
        }
        self.wait_not_busy()
    }

    fn write_sector(&mut self, lba: u64, buf: &[u8]) -> Result<(), DiskError> {
        self.wait_not_busy()?;
        self.select_lba(lba);
        outb(self.base + 7, 0x30); // WRITE SECTORS (with retry)
        self.wait_drq()?;

        let p = buf.as_ptr() as *const u16;
        unsafe {
            for i in 0..(SECTOR_SIZE / 2) {
                outw(self.base, p.add(i).read());
            }
        }
        self.wait_not_busy()?;
        // A final status read latches the result; check for a write error.
        if inb(self.base + 7) & STATUS_ERR != 0 {
            return Err(DiskError::Io);
        }
        Ok(())
    }

    fn wait_not_busy(&self) -> Result<(), DiskError> {
        for _ in 0..POLLS {
            if inb(self.base + 7) & STATUS_BSY == 0 {
                return Ok(());
            }
            spin_loop();
        }
        Err(DiskError::Timeout)
    }

    fn wait_drq(&self) -> Result<(), DiskError> {
        for _ in 0..POLLS {
            let st = inb(self.base + 7);
            if st & STATUS_BSY != 0 {
                continue;
            }
            if st & STATUS_ERR != 0 {
                return Err(DiskError::Io);
            }
            if st & STATUS_DRQ != 0 {
                return Ok(());
            }
            spin_loop();
        }
        Err(DiskError::Timeout)
    }

    fn check_range(&self, lba: u64, len: usize) -> Result<usize, DiskError> {
        if self.sectors == 0 {
            return Err(DiskError::NotReady);
        }
        if len == 0 || len % SECTOR_SIZE != 0 || lba + (len / SECTOR_SIZE) as u64 > self.sectors {
            return Err(DiskError::BadAddress);
        }
        Ok(len / SECTOR_SIZE)
    }
}

impl BlockDevice for AtaPio {
    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DiskError> {
        let count = self.check_range(lba, buf.len())?;
        for i in 0..count {
            let off = i * SECTOR_SIZE;
            self.read_sector(lba + i as u64, &mut buf[off..off + SECTOR_SIZE])?;
        }
        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<(), DiskError> {
        let count = self.check_range(lba, buf.len())?;
        for i in 0..count {
            let off = i * SECTOR_SIZE;
            self.write_sector(lba + i as u64, &buf[off..off + SECTOR_SIZE])?;
        }
        Ok(())
    }
}
