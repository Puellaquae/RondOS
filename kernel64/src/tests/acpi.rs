//! ACPI table probe — the plumbing behind `sys_shutdown`.
//!
//! This deliberately does **not** write PM1_CNT: that would power QEMU off in
//! the middle of the smoke suite.  It only proves that the loader handed an
//! RSDP over and that the RSDT/XSDT -> FADT -> DSDT `_S5_` walk succeeded.

use crate::acpi;
use crate::serial_println;

use super::{Case, Verdict};

pub static CASES: &[Case] = &[Case {
    name: "acpi-fadt",
    run: fadt,
}];

fn fadt() -> Verdict {
    let p = acpi::probe();
    serial_println!(
        "acpi: rsdp {:#x} fadt {:#x} dsdt {:#x} pm1a {:#x} pm1b {:#x} slp_a {} slp_b {} found {}",
        p.rsdp,
        p.fadt,
        p.dsdt,
        p.pm1a_cnt,
        p.pm1b_cnt,
        p.slp_typa,
        p.slp_typb,
        p.found
    );
    if p.rsdp == 0 {
        serial_println!("acpi: the loader handed over no RSDP");
        return Verdict::Fail;
    }
    if p.fadt == 0 || p.dsdt == 0 {
        return Verdict::Fail;
    }
    if !p.found {
        serial_println!("acpi: FADT/DSDT found but no _S5_ package");
        return Verdict::Fail;
    }
    if p.pm1a_cnt == 0 || p.pm1a_cnt > u16::MAX as u32 {
        serial_println!("acpi: PM1a_CNT_BLK {:#x} is not an I/O port", p.pm1a_cnt);
        return Verdict::Fail;
    }
    Verdict::Pass
}
