use core::{arch::asm, fmt};
use bit_field::BitField;

/// Specifies which element to load into a segment from
/// descriptor tables (i.e., is a index to LDT or GDT table
/// with some additional flags).
///
/// See Intel 3a, Section 3.4.2 "Segment Selectors"
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct SegmentSelector(pub u16);

impl SegmentSelector {
    /// Creates a new SegmentSelector
    ///
    /// # Arguments
    ///  * `index`: index in GDT or LDT array (not the offset)
    ///  * `rpl`: the requested privilege level
    #[inline]
    pub const fn new(index: u16, rpl: PrivilegeLevel) -> SegmentSelector {
        SegmentSelector(index << 3 | (rpl as u16))
    }

    /// Returns the GDT index.
    #[inline]
    pub fn index(self) -> u16 {
        self.0 >> 3
    }

    /// Returns the requested privilege level.
    #[inline]
    pub fn rpl(self) -> PrivilegeLevel {
        PrivilegeLevel::from_u16(self.0.get_bits(0..2))
    }

    /// Set the privilege level for this Segment selector.
    #[inline]
    pub fn set_rpl(&mut self, rpl: PrivilegeLevel) {
        self.0.set_bits(0..2, rpl as u16);
    }
}

impl fmt::Debug for SegmentSelector {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut s = f.debug_struct("SegmentSelector");
        s.field("index", &self.index());
        s.field("rpl", &self.rpl());
        s.finish()
    }
}

/// Represents a protection ring level.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PrivilegeLevel {
    /// Privilege-level 0 (most privilege): This level is used by critical system-software
    /// components that require direct access to, and control over, all processor and system
    /// resources. This can include BIOS, memory-management functions, and interrupt handlers.
    Ring0 = 0,

    /// Privilege-level 1 (moderate privilege): This level is used by less-critical system-
    /// software services that can access and control a limited scope of processor and system
    /// resources. Software running at these privilege levels might include some device drivers
    /// and library routines. The actual privileges of this level are defined by the
    /// operating system.
    Ring1 = 1,

    /// Privilege-level 2 (moderate privilege): Like level 1, this level is used by
    /// less-critical system-software services that can access and control a limited scope of
    /// processor and system resources. The actual privileges of this level are defined by the
    /// operating system.
    Ring2 = 2,

    /// Privilege-level 3 (least privilege): This level is used by application software.
    /// Software running at privilege-level 3 is normally prevented from directly accessing
    /// most processor and system resources. Instead, applications request access to the
    /// protected processor and system resources by calling more-privileged service routines
    /// to perform the accesses.
    Ring3 = 3,
}

impl PrivilegeLevel {
    /// Creates a `PrivilegeLevel` from a numeric value. The value must be in the range 0..4.
    ///
    /// This function panics if the passed value is >3.
    #[inline]
    pub fn from_u16(value: u16) -> PrivilegeLevel {
        match value {
            0 => PrivilegeLevel::Ring0,
            1 => PrivilegeLevel::Ring1,
            2 => PrivilegeLevel::Ring2,
            3 => PrivilegeLevel::Ring3,
            i => panic!("{} is not a valid privilege level", i),
        }
    }
}

/// A struct describing a pointer to a descriptor table (GDT / IDT).
/// This is in a format suitable for giving to 'lgdt' or 'lidt'.
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct DescriptorTablePointer {
    /// Size of the DT.
    pub limit: u16,
    /// Pointer to the memory region containing the DT.
    pub base: u32,
}

pub fn outb(port: u16, data: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") data);
    }
}

pub fn outw(port: u16, data: u16) {
    unsafe {
        asm!("out dx, ax", in("dx") port, in("ax") data);
    }
}

pub fn get_cs() -> u16 {
    let cs: u16;
    unsafe {
        asm!("mov ax, cs", out("ax") cs);
    }
    cs
}

pub fn get_cr2() -> u32 {
    let cr2: u32;
    unsafe {
        asm!("mov eax, cr2", out("eax") cr2);
    }
    cr2
}

pub fn lidt(idt: &DescriptorTablePointer) {
    unsafe {
        asm!("lidt [{}]", in(reg) idt, options(readonly, nostack, preserves_flags));
    }
}

use bitflags::bitflags;

bitflags! {
    /// The RFLAGS register.
    pub struct EFlags: u32 {
        /// Processor feature identification flag.
        ///
        /// If this flag is modifiable, the CPU supports CPUID.
        const ID = 1 << 21;
        /// Indicates that an external, maskable interrupt is pending.
        ///
        /// Used when virtual-8086 mode extensions (CR4.VME) or protected-mode virtual
        /// interrupts (CR4.PVI) are activated.
        const VIRTUAL_INTERRUPT_PENDING = 1 << 20;
        /// Virtual image of the INTERRUPT_FLAG bit.
        ///
        /// Used when virtual-8086 mode extensions (CR4.VME) or protected-mode virtual
        /// interrupts (CR4.PVI) are activated.
        const VIRTUAL_INTERRUPT = 1 << 19;
        /// Enable automatic alignment checking if CR0.AM is set. Only works if CPL is 3.
        const ALIGNMENT_CHECK = 1 << 18;
        /// Enable the virtual-8086 mode.
        const VIRTUAL_8086_MODE = 1 << 17;
        /// Allows to restart an instruction following an instrucion breakpoint.
        const RESUME_FLAG = 1 << 16;
        /// Used by `iret` in hardware task switch mode to determine if current task is nested.
        const NESTED_TASK = 1 << 14;
        /// The high bit of the I/O Privilege Level field.
        ///
        /// Specifies the privilege level required for executing I/O address-space instructions.
        const IOPL_HIGH = 1 << 13;
        /// The low bit of the I/O Privilege Level field.
        ///
        /// Specifies the privilege level required for executing I/O address-space instructions.
        const IOPL_LOW = 1 << 12;
        /// Set by hardware to indicate that the sign bit of the result of the last signed integer
        /// operation differs from the source operands.
        const OVERFLOW_FLAG = 1 << 11;
        /// Determines the order in which strings are processed.
        const DIRECTION_FLAG = 1 << 10;
        /// Enable interrupts.
        const INTERRUPT_FLAG = 1 << 9;
        /// Enable single-step mode for debugging.
        const TRAP_FLAG = 1 << 8;
        /// Set by hardware if last arithmetic operation resulted in a negative value.
        const SIGN_FLAG = 1 << 7;
        /// Set by hardware if last arithmetic operation resulted in a zero value.
        const ZERO_FLAG = 1 << 6;
        /// Set by hardware if last arithmetic operation generated a carry ouf of bit 3 of the
        /// result.
        const AUXILIARY_CARRY_FLAG = 1 << 4;
        /// Set by hardware if last result has an even number of 1 bits (only for some operations).
        const PARITY_FLAG = 1 << 2;
        /// Set by hardware if last arithmetic operation generated a carry out of the
        /// most-significant bit of the result.
        const CARRY_FLAG = 1;
    }
}