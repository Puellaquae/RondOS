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
  - `src/thread/`       内核线程、抢占式轮转调度、`sleep`/`exit`、`WaitQueue`
  - `src/bootinfo.rs`   版本化引导交接结构（magic/size/version）
  - `src/multiboot.rs`  临时 multiboot 适配器（M0.7 后删除）
  - `boot/multiboot32.s` 临时 32 位跳板（建立分页 → long mode → 跳内核），
    M0.7 的 UEFI stub 到位后删除
- `files/`      启动 tar 镜像内容（将来的用户程序与资源）
- `docs/user-mode-design.md`  用户态完整设计（迁移、ring 3、编译支持、可执行文件格式、
  冻结的 syscall ABI、Win3.1 复古桌面）
- `Makefile`    构建脚本

## 依赖

```bash
# Debian/Ubuntu
sudo apt install nasm qemu-system-x86 make

# Rust nightly + rust-src（no_std 内核 + build-std 需要）
rustup install nightly
rustup component add rust-src --toolchain nightly
```

## 编译运行

```bash
make            # 构建内核 + 32 位跳板
make run        # 构建并启动 QEMU（串口输出）
make test       # 无头启动，检查冒烟测试结果
make release    # release 构建
make clean
```

QEMU 无法用 `-kernel` 直接启动 64 位 ELF（multiboot 只收 32 位），所以 `make run`
是 `-kernel trampoline.bin -device loader,file=kernel64` 的组合。

## 迁移进度

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| M0.1 | `arch/x86_64` 骨架（端口 / CR / MSR / CPUID / 描述符表 / TLB） | ✅ |
| M0.2 | 4 级分页后端（NX、cache 策略、1 GiB physmap、大页拆分、`map_device`） | ✅ |
| M0.3 | GDT/TSS/per-CPU `swapgs` + IDT，ring3 能进出 | ✅ |
| M0.4 | 8259+8254、每向量 stub、`#DF` 用 IST、用户态 `#PF` 分流 | ✅ |
| M0.5 | 内核线程、抢占式轮转、`sleep`/`exit`、`WaitQueue` | ✅ |
| M0.6 | 版本化 `BootInfo`（引导交接正式化） | ✅ |
| M0.7 | `x86_64-unknown-uefi` stub（GOP 设模式 + 读 ESP 文件 + 跳内核） | 待办 |
| M0.8 | 删除 32 位跳板与 i686 路径 | ✅（i686 已移入 `legacy-i686` 分支） |

`make test` 目前输出 `smoke: ALL PASS (10/10)`：分页/physmap/大页拆分/地址空间
隔离/W^X/设备映射、ring3 系统调用往返、用户态缺页隔离、抢占、睡眠唤醒、线程退出。

## 参考资料

* Intel® 64 and IA-32 Architectures Software Developer's Manual
* [Writing an OS in Rust](https://os.phil-opp.com/)
* [rust-lang/compiler-builtins](https://github.com/rust-lang/compiler-builtins)
* PintOS
* 《30 天自制操作系统》
* 《x86 汇编语言——从实模式到保护模式》
* 《Orange'S：一个操作系统的实现》
