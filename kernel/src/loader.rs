pub fn get_kernel_size() -> u32 {
    unsafe { *(0xc0020200 as *const u32) }
}

pub const KERNEL_VADDR_BASE: u32 = 0xc0000000;
pub const KERNEL_STACK_PADDR: u32 = 0x7c00;
pub const KERNEL_PLACE_BEGIN_PADDR: u32 = 0x20000;

pub const SEGMENT_KERNEL_CODE: u16 = 0x8;