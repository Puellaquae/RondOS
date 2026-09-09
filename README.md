# RondOS

一个自制操作系统。当前正在从 i686/BIOS 迁移到 **x86-64 + UEFI**，
目标是一个**外观是 DOS / Windows 3.1、内部技术完全现代**的桌面系统
（capability + handle、事前冻结的 syscall ABI、用户态合成器、声明式 UI）。

主线是 `kernel64/`；原来的 i686 内核与 NASM 两段式 loader 保存在
**`legacy-i686` 分支**（`git switch legacy-i686` 可回到迁移前的状态）。

## 目录

- `kernel64/`  x86-64 内核，目标 `x86_64-unknown-none`，链接到 `0xFFFFFFFF80000000`
  - `src/arch/x86_64/`  端口 I/O、CR/MSR/CPUID、4 级分页（NX + PCD/PWT + 大页拆分）、
    GDT/TSS/per-CPU（`swapgs`）、IDT/统一 `TrapFrame`、8259+8254
  - `src/mm/`           位图页框分配器（physmap 视图）+ 架构无关 VMM 抽象
  - `src/thread/`       内核线程 + 用户线程、抢占式轮转调度、`sleep`/`exit`、`WaitQueue`；
    每个线程带自己的页表根，切换线程即切换地址空间
  - `src/proc/`         `Process`/VMA/`HandleTable`/`ExitStatus`、用户指针校验
  - `src/syscall.rs`    `int 0x80` v1 分发（info/exit/yield/clock/log/sleep + 文件与进程组）
  - `src/fs.rs`         boot tar 只读文件系统（ustar）+ tmpfs 可写层（固定槽位，
    存储是 `.bss` 数组，不占页框）
  - `src/obj.rs`        共享内存对象 + channel（引用计数、全局表）
  - `src/exec.rs`       ELF64 装载、`StartupBlock` + capability、`spawn_path`/`spawn_entry`
  - `src/bootinfo.rs`   版本化引导交接结构（magic/size/version），内核唯一的引导契约
- `boot/uefi/`  UEFI 引导 stub，目标 `x86_64-unknown-uefi`，基于 `uefi-rs`
  - GOP 选 32bpp 模式 → 读 ESP 上的 `\rondos\kernel.elf` → 按 `p_paddr` 装载
    → 填 `BootInfo`（内存图 + framebuffer + initrd）→ 建页表 →
    `ExitBootServices` → 跳内核（`rdi` = `BootInfo` 物理地址）
- `user/`  用户程序工作区（`cargo +nightly`，自定义 target spec）
  - `targets/x86_64-rondos.json`  用户态 target：`os = "rondos"`、开 SSE2、关红区
  - `user.ld`  固定地址（0x400000）ELF64 链接脚本，段页对齐、`.text` R+X、数据 RW+NX
  - `lib/rondos-abi/`  内核与用户程序共享的 ABI crate（syscall 号、`Status`、
    handle 编码、`Info` 等结构体 + 编译期布局断言）
  - `lib/rondos-rt/`  `_start`、`panic`、`println!`（走 `sys_log`）
  - `apps/init/`、`apps/crash/`、`apps/spin/`、`apps/echo/`  首批用户程序：`init` 用
    root 能力打开并 `spawn` 它们，`wait`/`proc_status` 拿结果、`kill` 长跑的那个、
    映射共享内存回读、建 channel 并把一端委托给 `echo` 做往返
- `user/c/`  最小 C 支持：`crt0.S` + `rondos.h`（手写的 ABI 头，带 `_Static_assert`）
  + `hello.c`，用宿主 `gcc -ffreestanding -nostdlib` 直接编出用户态 ELF
- `tools/mktar.py`  确定性 ustar 打包器（生成 `build/boot.tar`）
- `files/`      启动 tar 镜像内容（将来的用户程序与资源）
- `docs/user-mode-design.md`  用户态完整设计（迁移、ring 3、编译支持、可执行文件格式、
  冻结的 syscall ABI、Win3.1 复古桌面）
- `Makefile`    构建脚本

## 依赖

```bash
# Debian/Ubuntu
sudo apt install qemu-system-x86 ovmf make

# Rust nightly + rust-src（no_std 内核 + build-std 需要）
rustup install nightly
rustup component add rust-src --toolchain nightly
```

## 编译运行

```bash
make            # 构建用户程序 + 内核 + UEFI stub，打包 boot.tar，组装 build/esp/
make run        # 用 QEMU + OVMF 启动 build/esp/（串口输出到 stdio）
make test       # 无头启动，检查冒烟测试结果
make release    # release 构建
make clean
```

