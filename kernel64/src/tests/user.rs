//! User-process test programs (P0 blobs and the P1 boot-tar programs).

use crate::arch::x86_64::halt_loop;
use crate::exec;
use crate::fs;
use crate::mm;
use crate::mm::vm::{PAGE_USER_RW, PAGE_USER_RX};
use crate::proc::{self, ExitStatus};
use crate::syscall;
use crate::thread;

use super::{report, Case, Verdict};
use crate::serial_println;

pub static CASES: &[Case] = &[
    Case { name: "handle-table", run: handle_table },
    Case { name: "user-p0", run: user_p0 },
    Case { name: "elf-loader", run: elf_loader },
    Case { name: "shell", run: shell },
];

/// End-to-end P3: start `bin/shell`, type into the keyboard queue and require
/// its answers on the framebuffer console.
fn shell() -> Verdict {
    let pid = match exec::spawn_path(b"/bin/shell") {
        Ok(p) => p,
        Err(e) => {
            serial_println!("shell: cannot spawn bin/shell: {:?}", e);
            return Verdict::Fail;
        }
    };
    // Let it print its banner and prompt.
    for _ in 0..50 {
        if crate::io::fb::contains("rondos> ") {
            break;
        }
        thread::sleep(10);
    }
    let mut ok = crate::io::fb::contains("rondos> ");

    // `echo`: proves keys reach the shell and its output reaches the console.
    // Two characters are typed by mistake and then erased: this also proves the
    // echo is in-place (no `user: ` prefix, no forced newline) and that
    // backspace actually blanks the cell it stepped over.
    crate::io::input::inject(b"echo hello-from-shellZZ\x08\x08\r");
    ok &= wait_for_fb("rondos> echo hello-from-shell", 200);
    ok &= wait_for_fb("hello-from-shell", 200);
    ok &= !crate::io::fb::contains("hello-from-shellZZ");

    // `ls`: proves the built-in reaches the file system.
    crate::io::input::inject(b"ls\r");
    ok &= wait_for_fb("bin/selftest", 200);

    // `run`: spawn a child program from the shell and reap it.
    crate::io::input::inject(b"run /bin/chello\r");
    ok &= wait_for_fb("C program", 400);

    if !ok {
        serial_println!("shell: console state:\n{}", crate::io::fb::row(0).len());
    }
    proc::kill(pid).ok();
    thread::sleep(50);
    if ok {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}

fn wait_for_fb(needle: &str, ticks: u64) -> bool {
    for _ in 0..ticks {
        if crate::io::fb::contains(needle) {
            return true;
        }
        thread::sleep(10);
    }
    serial_println!("shell: never saw {:?}", needle);
    false
}

/// Handle-table unit checks (encoding, generations, rights, VMA ranges).
fn handle_table() -> Verdict {
    if proc::selftest_handles() {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}

/// Two hand-built ring3 processes: one prints and exits, one faults.  Reports
/// its own sub-results (spawn/process/fault-isolation/reclaim).
fn user_p0() -> Verdict {
    let free_before = mm::page_alloc().free_pages();
    let hello = spawn_user(
        "hello",
        user_hello,
        user_hello_end,
        0x0000_6000_0000_0000,
    );
    let fault = spawn_user(
        "fault",
        user_fault,
        user_fault_end,
        0x0000_7000_0000_0000,
    );
    let (hello_pid, fault_pid) = match (hello, fault) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            report("user-spawn", false);
            halt_loop()
        }
    };
    report("user-spawn", true);

    let hello_status = wait_for_exit(hello_pid, 4000);
    let fault_status = wait_for_exit(fault_pid, 4000);

    let hello_ok =
        hello_status == Some(ExitStatus::Exited(0)) && syscall::logged_bytes() >= 12;
    if !hello_ok {
        serial_println!(
            "user: hello status {:?}, sys_log accepted {} bytes",
            hello_status,
            syscall::logged_bytes()
        );
    }
    report("user-process", hello_ok);

    // Give the reaper a few ticks: the *status* is recorded as soon as the
    // process dies, but the slot (and its frames) are freed on the next tick.
    thread::sleep(50);
    let live = proc::table().live();

    let fault_ok = matches!(fault_status, Some(ExitStatus::Fault { vector: 14, .. }));
    if !fault_ok || live != 0 {
        serial_println!("user: fault status {:?}, {} live", fault_status, live);
    }
    report("user-fault-isolation", fault_ok && live == 0);

    // Every frame the two processes owned must be back in the allocator.
    let free_after = mm::page_alloc().free_pages();
    if free_after < free_before {
        serial_println!("user: frames leaked: {} -> {}", free_before, free_after);
    }
    report("user-reclaim", free_after >= free_before);
    Verdict::Reported
}

const USER_STACK_OFF: u64 = 0x0001_0000;
const USER_STACK_LEN: u64 = 0x2000;

