#![allow(dead_code)]

use core::marker::PhantomData;
use core::mem::size_of;
use core::ops::{Index, IndexMut};
use core::ptr::addr_of_mut;

use crate::arch::x86;
use crate::utils::singleton::Singleton;
use crate::utils::BitAccess;

use crate::x86::PrivilegeLevel;

type HandlerFunc = extern "x86-interrupt" fn(frame: ExceptionStackFrame);
type HandlerFuncErrCode = extern "x86-interrupt" fn(frame: ExceptionStackFrame, error_code: u32);
type DivergingHandlerFunc = extern "x86-interrupt" fn(_: ExceptionStackFrame) -> !;
type DivergingHandlerFuncErrCode = extern "x86-interrupt" fn(_: ExceptionStackFrame, _: u32) -> !;

pub static INTR_TABLE: Singleton<InterruptDescriptorTable> = Singleton::UNINIT;

#[repr(C)]
#[repr(align(16))]
pub struct InterruptDescriptorTable {
    /// A divide error (`#DE`) occurs when the denominator of a DIV instruction or
    /// an IDIV instruction is 0. A `#DE` also occurs if the result is too large to be
    /// represented in the destination.
    ///
    /// The saved instruction pointer points to the instruction that caused the `#DE`.
    ///
    /// The vector number of the `#DE` exception is 0.
    pub divide_error: InterruptEntry<HandlerFunc>,

    /// When the debug-exception mechanism is enabled, a `#DB` exception can occur under any
    /// of the following circumstances:
    ///
    /// <details>
    ///
    /// - Instruction execution.
    /// - Instruction single stepping.
    /// - Data read.
    /// - Data write.
    /// - I/O read.
    /// - I/O write.
    /// - Task switch.
    /// - Debug-register access, or general detect fault (debug register access when DR7.GD=1).
    /// - Executing the INT1 instruction (opcode 0F1h).
    ///
    /// </details>
    ///
    /// `#DB` conditions are enabled and disabled using the debug-control register, `DR7`
    /// and `RFLAGS.TF`.
    ///
    /// In the following cases, the saved instruction pointer points to the instruction that
    /// caused the `#DB`:
    ///
    /// - Instruction execution.
    /// - Invalid debug-register access, or general detect.
    ///
    /// In all other cases, the instruction that caused the `#DB` is completed, and the saved
    /// instruction pointer points to the instruction after the one that caused the `#DB`.
    ///
    /// The vector number of the `#DB` exception is 1.
    pub debug: InterruptEntry<HandlerFunc>,

    /// An non maskable interrupt exception (NMI) occurs as a result of system logic
    /// signaling a non-maskable interrupt to the processor.
    ///
    /// The processor recognizes an NMI at an instruction boundary.
    /// The saved instruction pointer points to the instruction immediately following the
    /// boundary where the NMI was recognized.
    ///
    /// The vector number of the NMI exception is 2.
    pub non_maskable_interrupt: InterruptEntry<HandlerFunc>,

    /// A breakpoint (`#BP`) exception occurs when an `INT3` instruction is executed. The
    /// `INT3` is normally used by debug software to set instruction breakpoints by replacing
    ///
    /// The saved instruction pointer points to the byte after the `INT3` instruction.
    ///
    /// The vector number of the `#BP` exception is 3.
    pub breakpoint: InterruptEntry<HandlerFunc>,

    /// An overflow exception (`#OF`) occurs as a result of executing an `INTO` instruction
    /// while the overflow bit in `RFLAGS` is set to 1.
    ///
    /// The saved instruction pointer points to the instruction following the `INTO`
    /// instruction that caused the `#OF`.
    ///
    /// The vector number of the `#OF` exception is 4.
    pub overflow: InterruptEntry<HandlerFunc>,

    /// A bound-range exception (`#BR`) exception can occur as a result of executing
    /// the `BOUND` instruction. The `BOUND` instruction compares an array index (first
    /// operand) with the lower bounds and upper bounds of an array (second operand).
    /// If the array index is not within the array boundary, the `#BR` occurs.
    ///
    /// The saved instruction pointer points to the `BOUND` instruction that caused the `#BR`.
    ///
    /// The vector number of the `#BR` exception is 5.
    pub bound_range_exceeded: InterruptEntry<HandlerFunc>,

