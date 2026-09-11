//! ACPI tables and S5 power-off.
//!
//! The design's ACPI scope is deliberately tiny (`docs/user-mode-design.md`
//! §15: shutdown and reset only; no MADT, no AML interpreter).  The UEFI stub
//! hands over the RSDP physical address in `BootInfo.acpi_rsdp`, and this
//! module walks
//!
//! ```text
//!   RSDP ─▶ RSDT / XSDT ─▶ FADT ─▶ DSDT ─▶ Name (_S5_, Package (...))
//! ```
//!
//! once at boot, keeping the PM1a/PM1b control-block ports and the two
//! `SLP_TYP` values.  [`power_off`] then writes
//! `(SLP_TYP << 10) | SLP_EN` to PM1a (and PM1b when it exists), which is what
//! the ACPI specification calls for.
//!
//! Physical reads go through the kernel physmap; the loader maps at least the
//! low 4 GiB, which is where ACPI tables live, so this also works on the
//! loader's scaffolding tables.

#![allow(dead_code)]

use core::cell::UnsafeCell;

use crate::arch::x86_64::paging::phys_to_virt;
use crate::arch::x86_64::outw;

/// `SLP_EN` in PM1_CNT: writing it with a valid `SLP_TYP` starts the sleep.
const SLP_EN: u16 = 1 << 13;

/// Upper bound on the physical addresses we will dereference: the loader
/// physmaps at least the low 4 GiB, and every ACPI table lives there.
const MAX_PHYS: u64 = 1 << 32;

/// Everything `_S5_` needs, resolved once by [`init`].
#[derive(Clone, Copy)]
pub struct PowerInfo {
    /// Set once `init` has run, so a later call is a no-op.
    pub inited: bool,
    /// True when the FADT *and* the DSDT `_S5_` package were both parsed.
    pub found: bool,
    pub rsdp: u64,
    pub fadt: u64,
    pub dsdt: u64,
    pub pm1a_cnt: u32,
    pub pm1b_cnt: u32,
    pub slp_typa: u16,
    pub slp_typb: u16,
}

impl PowerInfo {
    const fn new() -> Self {
        Self {
            inited: false,
            found: false,
            rsdp: 0,
            fadt: 0,
            dsdt: 0,
            pm1a_cnt: 0,
            pm1b_cnt: 0,
            slp_typa: 0,
            slp_typb: 0,
        }
    }
}

struct InfoCell(UnsafeCell<PowerInfo>);

unsafe impl Sync for InfoCell {}

static INFO: InfoCell = InfoCell(UnsafeCell::new(PowerInfo::new()));

fn info() -> &'static mut PowerInfo {
    unsafe { &mut *INFO.0.get() }
}

/// The parsed power-off parameters (a copy, for the boot self-check).
pub fn probe() -> PowerInfo {
    let i = info();
    if !i.inited {
        init();
    }
    *info()
}

#[inline]
fn phys_ok(pa: u64, len: usize) -> bool {
    pa >= 0x1000 && pa.checked_add(len as u64).is_some_and(|end| end <= MAX_PHYS)
}

unsafe fn rd_u8(pa: u64) -> u8 {
    core::ptr::read_unaligned(phys_to_virt(pa as usize) as *const u8)
}

unsafe fn rd_u32(pa: u64) -> u32 {
    core::ptr::read_unaligned(phys_to_virt(pa as usize) as *const u32)
}

unsafe fn rd_u64(pa: u64) -> u64 {
    core::ptr::read_unaligned(phys_to_virt(pa as usize) as *const u64)
}

unsafe fn rd_slice<'a>(pa: u64, len: usize) -> &'a [u8] {
    core::slice::from_raw_parts(phys_to_virt(pa as usize) as *const u8, len)
}

/// Signature of the table at `pa`, or `None` when it is out of the physmap.
fn table_sig(pa: u64) -> Option<[u8; 4]> {
    if !phys_ok(pa, 8) {
        return None;
    }
    let b = unsafe { rd_slice(pa, 4) };
    Some([b[0], b[1], b[2], b[3]])
}

/// Total table length from the common ACPI header.
fn table_len(pa: u64) -> Option<usize> {
    if !phys_ok(pa, 8) {
        return None;
    }
    Some(unsafe { rd_u32(pa + 4) } as usize)
}

/// A Generic Address Structure's address, when it describes system memory.
///
/// Returns 0 for an all-zero GAS (the "not present" encoding) or one that
/// addresses I/O space / has a zero register width.
fn gas_address(pa: u64) -> u64 {
    if !phys_ok(pa, 12) {
        return 0;
    }
    let space = unsafe { rd_u8(pa) };
    let width = unsafe { rd_u8(pa + 1) };
    let addr = unsafe { rd_u64(pa + 4) };
    if space == 0 && width != 0 {
        addr
    } else {
        0
    }
}

