use core::fmt;

use bit_field::BitField;

use crate::{x86::{lidt, DescriptorTablePointer, PrivilegeLevel, SegmentSelector, EFlags}, loader::SEGMENT_KERNEL_CODE};

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Interrupts {
    DivideError = 0,
    DebugException = 1,
    Interrupt = 2,
    BreakpointException = 3,
    OverflowException = 4,
    BoundRangeExceededException = 5,
    InvalidOpcodeException = 6,
    DeviceNotAvailableException = 7,
    DoubleFaultException = 8,
    CoprocessorSegmentOverrun = 9,
    InvaildTSSException = 10,
    SegmentNotPresent = 11,
    StackFaultException = 12,
    GeneralProtectionException = 13,
    PageFaultException = 14,
    FPUFloatingPointException = 16,
    AlignmentCheckException = 17,
    MachineCheckException = 18,
    SIMDFloatingPointException = 19,
}

#[repr(C)]
pub struct ExceptionStackFrame {
    pub instruction_pointer: u32,
    pub code_segment: SegmentSelector,
    pub cpu_flags: EFlags,
    // stack_pointer: u32,
    // stack_segment: SegmentSelector,
}

impl fmt::Debug for ExceptionStackFrame {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut s = f.debug_struct("ExceptionStackFrame");
        s.field("eip", &format_args!("0x{:x}", self.instruction_pointer));
        s.field("cs", &self.code_segment);
        s.field("eflags", &self.cpu_flags);
        s.finish()
    }
}

#[macro_export]
macro_rules! handler {
    ($name: ident) => {{
        #[naked]
        extern "C" fn wrapper() -> ! {
            unsafe {
                core::arch::asm!(
                    "mov edi, esp",
                    "push edi",
                    "call {}",
                    sym $name,
                    options(noreturn)
                );
            }
        }
        wrapper
    }}
}

pub struct Idt(pub [Entry; 256]);

#[allow(unaligned_references)]
impl Idt {
    pub fn new() -> Idt {
        Idt([Entry::missing(); 256])
    }

    
    pub fn set_handler(&mut self, entry: Interrupts, handler: HandlerFunc) -> &mut EntryOptions {
        self.0[entry as u8 as usize] = Entry::new(SegmentSelector(SEGMENT_KERNEL_CODE), handler);
        &mut self.0[entry as u8 as usize].options
    }

    pub fn load(&'static self) {
        use core::mem::size_of;

        let ptr = DescriptorTablePointer {
            base: self as *const _ as u32,
            limit: (size_of::<Self>() - 1) as u16,
        };

        lidt(&ptr);
    }
}

pub type HandlerFunc = extern "C" fn() -> !;

#[derive(Clone, Copy)]
#[repr(C, packed)]
pub struct Entry {
    pointer_low: u16,
    gdt_selector: SegmentSelector,
    options: EntryOptions,
    pointer_middle: u16,
}

impl Entry {
    fn new(gdt_selector: SegmentSelector, handler: HandlerFunc) -> Self {
        let pointer = handler as u64;
        Entry {
            gdt_selector,
            pointer_low: pointer as u16,
            pointer_middle: (pointer >> 16) as u16,
            options: EntryOptions::new(),
        }
    }

    fn missing() -> Self {
        Entry {
            gdt_selector: SegmentSelector::new(0, PrivilegeLevel::Ring0),
            pointer_low: 0,
            pointer_middle: 0,
            options: EntryOptions::minimal(),
        }
    }
}

#[allow(unaligned_references)]
impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut s = f.debug_struct("Entry");
        s.field("pointer", &format_args!("0x{:x}", (self.pointer_middle as u32) << 16 | (self.pointer_low as u32)));
        s.field("gdt_selector", &self.gdt_selector);
        s.field("option", &self.options);
        s.finish()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EntryOptions(u16);

impl EntryOptions {
    fn minimal() -> Self {
        let mut options = 0;
        options.set_bits(9..12, 0b111); // 'must-be-one' bits
        EntryOptions(options)
    }

    fn new() -> Self {
        let mut options = Self::minimal();
        options.set_present(true).disable_interrupts(true);
        options
    }

    pub fn set_present(&mut self, present: bool) -> &mut Self {
        self.0.set_bit(15, present);
        self
    }

    pub fn disable_interrupts(&mut self, disable: bool) -> &mut Self {
        self.0.set_bit(8, !disable);
        self
    }

    pub fn set_privilege_level(&mut self, dpl: u16) -> &mut Self {
        self.0.set_bits(13..15, dpl);
        self
    }

    pub fn set_stack_index(&mut self, index: u16) -> &mut Self {
        self.0.set_bits(0..3, index);
        self
    }
}
