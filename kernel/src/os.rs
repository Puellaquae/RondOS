use crate::{
    handler, handler_with_code, print, println,
    x86::{
        interrupt::{ExceptionStackFrame, Idt, Interrupts, PageFaultErrorCode},
        pic::{end_of_interrupt, pic_init, pit_configure_channel},
        reg::{get_cr2, inb, outb},
    }, keyboard,
};

const TIMER_FREQ: u32 = 200;

pub static mut OS: Os = Os(None);
pub struct Os(Option<RondOs>);

impl Os {
    pub fn init(&mut self) -> &mut Self {
        *self = Os(Some(RondOs::new()));
        self
    }

    pub fn get(&self) -> &RondOs {
        self.0.as_ref().unwrap()
    }

    pub fn get_mut(&mut self) -> &mut RondOs {
        self.0.as_mut().unwrap()
    }
}
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

    pub fn keyboard_init(&mut self) {
        self.idt.set_handler(0x21, handler!(keyboard_handler));
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

extern "C" fn timer_handler(_f: &ExceptionStackFrame) {
    static mut TICKS: u64 = 0;
    unsafe {
        TICKS += 1;
    }
    if unsafe { TICKS % (TIMER_FREQ as u64) == 0 } {
        // print!(".");
    }
    end_of_interrupt(0x20);
}

extern "C" fn keyboard_handler(_f: &ExceptionStackFrame) {
    let scancode = inb(0x60);
    if let Some(ch) = keyboard::scancode_to_char(scancode) {
        print!("{}", ch);
    }
    let mut p61 = inb(0x61);
    p61 |= 0x80;
    outb(0x61, p61);
    p61 &= 0x7f;
    outb(0x61, p61);
    end_of_interrupt(0x20);
}
