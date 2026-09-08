# RondOS

原本是自制的一个简易 x86 操作系统，现在已经是 Vibe OS 了。内核使用 Rust 开发，loader 用 NASM 编写。loader 是个两段式 boot loader，直接解析内核 ELF 文件并将其装入内存。

## 目录

- `kernel/`  Rust 内核，目标 `i686-unknown-none`，链接到虚拟地址 `0xc0022000`（迁移中，M0.8 后删除）
- `loader/stage1.s`  512 字节 MBR，把 Stage2 读到 `0x80000`
- `loader/stage2.s`  解析内核 ELF32，按 `PT_LOAD` 段装入 1 MiB 临时区后 memcpy 到 `p_paddr`，建立分页并跳入 `e_entry`
- `kernel64/`  **x86-64 内核**（迁移目标），高半区 `0xFFFFFFFF80000000`，`make run64` / `make test64`
  - `boot/multiboot32.s`  临时 32 位跳板（进 long mode），M0.7 的 UEFI stub 到位后删除
- `Makefile`  Linux 工具链构建脚本
- `docs/user-mode-design.md`  用户态方案设计（x86-64 + UEFI 迁移、ring 3、编译支持、可执行文件格式、冻结的 syscall ABI、Win3.1 复古桌面）

## x86-64 迁移进度

M0.1（`arch/x86_64` 骨架）、M0.2（4 级分页后端 + NX + physmap）与 M0.3（GDT/TSS/per-CPU/`swapgs`/IDT，ring3 能进出）已完成：

```bash
make run64      # QEMU 里启动 x86-64 内核（串口输出）
make test64     # 无头启动并检查 7 项冒烟测试
```

## 依赖

```bash
# Debian/Ubuntu
sudo apt install nasm qemu-system-x86 make

# Rust nightly + rust-src（编译 no_std 内核需要）
rustup install nightly
rustup component add rust-src --toolchain nightly
```

## 编译运行

```bash
make              # debug build
make release      # release build
make run          # debug build + QEMU
make clean        # 清理
```

`disk.img` 由 Stage1（512B）、Stage2（16 KiB）、Kernel ELF、padding 拼接而成。loader 会一次性把整个 Kernel ELF（最大 1 MiB）读到物理地址 `0x100000`，再根据每个 `PT_LOAD` 段拷到目标 `p_paddr`。

## 入口约定

- Kernel 入口符号 `_start` 由 `kernel.ld` 链接到虚拟 `0xc0022000`（物理 `0x00022000`）
- Stage2 从 ELF header 读 `e_entry`，建好分页后 `jmp KERNEL_BASE + e_entry`
- Stack 物理地址：`0x9000`（实模式阶段）和 `0x9000`（保护模式沿用）
- 内存布局传给内核的 `get_memlayout()`：最多 4 条 e820 条目在 `0xc0020200`（`u32` 长度 + 数据）

## 参考资料

* PintOS
* Intel® 64 and IA-32 Architectures Software Developer's Manual
* [Writing an OS in Rust](https://os.phil-opp.com/)
* [rust-lang/compiler-builtins](https://github.com/rust-lang/compiler-builtins)
* [MauriceKayser/rs-windows-builtins](https://github.com/MauriceKayser/rs-windows-builtins)
* 《30 天自制操作系统》
* 《x86 汇编语言——从实模式到保护模式》
* 《Orange'S：一个操作系统的实现》
