//! `echo` — receives one message on the channel capability its parent delegated
//! and sends it straight back (P2's channel round-trip).

#![no_std]
#![no_main]

use rondos_abi::{ObjKind, StartupBlock};

#[no_mangle]
pub extern "C" fn app_main(block: &StartupBlock) -> i32 {
    let Some(chan) = block.cap(ObjKind::Chan).map(|c| rondos_abi::Handle(c.handle)) else {
        rondos_rt::println!("echo: no channel capability");
        return 1;
    };
    let mut buf = [0u8; 64];
    let n = match rondos_rt::chan_recv(chan, &mut buf) {
        Ok(n) => n,
        Err(e) => {
            rondos_rt::println!("echo: recv failed: {:?}", e);
            return 2;
        }
    };
    // Reply with a prefix: the parent must be able to tell the child's answer
    // apart from its own message (a channel is a pipe, not a shared mailbox).
    let mut reply = [0u8; 80];
    reply[..5].copy_from_slice(b"echo:");
    reply[5..5 + n].copy_from_slice(&buf[..n]);
    match rondos_rt::chan_send(chan, &reply[..5 + n]) {
        Ok(m) => {
            rondos_rt::println!("echo: returned {} bytes", m);
            0
        }
        Err(e) => {
            rondos_rt::println!("echo: send failed: {:?}", e);
            3
        }
    }
}
