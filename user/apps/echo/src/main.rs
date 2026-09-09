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
    match rondos_rt::chan_send(chan, &buf[..n]) {
        Ok(_) => {
            rondos_rt::println!("echo: returned {} bytes", n);
            0
        }
        Err(e) => {
            rondos_rt::println!("echo: send failed: {:?}", e);
            3
        }
    }
}