    /// An invalid opcode exception (`#UD`) occurs when an attempt is made to execute an
    /// invalid or undefined opcode. The validity of an opcode often depends on the
    /// processor operating mode.
    ///
    /// <details><summary>A `#UD` occurs under the following conditions:</summary>
    ///
    /// - Execution of any reserved or undefined opcode in any mode.
    /// - Execution of the `UD2` instruction.
    /// - Use of the `LOCK` prefix on an instruction that cannot be locked.
    /// - Use of the `LOCK` prefix on a lockable instruction with a non-memory target location.
    /// - Execution of an instruction with an invalid-operand type.
    /// - Execution of the `SYSENTER` or `SYSEXIT` instructions in long mode.
    /// - Execution of any of the following instructions in 64-bit mode: `AAA`, `AAD`,
    ///   `AAM`, `AAS`, `BOUND`, `CALL` (opcode 9A), `DAA`, `DAS`, `DEC`, `INC`, `INTO`,
    ///   `JMP` (opcode EA), `LDS`, `LES`, `POP` (`DS`, `ES`, `SS`), `POPA`, `PUSH` (`CS`,
    ///   `DS`, `ES`, `SS`), `PUSHA`, `SALC`.
    /// - Execution of the `ARPL`, `LAR`, `LLDT`, `LSL`, `LTR`, `SLDT`, `STR`, `VERR`, or
    ///   `VERW` instructions when protected mode is not enabled, or when virtual-8086 mode
    ///   is enabled.
    /// - Execution of any legacy SSE instruction when `CR4.OSFXSR` is cleared to 0.
    /// - Execution of any SSE instruction (uses `YMM`/`XMM` registers), or 64-bit media
    /// instruction (uses `MMXTM` registers) when `CR0.EM` = 1.
    /// - Execution of any SSE floating-point instruction (uses `YMM`/`XMM` registers) that
    /// causes a numeric exception when `CR4.OSXMMEXCPT` = 0.
    /// - Use of the `DR4` or `DR5` debug registers when `CR4.DE` = 1.
    /// - Execution of `RSM` when not in `SMM` mode.
    ///
    /// </details>
    ///
    /// The saved instruction pointer points to the instruction that caused the `#UD`.
    ///
    /// The vector number of the `#UD` exception is 6.
    pub invalid_opcode: InterruptEntry<HandlerFunc>,

    /// A device not available exception (`#NM`) occurs under any of the following conditions:
    ///
    /// <details>
    ///
    /// - An `FWAIT`/`WAIT` instruction is executed when `CR0.MP=1` and `CR0.TS=1`.
    /// - Any x87 instruction other than `FWAIT` is executed when `CR0.EM=1`.
    /// - Any x87 instruction is executed when `CR0.TS=1`. The `CR0.MP` bit controls whether the
    ///   `FWAIT`/`WAIT` instruction causes an `#NM` exception when `TS=1`.
    /// - Any 128-bit or 64-bit media instruction when `CR0.TS=1`.
    ///
    /// </details>
    ///
    /// The saved instruction pointer points to the instruction that caused the `#NM`.
    ///
    /// The vector number of the `#NM` exception is 7.
    pub device_not_available: InterruptEntry<HandlerFunc>,

    /// A double fault (`#DF`) exception can occur when a second exception occurs during
    /// the handling of a prior (first) exception or interrupt handler.
    ///
    /// <details>
    ///
    /// Usually, the first and second exceptions can be handled sequentially without
    /// resulting in a `#DF`. In this case, the first exception is considered _benign_, as
    /// it does not harm the ability of the processor to handle the second exception. In some
    /// cases, however, the first exception adversely affects the ability of the processor to
    /// handle the second exception. These exceptions contribute to the occurrence of a `#DF`,
    /// and are called _contributory exceptions_. The following exceptions are contributory:
    ///
    /// - Invalid-TSS Exception
    /// - Segment-Not-Present Exception
    /// - Stack Exception
    /// - General-Protection Exception
    ///
    /// A double-fault exception occurs in the following cases:
    ///
    /// - If a contributory exception is followed by another contributory exception.
    /// - If a divide-by-zero exception is followed by a contributory exception.
    /// - If a page  fault is followed by another page fault or a contributory exception.
    ///
    /// If a third interrupting event occurs while transferring control to the `#DF` handler,
    /// the processor shuts down.
    ///
    /// </details>
    ///
    /// The returned error code is always zero. The saved instruction pointer is undefined,
    /// and the program cannot be restarted.
    ///
    /// The vector number of the `#DF` exception is 8.
    pub double_fault: InterruptEntry<DivergingHandlerFuncErrCode>,

