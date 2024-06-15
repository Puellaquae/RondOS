#![allow(dead_code)]

pub enum ThreadState {
    Running,
    Ready,
    Blocking,
    Dying
}

#[repr(C)]
pub struct Thread {
    stack: *mut u8,
    pid: u32,
    state: ThreadState
}