/// Parse the ACPI tables once.  Returns true when a usable `_S5_` was found.
pub fn init() -> bool {
    let i = info();
    if i.inited {
        return i.found;
    }
    i.inited = true;

    let rsdp = crate::bootinfo::get().acpi_rsdp;
    i.rsdp = rsdp;
    if !phys_ok(rsdp, 36) || unsafe { rd_slice(rsdp, 8) } != b"RSD PTR " {
        crate::serial_println!("acpi: no usable RSDP (bootinfo.acpi_rsdp {:#x})", rsdp);
        return false;
    }
    let revision = unsafe { rd_u8(rsdp + 15) };
    let rsdt = (unsafe { rd_u32(rsdp + 16) }) as u64;
    let xsdt = if revision >= 2 {
        unsafe { rd_u64(rsdp + 24) }
    } else {
        0
    };

    // ACPI 2.0+ points at an XSDT with 64-bit entries; 1.0 has only the RSDT.
    let (root, wide) = if xsdt != 0 && table_sig(xsdt) == Some(*b"XSDT") {
        (xsdt, true)
    } else if rsdt != 0 && table_sig(rsdt) == Some(*b"RSDT") {
        (rsdt, false)
    } else {
        crate::serial_println!("acpi: RSDP rev {} has no usable RSDT/XSDT", revision);
        return false;
    };
    let Some(len) = table_len(root) else {
        return false;
    };
    if len < 36 || !phys_ok(root, len) {
        return false;
    }
    let step = if wide { 8 } else { 4 };
    let entries = (len - 36) / step;

    let mut fadt = 0u64;
    for k in 0..entries {
        let epa = root + 36 + (k * step) as u64;
        let pa = if wide {
            unsafe { rd_u64(epa) }
        } else {
            (unsafe { rd_u32(epa) }) as u64
        };
        if table_sig(pa) == Some(*b"FACP") {
            fadt = pa;
            break;
        }
    }
    if fadt == 0 {
        crate::serial_println!("acpi: no FADT among {} tables", entries);
        return false;
    }
    i.fadt = fadt;

    let flen = table_len(fadt).unwrap_or(0);
    // Through PM1b_CNT_BLK; a shorter FADT cannot power anything off.
    if flen < 0x48 || !phys_ok(fadt, flen) {
        crate::serial_println!("acpi: FADT too short ({:#x})", flen);
        return false;
    }

    let mut pm1a = unsafe { rd_u32(fadt + 0x40) };
    let mut pm1b = unsafe { rd_u32(fadt + 0x44) };
    // The 64-bit X_ GAS fields win when the table is long enough and they are
    // well-formed; plenty of firmware only fills the 32-bit ones.
    if flen >= 0xB8 {
        let xa = gas_address(fadt + 0xAC);
        let xb = gas_address(fadt + 0xB8);
        if xa != 0 {
            pm1a = xa as u32;
        }
        if xb != 0 {
            pm1b = xb as u32;
        }
    }
    i.pm1a_cnt = pm1a;
    i.pm1b_cnt = pm1b;

    let dsdt32 = (unsafe { rd_u32(fadt + 0x28) }) as u64;
    let xdsdt = if flen >= 0x94 {
        unsafe { rd_u64(fadt + 0x8C) }
    } else {
        0
    };
    let dsdt = if xdsdt != 0 { xdsdt } else { dsdt32 };
    i.dsdt = dsdt;

    if pm1a == 0 {
        crate::serial_println!("acpi: FADT has no PM1a_CNT_BLK");
        return false;
    }
    match find_s5(dsdt) {
        Some((a, b)) => {
            i.slp_typa = a;
            i.slp_typb = b;
            i.found = true;
        }
        None => {
            crate::serial_println!(
                "acpi: FADT {:#x} pm1a {:#x}, but no _S5_ package in DSDT {:#x}",
                fadt,
                pm1a,
                dsdt
            );
        }
    }
    crate::serial_println!(
        "acpi: rev {} FADT {:#x} DSDT {:#x} PM1a_CNT {:#x} SLP_TYPa {} ({}power-off)",
        revision,
        fadt,
        dsdt,
        pm1a,
        i.slp_typa,
        if i.found { "" } else { "no " }
    );
    i.found
}