    /// This interrupt vector is reserved. It is for a discontinued exception originally used
    /// by processors that supported external x87-instruction coprocessors. On those processors,
    /// the exception condition is caused by an invalid-segment or invalid-page access on an
    /// x87-instruction coprocessor-instruction operand. On current processors, this condition
    /// causes a general-protection exception to occur.
    coprocessor_segment_overrun: InterruptEntry<HandlerFunc>,

    /// An invalid TSS exception (`#TS`) occurs only as a result of a control transfer through
    /// a gate descriptor that results in an invalid stack-segment reference using an `SS`
    /// selector in the TSS.
    ///
    /// The returned error code is the `SS` segment selector. The saved instruction pointer
    /// points to the control-transfer instruction that caused the `#TS`.
    ///
    /// The vector number of the `#TS` exception is 10.
    pub invalid_tss: InterruptEntry<HandlerFuncErrCode>,

    /// An segment-not-present exception (`#NP`) occurs when an attempt is made to load a
    /// segment or gate with a clear present bit.
    ///
    /// The returned error code is the segment-selector index of the segment descriptor
    /// causing the `#NP` exception. The saved instruction pointer points to the instruction
    /// that loaded the segment selector resulting in the `#NP`.
    ///
    /// The vector number of the `#NP` exception is 11.
    pub segment_not_present: InterruptEntry<HandlerFuncErrCode>,

    /// An stack segment exception (`#SS`) can occur in the following situations:
    ///
    /// - Implied stack references in which the stack address is not in canonical
    ///   form. Implied stack references include all push and pop instructions, and any
    ///   instruction using `RSP` or `RBP` as a base register.
    /// - Attempting to load a stack-segment selector that references a segment descriptor
    ///   containing a clear present bit.
    /// - Any stack access that fails the stack-limit check.
    ///
    /// The returned error code depends on the cause of the `#SS`. If the cause is a cleared
    /// present bit, the error code is the corresponding segment selector. Otherwise, the
    /// error code is zero. The saved instruction pointer points to the instruction that
    /// caused the `#SS`.
    ///
    /// The vector number of the `#NP` exception is 12.
    pub stack_segment_fault: InterruptEntry<HandlerFuncErrCode>,

    /// A general protection fault (`#GP`) can occur in various situations. Common causes include:
    ///
    /// - Executing a privileged instruction while `CPL > 0`.
    /// - Writing a 1 into any register field that is reserved, must be zero (MBZ).
    /// - Attempting to execute an SSE instruction specifying an unaligned memory operand.
    /// - Loading a non-canonical base address into the `GDTR` or `IDTR`.
    /// - Using WRMSR to write a read-only MSR.
    /// - Any long-mode consistency-check violation.
    ///
    /// The returned error code is a segment selector, if the cause of the `#GP` is
    /// segment-related, and zero otherwise. The saved instruction pointer points to
    /// the instruction that caused the `#GP`.
    ///
    /// The vector number of the `#GP` exception is 13.
    pub general_protection_fault: InterruptEntry<HandlerFuncErrCode>,

