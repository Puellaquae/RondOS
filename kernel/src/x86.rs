use core::arch::asm;

pub fn inb(port: u16) -> u8 {
    let mut data: u8;
    unsafe {
        asm!("in al, dx", in("dx") port, out("al") data);
    }
    data
}

pub fn outb(port: u16, data: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") data);
    }
}

pub fn cr3() -> u32 {
    let mut addr: u32;
    unsafe {
        asm!("mov eax, cr3", out("eax") addr);
    }
    return addr;
}
