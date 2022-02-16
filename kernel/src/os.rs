use crate::{
    handler, handler_with_code, print, println,
    x86::{
        interrupt::{ExceptionStackFrame, Idt, Interrupts, PageFaultErrorCode},
        pic::{end_of_interrupt, pic_init, pit_configure_channel},
        reg::get_cr2,
    },
};

const TIMER_FREQ: u32 = 200;

pub struct RondOs {
    pub idt: Idt,
}

impl RondOs {
    pub fn new() -> RondOs {
        RondOs { idt: Idt::new() }
    }

    pub fn intr_init(&mut self) {
        pic_init();

        self.idt.set_handler(
            Interrupts::BreakpointException as u8,
            handler!(breakpoint_handler),
        );
        self.idt.set_handler(
            Interrupts::PageFaultException as u8,
            handler_with_code!(page_fault_handler),
        );
        self.idt.set_handler(
            Interrupts::DoubleFaultException as u8,
            handler_with_code!(double_handler),
        );
        self.idt.load();
    }

    pub fn timer_init(&mut self) {
        pit_configure_channel(0, 2, TIMER_FREQ);
        self.idt.set_handler(0x20, handler!(timer_handler));
    }
}

extern "C" fn breakpoint_handler(f: &ExceptionStackFrame) {
    println!("EXCEPTION: BreakPoint At 0x{:x}", f.instruction_pointer);
}

extern "C" fn page_fault_handler(f: &ExceptionStackFrame) {
    println!(
        "EXCEPTION: Page Fault {:?} When Access 0x{:x}",
        PageFaultErrorCode::from_bits(f.error_code).unwrap(),
        get_cr2()
    );
}

extern "C" fn double_handler(f: &ExceptionStackFrame) {
    println!("EXCEPTION: Double Fault");
    println!("{:#?}", f);
}

static mut TICKS: u64 = 0;

extern "C" fn timer_handler(_f: &ExceptionStackFrame) {
    unsafe {
        TICKS += 1;
    }
    if unsafe { TICKS % (TIMER_FREQ as u64) == 0 } {
        print!(".");
    }
    end_of_interrupt(0x20);
}