    /// A page fault (`#PF`) can occur during a memory access in any of the following situations:
    ///
    /// - A page-translation-table entry or physical page involved in translating the memory
    ///   access is not present in physical memory. This is indicated by a cleared present
    ///   bit in the translation-table entry.
    /// - An attempt is made by the processor to load the instruction TLB with a translation
    ///   for a non-executable page.
    /// - The memory access fails the paging-protection checks (user/supervisor, read/write,
    ///   or both).
    /// - A reserved bit in one of the page-translation-table entries is set to 1. A `#PF`
    ///   occurs for this reason only when `CR4.PSE=1` or `CR4.PAE=1`.
    ///
    /// The virtual (linear) address that caused the `#PF` is stored in the `CR2` register.
    /// The saved instruction pointer points to the instruction that caused the `#PF`.
    ///
    /// The page-fault error code is described by the
    /// [`PageFaultErrorCode`](struct.PageFaultErrorCode.html) struct.
    ///
    /// The vector number of the `#PF` exception is 14.
    pub page_fault: InterruptEntry<HandlerFuncErrCode>,

    /// vector nr. 15
    reserved_1: InterruptEntry<HandlerFunc>,

    /// The x87 Floating-Point Exception-Pending exception (`#MF`) is used to handle unmasked x87
    /// floating-point exceptions. In 64-bit mode, the x87 floating point unit is not used
    /// anymore, so this exception is only relevant when executing programs in the 32-bit
    /// compatibility mode.
    ///
    /// The vector number of the `#MF` exception is 16.
    pub x87_floating_point: InterruptEntry<HandlerFunc>,

    /// An alignment check exception (`#AC`) occurs when an unaligned-memory data reference
    /// is performed while alignment checking is enabled. An `#AC` can occur only when CPL=3.
    ///
    /// The returned error code is always zero. The saved instruction pointer points to the
    /// instruction that caused the `#AC`.
    ///
    /// The vector number of the `#AC` exception is 17.
    pub alignment_check: InterruptEntry<HandlerFuncErrCode>,

    /// The machine check exception (`#MC`) is model specific. Processor implementations
    /// are not required to support the `#MC` exception, and those implementations that do
    /// support `#MC` can vary in how the `#MC` exception mechanism works.
    ///
    /// There is no reliable way to restart the program.
    ///
    /// The vector number of the `#MC` exception is 18.
    pub machine_check: InterruptEntry<DivergingHandlerFunc>,

    /// The SIMD Floating-Point Exception (`#XF`) is used to handle unmasked SSE
    /// floating-point exceptions. The SSE floating-point exceptions reported by
    /// the `#XF` exception are (including mnemonics):
    ///
    /// - IE: Invalid-operation exception (also called #I).
    /// - DE: Denormalized-operand exception (also called #D).
    /// - ZE: Zero-divide exception (also called #Z).
    /// - OE: Overflow exception (also called #O).
    /// - UE: Underflow exception (also called #U).
    /// - PE: Precision exception (also called #P or inexact-result exception).
    ///
    /// The saved instruction pointer points to the instruction that caused the `#XF`.
    ///
    /// The vector number of the `#XF` exception is 19.
    pub simd_floating_point: InterruptEntry<HandlerFunc>,

    /// vector nr. 20
    pub virtualization: InterruptEntry<HandlerFunc>,

    /// vector nr. 21-29
    reserved_2: [InterruptEntry<HandlerFunc>; 9],

    /// The Security Exception (`#SX`) signals security-sensitive events that occur while
    /// executing the VMM, in the form of an exception so that the VMM may take appropriate
    /// action. (A VMM would typically intercept comparable sensitive events in the guest.)
    /// In the current implementation, the only use of the `#SX` is to redirect external INITs
    /// into an exception so that the VMM may — among other possibilities.
    ///
    /// The only error code currently defined is 1, and indicates redirection of INIT has occurred.
    ///
    /// The vector number of the ``#SX`` exception is 30.
    pub security_exception: InterruptEntry<HandlerFuncErrCode>,

    /// vector nr. 31
    reserved_3: InterruptEntry<HandlerFunc>,

