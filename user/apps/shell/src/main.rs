//! `shell` — a line-oriented command interpreter (P3).
//!
//! It is the first program that uses both new device capabilities: the console
//! (write text, which the kernel mirrors to the framebuffer and the serial
//! port) and the keyboard (read key bytes).  The terminal is deliberately
//! simple — printable bytes, backspace, Enter, Ctrl+C — and the built-ins are
//! the smallest set that makes the system inspectable from a real machine.

#![no_std]
#![no_main]

use rondos_abi::{Handle, ObjKind, StartupBlock};
use rondos_rt::{close, open_file, print, println, readdir, spawn, wait, write_file};

const PROMPT: &[u8] = b"rondos> ";
const MAX_LINE: usize = 128;

struct Term {
    con: Handle,
    kbd: Handle,
    line: [u8; MAX_LINE],
    len: usize,
}

impl Term {
    fn puts(&self, s: &str) {
        let _ = write_file(self.con, s.as_bytes());
    }

    fn put_bytes(&self, b: &[u8]) {
        let _ = write_file(self.con, b);
    }

    /// Write one formatted line straight to the console.
    ///
    /// The shell is a terminal, so its own messages must not go through
    /// `println!`/`sys_log`: that path mirrors them with a kernel `user: `
    /// prefix and a forced newline, which is exactly the "log line" look a
    /// shell should not have.
    fn line(&self, args: core::fmt::Arguments) {
        struct Sink(Handle);
        impl core::fmt::Write for Sink {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                let _ = write_file(self.0, s.as_bytes());
                Ok(())
            }
        }
        let mut sink = Sink(self.con);
        let _ = core::fmt::write(&mut sink, args);
        self.puts("\n");
    }

    /// Read one key, echoing it.  Returns `None` on error.
    fn key(&self) -> Option<u8> {
        let mut b = [0u8; 1];
        loop {
            match rondos_rt::read(self.kbd, &mut b) {
                Ok(1) => {
                    // Swallow the rest of an escape sequence (arrow keys).
                    if b[0] == 0x1b {
                        let mut skip = [0u8; 8];
                        let _ = rondos_rt::read(self.kbd, &mut skip);
                        continue;
                    }
                    return Some(b[0]);
                }
                Ok(_) => continue,
                // The kernel returns NotReady when nothing was typed within
                // its wait window; that is not an error, just idle.
                Err(rondos_abi::Status::NotReady) => continue,
                Err(_) => return None,
            }
        }
    }

    fn prompt(&mut self) {
        self.len = 0;
        self.put_bytes(PROMPT);
    }

    /// Read a line with echo and backspace; returns its length.
    fn read_line(&mut self) -> Option<usize> {
        loop {
            let c = self.key()?;
            match c {
                b'\r' | b'\n' => {
                    self.puts("\n");
                    return Some(self.len);
                }
                0x08 | 0x7f => {
                    if self.len > 0 {
                        self.len -= 1;
                        self.put_bytes(b"\x08 \x08");
                    }
                }
                0x03 => {
                    self.puts("^C\n");
                    self.prompt();
                }
                b if b >= 0x20 && b < 0x7f => {
                    if self.len < MAX_LINE {
                        self.line[self.len] = b;
                        self.len += 1;
                        self.put_bytes(&[b]);
                    }
                }
                _ => {}
            }
        }
    }
}

fn parse(line: &[u8]) -> (&[u8], &[u8]) {
    let mut i = 0;
    while i < line.len() && line[i] == b' ' {
        i += 1;
    }
    let start = i;
    while i < line.len() && line[i] != b' ' {
        i += 1;
    }
    let cmd = &line[start..i];
    while i < line.len() && line[i] == b' ' {
        i += 1;
    }
    (cmd, &line[i..])
}

fn builtin(t: &Term, root: Handle, cmd: &[u8], arg: &[u8]) -> bool {
    match cmd {
        b"" => {}
        b"help" => {
            t.puts("commands: help echo ls cat run clear uptime exit\n");
        }
        b"echo" => {
            t.put_bytes(arg);
            t.puts("\n");
        }
        b"ls" => {
            let mut i = 0u32;
            while let Ok(e) = readdir(root, i) {
                t.put_bytes(e.name());
                t.puts("\n");
                i += 1;
            }
            if i == 0 {
                t.puts("(empty)\n");
            }
        }
        b"cat" => {
            if arg.is_empty() {
                t.puts("usage: cat <path>\n");
            } else {
                match open_file(root, arg) {
                    Ok(h) => {
                        let mut buf = [0u8; 128];
                        loop {
                            match rondos_rt::read(h, &mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => t.put_bytes(&buf[..n]),
                            }
                        }
                        let _ = close(h);
                    }
                    Err(e) => t.line(format_args!("cat: {:?}", e)),
                }
            }
        }
        b"run" => {
            if arg.is_empty() {
                t.puts("usage: run <path>\n");
            } else {
                match open_file(root, arg).and_then(spawn) {
                    Ok(child) => {
                        match wait(&[child], 10_000_000_000) {
                            Ok(w) => t.line(format_args!("shell: child finished (reason {})", w.reason)),
                            Err(e) => t.line(format_args!("shell: wait failed: {:?}", e)),
                        }
                        let _ = close(child);
                    }
                    Err(e) => t.line(format_args!("run: {:?}", e)),
                }
            }
        }
        b"clear" => {
            t.put_bytes(b"\x0c");
        }
        b"uptime" => {
            t.line(format_args!("shell: {} ms since boot", rondos_rt::now_ns() / 1_000_000));
        }
        b"exit" => {
            t.puts("shell: bye\n");
            return false;
        }
        other => {
            t.line(format_args!(
                "shell: unknown command {:?}",
                core::str::from_utf8(other).unwrap_or("?")
            ));
        }
    }
    true
}

#[no_mangle]
pub extern "C" fn app_main(block: &StartupBlock) -> i32 {
    let Some(con) = rondos_rt::cap_with(ObjKind::Device, rondos_abi::rights::WRITE) else {
        println!("shell: no console capability");
        return 1;
    };
    let Some(kbd) = rondos_rt::cap_with(ObjKind::Device, rondos_abi::rights::READ) else {
        println!("shell: no keyboard capability");
        return 2;
    };
    let Some(root) = block.cap(ObjKind::Dir).map(|c| Handle(c.handle)) else {
        println!("shell: no root capability");
        return 3;
    };

    let mut t = Term {
        con,
        kbd,
        line: [0; MAX_LINE],
        len: 0,
    };
    // ASCII only: the framebuffer font is 8x16 VGA and renders every non-ASCII
    // byte as '?', so an em-dash here showed up as "???" on a real screen.
    t.puts("\nRondOS shell - type 'help' for commands\n");
    t.prompt();
    loop {
        let Some(n) = t.read_line() else {
            t.puts("shell: input device gone\n");
            return 4;
        };
        let (cmd, arg) = parse(&t.line[..n]);
        let keep_going = builtin(&t, root, cmd, arg);
        if !keep_going {
            break;
        }
        t.prompt();
    }
    // Stay alive: PID 2 exiting would leave nothing supervising anything yet.
    loop {
        rondos_rt::sleep_ns(1_000_000_000);
    }
}