/// Scan the DSDT for `Name (_S5_, Package (...))` and return
/// `(SLP_TYPa, SLP_TYPb)`.
fn find_s5(dsdt: u64) -> Option<(u16, u16)> {
    if table_sig(dsdt) != Some(*b"DSDT") {
        return None;
    }
    let len = table_len(dsdt)?;
    if len < 36 || !phys_ok(dsdt, len) {
        return None;
    }
    let aml = unsafe { rd_slice(dsdt + 36, len - 36) };

    // `NameOp` then the name; firmware writes it either as the bare name (it is
    // already in the root scope) or with an explicit root prefix.
    let mut i = 0usize;
    while i + 5 <= aml.len() {
        let after = if aml[i] == 0x08 && aml[i + 1..i + 5] == *b"_S5_" {
            i + 5
        } else if i + 6 <= aml.len()
            && aml[i] == 0x08
            && aml[i + 1] == 0x5C // RootChar
            && aml[i + 2..i + 6] == *b"_S5_"
        {
            i + 6
        } else {
            i += 1;
            continue;
        };
        if let Some(v) = parse_s5_package(&aml[after..]) {
            return Some(v);
        }
        i = after;
    }
    None
}

/// `PackageOp PkgLength NumElements elem0 elem1 ...` — take the first two
/// elements, which the spec defines as SLP_TYPa and SLP_TYPb.
fn parse_s5_package(b: &[u8]) -> Option<(u16, u16)> {
    if *b.first()? != 0x12 {
        return None; // not a Package
    }
    let (_pkg_len, n) = decode_pkg_length(&b[1..])?;
    let mut j = 1 + n;
    let _num_elements = *b.get(j)?;
    j += 1;
    let (a, adv) = parse_int(&b[j..])?;
    j += adv;
    let (b2, _) = parse_int(&b[j..]).unwrap_or((a, 0));
    Some((a as u16, b2 as u16))
}

/// AML `PkgLength`: two high bits say how many extra bytes follow; the low six
/// bits plus those bytes hold the length.  Returns `(length, bytes consumed)`.
fn decode_pkg_length(b: &[u8]) -> Option<(usize, usize)> {
    let first = *b.first()?;
    let extra = (first >> 6) as usize;
    if extra > 3 {
        return None;
    }
    let mut len = (first & 0x3F) as usize;
    for k in 0..extra {
        len |= (*b.get(1 + k)? as usize) << (6 + 8 * k);
    }
    Some((len, 1 + extra))
}

/// One AML integer constant.  Returns `(value, bytes consumed)`; `None` for a
/// named reference (which this deliberately does not resolve).
fn parse_int(b: &[u8]) -> Option<(u64, usize)> {
    match *b.first()? {
        0x00 => Some((0, 1)),        // ZeroOp
        0x01 => Some((1, 1)),        // OneOp
        0xFF => Some((u64::MAX, 1)), // OnesOp
        0x0A => Some((*b.get(1)? as u64, 2)),
        0x0B => {
            let v = u16::from_le_bytes([*b.get(1)?, *b.get(2)?]);
            Some((v as u64, 3))
        }
        0x0C => {
            let v = u32::from_le_bytes([*b.get(1)?, *b.get(2)?, *b.get(3)?, *b.get(4)?]);
            Some((v as u64, 5))
        }
        0x0E => {
            let v = u64::from_le_bytes([
                *b.get(1)?,
                *b.get(2)?,
                *b.get(3)?,
                *b.get(4)?,
                *b.get(5)?,
                *b.get(6)?,
                *b.get(7)?,
                *b.get(8)?,
            ]);
            Some((v, 9))
        }
        _ => None,
    }
}

/// Power the machine off (ACPI S5).
///
/// Returns true when the FADT gave a real PM1 control block, i.e. a proper
/// shutdown command was issued.  When ACPI was unavailable it falls back to the
/// PM1a_CNT ports the common emulators use and returns false, so the caller can
/// tell the user the machine is not guaranteed to go off.
pub fn power_off() -> bool {
    let i = probe();
    if i.found && i.pm1a_cnt != 0 {
        outw(i.pm1a_cnt as u16, (i.slp_typa << 10) | SLP_EN);
        if i.pm1b_cnt != 0 {
            outw(i.pm1b_cnt as u16, (i.slp_typb << 10) | SLP_EN);
        }
        return true;
    }
    // No `_S5_`: try the PM1a_CNT bases QEMU (legacy 0xB004 and Q35/ICH9
    // 0x604), Bochs and VirtualBox use, with their usual SLP_TYP values.
    outw(0x604, 0x2000);
    outw(0xB004, 0x2000);
    outw(0x4004, 0x3400);
    false
}