    /// User-defined interrupts can be initiated either by system logic or software. They occur
    /// when:
    ///
    /// - System logic signals an external interrupt request to the processor. The signaling
    ///   mechanism and the method of communicating the interrupt vector to the processor are
    ///   implementation dependent.
    /// - Software executes an `INTn` instruction. The `INTn` instruction operand provides
    ///   the interrupt vector number.
    ///
    /// Both methods can be used to initiate an interrupt into vectors 0 through 255. However,
    /// because vectors 0 through 31 are defined or reserved by the AMD64 architecture,
    /// software should not use vectors in this range for purposes other than their defined use.
    ///
    /// The saved instruction pointer depends on the interrupt source:
    ///
    /// - External interrupts are recognized on instruction boundaries. The saved instruction
    ///   pointer points to the instruction immediately following the boundary where the
    ///   external interrupt was recognized.
    /// - If the interrupt occurs as a result of executing the INTn instruction, the saved
    ///   instruction pointer points to the instruction after the INTn.
    interrupts: [InterruptEntry<HandlerFunc>; 256 - 32],
}

impl InterruptDescriptorTable {
    pub fn new() -> InterruptDescriptorTable {
        InterruptDescriptorTable {
            divide_error: InterruptEntry::default(),
            debug: InterruptEntry::default(),
            non_maskable_interrupt: InterruptEntry::default(),
            breakpoint: InterruptEntry::default(),
            overflow: InterruptEntry::default(),
            bound_range_exceeded: InterruptEntry::default(),
            invalid_opcode: InterruptEntry::default(),
            device_not_available: InterruptEntry::default(),
            double_fault: InterruptEntry::default(),
            coprocessor_segment_overrun: InterruptEntry::default(),
            invalid_tss: InterruptEntry::default(),
            segment_not_present: InterruptEntry::default(),
            stack_segment_fault: InterruptEntry::default(),
            general_protection_fault: InterruptEntry::default(),
            page_fault: InterruptEntry::default(),
            reserved_1: InterruptEntry::default(),
            x87_floating_point: InterruptEntry::default(),
            alignment_check: InterruptEntry::default(),
            machine_check: InterruptEntry::default(),
            simd_floating_point: InterruptEntry::default(),
            virtualization: InterruptEntry::default(),
            reserved_2: [InterruptEntry::default(); 9],
            security_exception: InterruptEntry::default(),
            reserved_3: InterruptEntry::default(),
            interrupts: [InterruptEntry::default(); 256 - 32],
        }
    }

    pub fn update(&'static self) {
        let pidt = x86::DescriptorTablePointer {
            base: (self as *const _ as u32),
            limit: (size_of::<Self>() - 1) as u16,
        };
        x86::lidt(&pidt);
    }
}

impl Default for InterruptDescriptorTable {
    fn default() -> Self {
        Self::new()
    }
}

impl Index<usize> for InterruptDescriptorTable {
    type Output = InterruptEntry<HandlerFunc>;

    fn index(&self, index: usize) -> &Self::Output {
        match index {
            0 => &self.divide_error,
            1 => &self.debug,
            2 => &self.non_maskable_interrupt,
            3 => &self.breakpoint,
            4 => &self.overflow,
            5 => &self.bound_range_exceeded,
            6 => &self.invalid_opcode,
            7 => &self.device_not_available,
            9 => &self.coprocessor_segment_overrun,
            16 => &self.x87_floating_point,
            19 => &self.simd_floating_point,
            20 => &self.virtualization,
            i @ 32..=255 => &self.interrupts[i - 32],
            i @ 15 | i @ 31 | i @ 21..=29 => panic!("entry {} is reserved", i),
            i @ 8 | i @ 10..=14 | i @ 17 | i @ 30 => {
                panic!("entry {} is an exception with error code", i)
            }
            i @ 18 => panic!("entry {} is an diverging exception (must not return)", i),
            i => panic!("no entry with index {}", i),
        }
    }
}