`make run` 把 `build/esp/` 当作 FAT 盘直接喂给 QEMU（`-drive file=fat:rw:...`），
所以不需要 `mkfs.vfat`/`mtools`：

```
build/esp/EFI/BOOT/BOOTX64.EFI   UEFI stub
build/esp/rondos/kernel.elf      x86-64 内核
build/esp/rondos/boot.tar        ustar 镜像（init.elf / crash.elf）
```

OVMF 路径用 `OVMF=/path/to/OVMF.fd` 覆盖（默认 `/usr/share/ovmf/OVMF.fd`）。
真机上把 `build/esp/` 里的两个文件拷进 ESP 的同样路径即可（Secure Boot 需关闭）。

## 迁移进度

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| M0.1 | `arch/x86_64` 骨架（端口 / CR / MSR / CPUID / 描述符表 / TLB） | ✅ |
| M0.2 | 4 级分页后端（NX、cache 策略、1 GiB physmap、大页拆分、`map_device`） | ✅ |
| M0.3 | GDT/TSS/per-CPU `swapgs` + IDT，ring3 能进出 | ✅ |
| M0.4 | 8259+8254、每向量 stub、`#DF` 用 IST、用户态 `#PF` 分流 | ✅ |
| M0.5 | 内核线程、抢占式轮转、`sleep`/`exit`、`WaitQueue` | ✅ |
| M0.6 | 版本化 `BootInfo`（引导交接正式化） | ✅ |
| M0.7 | `x86_64-unknown-uefi` stub + 内核自有页表（删除 32 位跳板） | ✅ |
| M0.8 | i686 路径移入 `legacy-i686` 分支 | ✅ |
| P0 | `proc/`（进程 + VMA + handle 表）、每线程地址空间、`rondos-abi`、`int 0x80` v1 分发 | ✅ |
| P1 | `user/` 工作区、ELF64 装载器、boot.tar（tarfs）、`StartupBlock` + capability、
`sys_open`/`read`/`write`/`close`/`spawn`/`wait`/`proc_status`/`kill`/`sleep_ns`，
`init` 派生并回收子进程 | ✅ |
| P2a | 共享内存对象（`sys_mem_map/unmap/share/map_phys` + `sys_stat`）、
channel IPC（`chan_create/send/recv`）、`sys_spawn` 的 capability 委托、
每线程 FXSAVE（M0.9） | ✅ |
| P2b | 最小 C 支持（`crt0.S` + `rondos.h` + 宿主 gcc 直接出 ELF） | ✅ |
| P2c | tmpfs 可写层（`O_CREATE`/`sys_write`）+ `sys_readdir` | ✅ |

`make test` 用 OVMF 走真实 UEFI 固件启动，输出 `smoke: ALL PASS (17/17)`（另有 4 条
串口断言：init 进 ring3、派生/回收子进程、channel 往返、C 程序跑通）：
分页/physmap/大页拆分/地址空间隔离/W^X/设备映射、ring3 系统调用往返、
用户态缺页隔离、抢占、睡眠唤醒、线程退出，以及 P0 的进程生命周期
（handle 表、用户进程跑完 `sys_info`/`sys_clock_gettime`/`sys_yield`/`sys_log`/`sys_exit`、
一个进程缺页只杀自己、所有帧回收，以及 P1a 的 ELF 装载（`user/` 编译出的
`init`/`crash` 从 `boot.tar` 装载进 ring3，`init` 打印后正常退出、`crash` 缺页只杀自己）。
P1/P2 的 `init` 从 boot tar 里 `open`/`spawn`/`wait`/`kill` 自己的子进程，
用 channel 和共享内存做 IPC，内核不含任何进程策略。内核在第一条指令就切到自己的栈，
`BootInfo` 落地后立即切到内核自有的 PML4（丢掉 loader 的低地址 identity map）。

`user/lib/rondos-abi/` 是内核与用户程序共享的 ABI crate（syscall 号、`Status`、
handle 编码、结构体布局），内核通过 path 依赖它，两边布局不可能漂移。

## 参考资料

* Intel® 64 and IA-32 Architectures Software Developer's Manual
* [Writing an OS in Rust](https://os.phil-opp.com/)
* [rust-lang/compiler-builtins](https://github.com/rust-lang/compiler-builtins)
* PintOS
* 《30 天自制操作系统》
* 《x86 汇编语言——从实模式到保护模式》
* 《Orange'S：一个操作系统的实现》