fn spawn_user(
    name: &'static str,
    start: unsafe extern "C" fn(),
    end: unsafe extern "C" fn(),
    base: u64,
) -> Option<u32> {
    let len = end as usize - start as usize;
    let code = unsafe { core::slice::from_raw_parts(start as *const u8, len) };
    let pid = proc::table().create(0)?;
    let root = {
        let p = proc::table().get(pid)?;
        p.map_blob(base, code, PAGE_USER_RX).ok()?;
        p.map_anon(base + USER_STACK_OFF, USER_STACK_LEN, PAGE_USER_RW)
            .ok()?;
        p.root()
    };
    let stack_top = base + USER_STACK_OFF + USER_STACK_LEN - 16;
    let tid = thread::thread_create_user(pid, root, name, base, stack_top, 0)?;
    proc::table().attach_thread(pid, tid).ok()?;
    serial_println!(
        "user: '{}' pid {} tid {} code {:#x} stack {:#x}",
        name,
        pid,
        tid,
        base,
        stack_top
    );
    Some(pid)
}

/// Poll until `pid` leaves `Running` (the outcome survives reaping).
fn wait_for_exit(pid: u32, timeout_ms: u64) -> Option<ExitStatus> {
    let deadline = thread::ticks() + (timeout_ms + thread::TICK_MS - 1) / thread::TICK_MS;
    loop {
        if let Some(st) = proc::table().status_of(pid) {
            if st != ExitStatus::Running {
                return Some(st);
            }
        }
        if thread::ticks() >= deadline {
            return None;
        }
        thread::sleep(5);
    }
}

// Two tiny ring3 programs.  They are *not* the P1 ELF path: they exist so P0
// can prove process lifetime, syscalls and fault containment end to end.

core::arch::global_asm!(
    ".global user_hello",
    "user_hello:",
    // sys_info into a stack buffer (proves copy_to_user + VMA write check).
    "  lea rdi, [rsp - 128]",
    "  mov dword ptr [rdi], 112",       // hdr.size = sizeof(Info)
    "  mov dword ptr [rdi + 4], 1",     // hdr.version
    "  mov rax, 0x00",                  // SyscallId::Info
    "  int 0x80",
    "  test rax, rax",
    "  jnz 2f",                         // -> exit(1)
    "  cmp dword ptr [rdi + 8], 1",     // abi_version
    "  jne 3f",                         // -> exit(2)
    // sys_clock_gettime(Monotonic) -> rdx = ns since boot
    "  mov rax, 0x15",
    "  xor edi, edi",
    "  int 0x80",
    "  test rax, rax",
    "  jnz 4f",                         // -> exit(3)
    "  test rdx, rdx",
    "  jz 4f",
    // sys_yield()
    "  mov rax, 0x13",
    "  int 0x80",
    "  test rax, rax",
    "  jnz 5f",                         // -> exit(4)
    // sys_log(Error, msg, 12)
    "  mov rax, 0x16",
    "  xor edi, edi",
    "  lea rsi, [rip + user_hello_msg]",
    "  mov rdx, 12",
    "  int 0x80",
    "  cmp rdx, 12",                    // value, not status
    "  jne 6f",                         // -> exit(5)
    // sys_exit(0)
    "  mov rax, 0x10",
    "  xor edi, edi",
    "  int 0x80",
    "  ud2",
    "2: mov edi, 1",
    "  jmp 7f",
    "3: mov edi, 2",
    "  jmp 7f",
    "4: mov edi, 3",
    "  jmp 7f",
    "5: mov edi, 4",
    "  jmp 7f",
    "6: mov edi, 5",
    "7: mov rax, 0x10",
    "  int 0x80",
    "  ud2",
    "user_hello_msg:",
    "  .ascii \"hello ring3\\n\"",
    ".global user_hello_end",
    "user_hello_end:",
);

core::arch::global_asm!(
    ".global user_fault",
    "user_fault:",
    "  mov rax, 0x1234",            // unmapped in the process address space
    "  mov byte ptr [rax], 0x5a",
    "  ud2",
    ".global user_fault_end",
    "user_fault_end:",
);

extern "C" {
    fn user_hello();
    fn user_hello_end();
    fn user_fault();
    fn user_fault_end();
}

fn elf_loader() -> Verdict {
    test_user_images()
}

fn test_user_images() -> Verdict {
    let Some(tar) = fs::TarFs::root() else {
        serial_println!("exec: no boot tar (BootInfo.initrd_len == 0)");
        return Verdict::Fail;
    };
    serial_println!(
        "exec: boot tar {} bytes, {} file(s)",
        tar.len(),
        tar.count()
    );
    for e in tar.entries() {
        serial_println!(
            "  {} ({} bytes)",
            core::str::from_utf8(e.name).unwrap_or("?"),
            e.len
        );
    }

    let free_before = mm::page_alloc().free_pages();

    // The boot tar is the root FS and `bin/init` is the first process.  It
    // spawns `bin/crash` and `bin/spin` itself through sys_open/sys_spawn, so
    // this one spawn exercises the whole P1b path.
    let init_pid = match exec::spawn_path(b"bin/selftest") {
        Ok(p) => p,
        Err(e) => {
            serial_println!("exec: cannot spawn bin/selftest: {:?}", e);
            return Verdict::Fail;
        }
    };
    let init_status = wait_for_exit(init_pid, 8000);

    thread::sleep(50);
    let live = proc::table().live();
    let free_after = mm::page_alloc().free_pages();
    let ok = init_status == Some(ExitStatus::Exited(0)) && live == 0 && free_after >= free_before;
    if !ok {
        serial_println!(
            "exec: init {:?}, {} live, frames {} -> {}",
            init_status,
            live,
            free_before,
            free_after
        );
    }
    if ok {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}