impl IndexMut<usize> for InterruptDescriptorTable {
    /// Returns a mutable reference to the IDT entry with the specified index.
    ///
    /// Panics if index is outside the IDT (i.e. greater than 255) or if the entry is an
    /// exception that pushes an error code (use the struct fields for accessing these entries).
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        match index {
            0 => &mut self.divide_error,
            1 => &mut self.debug,
            2 => &mut self.non_maskable_interrupt,
            3 => &mut self.breakpoint,
            4 => &mut self.overflow,
            5 => &mut self.bound_range_exceeded,
            6 => &mut self.invalid_opcode,
            7 => &mut self.device_not_available,
            9 => &mut self.coprocessor_segment_overrun,
            16 => &mut self.x87_floating_point,
            19 => &mut self.simd_floating_point,
            20 => &mut self.virtualization,
            i @ 32..=255 => &mut self.interrupts[i - 32],
            i @ 15 | i @ 31 | i @ 21..=29 => panic!("entry {} is reserved", i),
            i @ 8 | i @ 10..=14 | i @ 17 | i @ 30 => {
                panic!("entry {} is an exception with error code", i)
            }
            i @ 18 => panic!("entry {} is an diverging exception (must not return)", i),
            i => panic!("no entry with index {}", i),
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct InterruptEntry<T> {
    pointer_low: u16,
    gdt_selector: x86::SegmentSelector,
    options: InterruptOption,
    pointer_middle: u16,
    phantom: PhantomData<T>,
}

impl<T> InterruptEntry<T> {
    fn pointer(&self) -> usize {
        ((self.pointer_middle as usize) << 16) | (self.pointer_low as usize)
    }

    pub fn set_option(&mut self, opt: InterruptOption) -> &mut Self {
        unsafe { addr_of_mut!(self.options).write_unaligned(opt) };
        self
    }

    pub fn set_handle_addr(&mut self, pointer: usize) -> &mut Self {
        self.pointer_low = (pointer & 0xffff) as u16;
        self.pointer_middle = (pointer >> 16) as u16;
        self.gdt_selector = x86::get_cs();
        self.set_option(InterruptOption::new(false, PrivilegeLevel::Ring0, true));
        self
    }
}

impl InterruptEntry<HandlerFunc> {
    pub fn set_handle_fn(&mut self, handler: HandlerFunc) -> &mut Self {
        self.set_handle_addr(handler as usize)
    }
}

impl InterruptEntry<HandlerFuncErrCode> {
    pub fn set_handle_fn(&mut self, handler: HandlerFuncErrCode) -> &mut Self {
        self.set_handle_addr(handler as usize)
    }
}

impl InterruptEntry<DivergingHandlerFunc> {
    pub fn set_handle_fn(&mut self, handler: DivergingHandlerFunc) -> &mut Self {
        self.set_handle_addr(handler as usize)
    }
}

impl InterruptEntry<DivergingHandlerFuncErrCode> {
    pub fn set_handle_fn(&mut self, handler: DivergingHandlerFuncErrCode) -> &mut Self {
        self.set_handle_addr(handler as usize)
    }
}

impl<T> Default for InterruptEntry<T> {
    fn default() -> Self {
        Self {
            pointer_low: 0,
            gdt_selector: x86::SegmentSelector(0),
            options: InterruptOption::default(),
            pointer_middle: 0,
            phantom: PhantomData,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct InterruptOption(u16);

impl InterruptOption {
    fn is_trap(&self) -> bool {
        self.0.get_bit(8)
    }

    fn set_trap(&mut self, val: bool) -> &mut Self {
        self.0.set_bit(8, val);
        self
    }

    fn privilege_level(&self) -> u16 {
        self.0.get_bits(13..=14)
    }

    fn set_privilege_level(&mut self, val: u16) -> &mut Self {
        self.0.set_bits(13..=14, val);
        self
    }

    fn is_present(&self) -> bool {
        self.0.get_bit(15)
    }

    fn set_present(&mut self, val: bool) -> &mut Self {
        self.0.set_bit(15, val);
        self
    }

    fn new(is_trap: bool, privilege_level: PrivilegeLevel, is_present: bool) -> Self {
        *InterruptOption(0b0000111000000000)
            .set_trap(is_trap)
            .set_privilege_level(privilege_level as u16)
            .set_present(is_present)
    }
}

impl Default for InterruptOption {
    fn default() -> Self {
        Self::new(false, PrivilegeLevel::Ring0, false)
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ExceptionStackFrame {
    pub instruction_pointer: u32,
    pub code_segment: u16,
    pub cpu_flags: u32,
    pub stack_pointer: u32,
    pub stack_segment: u16,
}

pub fn end_of_interrupt() {
    x86::outb(0x20, 0x20);
}
