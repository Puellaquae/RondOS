# RondOS 用户态方案设计（x86-64 + UEFI）

> 目标：把 RondOS 从 i686/BIOS 迁到 **x86-64 + UEFI 单路径**，在其上引入 **ring 3 用户态**，
> 让用户程序有明确的编译支持、可执行文件格式与**冻结的**系统调用 ABI；
> 桌面外观做成 **DOS / Windows 3.1 复古风格**（chunky 3D 边框、时代配色、位图字体、Program Manager），
> 但 ABI 与实现技术一律用现代做法（capability + handle、版本化结构体、类型化事件、用户态合成器、声明式 UI），
> **不模仿 MS 风格的 syscall**（不用 `INT 21h` / `AH=功能号` / `wParam/lParam` 大杂烩 / 段式语义）。
> 并且要能在**实机**上启动运行（见 §11）。

---

## 0. 设计原则

| 原则 | 具体含义 |
| --- | --- |
| 内核小、策略在用户态 | 内核只提供：地址空间、线程、IPC、文件句柄、设备映射。窗口/合成/主题/控件全在用户态服务里 |
| 能力制，而非环境权威 | 每个进程持有 handle 表，每次 syscall 都过权限位检查；`spawn` 只继承显式传入的 handle 子集 |
| ABI 单一真相源 | 内核与用户态共享同一个 `rondos-abi` crate，syscall 枚举用 `match` 穷尽性检查，加一个调用就必须加一个实现，否则编译不过 |
| ABI 事前冻结 | v1 定稿即只增不改（§6.8）。**这条是决定「先迁 x64 再做用户态」的直接原因**：32 位 ABI 一冻结就没法迁 64 位了 |
| 版本化、可演进 | 所有跨边界结构体以 `{size, version}` 开头；`sys_info()` 返回 ABI 版本与 feature 位，用户态做特性探测而不是假设 |
| 不信任用户输入 | 指针、长度、handle、manifest、对齐全部校验；用户态异常只杀进程，不 panic 内核 |
| 复古只在观感上 | 时代配色表 + 位图字体 + 1px 立体边框 + 无抗锯齿；底层是 32bpp damage 跟踪合成、共享内存 surface、类型化事件枚举 |
| 实机可用 | UEFI 单路径、GOP 取帧缓冲、无串口也能调试（帧缓冲控制台）；驱动缺失要能降级而不是挂死 |

非目标（v1 明确不做）：`fork`、动态链接 / `.so`、信号、SMP、抢占式内核、ASLR、多架构移植。
（PAE/NX 曾经是 i686 的短板，迁到 x64 后 **NX 是白送的**，见 §10。）

---

## 1. 现状与迁移决策

### 1.1 现有资产（迁移后仍然可用）

- `mm::vm::{PagingArch, AddressSpace, PageFlags}`：**架构无关的 VMM 抽象**，分页后端被隔离在一个 trait 后面 —— 这是迁移成本可控的关键。
- `mm::page::PageAllocator`：位图页框分配器，逻辑与位宽无关（只需换基址）。
- `mm::heap`：内核堆，按需映射虚拟区间。
- `thread`：抢占式轮转调度器，IRQ 整帧切换 + 就绪队列 + `sleep/exit` 状态机 —— **调度逻辑一行不改**。
- `fs`（ramfs + tar）、`io::vga`（文本控制台）、`disk::ata`（QEMU 用）、`utils`（`Singleton`/`SpinLock`/位操作）。

### 1.2 为什么迁 x86-64 + UEFI（以及为什么必须现在迁）

| 论据 | 说明 |
| --- | --- |
| **ABI 冻结**（决定性） | 若先做 i686 用户态再迁 x64，指针宽度从 `u32` 变 `u64`，`StartupBlock`、syscall 参数、handle、ELF 装载器全改 —— v1 冻结的 ABI 当场破裂。迁移只有两个时机：**冻结 ABI 之前**，或永远不迁 |
| **实机 ⇒ UEFI ⇒ 64 位** | 2012 年后的主板基本没有 CSM；而 64 位固件**不执行 IA32 UEFI 应用**（本机 OVMF 就是纯 64 位，连 OVMF32 都没装）。保留 i686 就得写「64 位固件 → 退出 long mode → 32 位保护模式」的 stub，纯增脆弱性 |
| **x64 补上现有短板** | NX 位（i686 无 PAE 根本做不到 W^X）、4 级分页、physmap 覆盖全部内存（不必再挤 64 MiB）、寄存器充足、高半区内核 |
| **`x86_64-unknown-none` 正好省事** | 官方 target 默认 `code-model: kernel`、`disable-redzone: true`、`+soft-float`：**内核保持 soft-float（自身完全不碰 XMM）**；用户程序另用 SSE2 + `sysv64` 的 spec 拿到硬件浮点，代价只是每线程一块 `fxsave` 区（§7.3） |
| **成本有界** | 内核目前约 4000 行，分页后端已被 trait 隔离。需重写：`arch/x86_64/*`、trap frame、`kernel.ld`、UEFI stub、构建系统，约 1500~2000 行；并**删掉 NASM 两段式 loader** |

### 1.3 迁移保留 / 重写清单

| 保留 | 重写 |
| --- | --- |
| `mm/vm.rs` 抽象、页框分配器、内核堆、`utils` | `arch/x86/*` → `arch/x86_64/*`（paging/gdt/tss/intr/percpu/mod） |
| 调度器逻辑、就绪队列、`sleep/exit` 状态机 | `thread` 的 `TrapFrame` 布局与汇编 stub |
| `fs`（ramfs/tar）、`disk::ata`（QEMU） | `loader/*.s`、`loader.bin`、`BootInfo` 交接、`kernel.ld` |
| 用户态方案整体架构（capability/ELF/syscall/UI） | ABI 字段宽度（`u32`→`u64`）、用户地址空间布局 |

---

## 2. 总体架构

```
┌──────────────────────────── 用户态 (ring 3) ────────────────────────────┐
│  apps: progman / notepad / calc / paint / minesweeper / shell          │
│        │  libui (Win3.1 控件库)  │  libgfx (位图绘制/字体/抖动)        │
│        └──────────┬──────────────┘                                      │
│                   │  channel IPC + 共享内存 surface                     │
│        display-server (WM + 合成器 + 主题 + 光标 + 输入路由)            │
│        init (第一个进程，持有 root/设备能力，负责拉起服务与回收子进程)  │
├──────────────────────────── syscall (int 0x80) ─────────────────────────┤
│  内核 (x86-64, higher-half): 地址空间 | 线程/调度 | VMA | handle 表    │
│        IPC channel | ramfs/VFS | 页框/堆 | fb 控制台 | PS/2 键盘       │
│        PIT/时钟 | 串口日志 | (AHCI/USB 见 §11)                           │
└─────────────────────────────────────────────────────────────────────────┘
        ▲
        │ BootInfo（版本化）
   ┌────┴─────────────────────────────────────────────┐
   │ UEFI stub (x86_64-unknown-uefi)                  │
   │ GOP 设模式 → 读 ESP 上的 kernel.elf + boot.tar   │
   │ GetMemoryMap → 组装 BootInfo → ExitBootServices  │
   │ 建 4 级页表 + NXE → 跳内核（RDI = &BootInfo）    │
   └──────────────────────────────────────────────────┘
```

关键决定：**内核不含任何窗口管理策略**。内核只把「线性帧缓冲物理地址 + 尺寸 + pitch + `PixelFormat`」通过 `BootInfo`（`sys_info` 可取）暴露，并用 `sys_mem_map_phys` 把 LFB 映射给唯一持有显示能力的 `display-server`；窗口 surface 是普通共享内存对象，客户端 `chan_send` 把 handle 交给 WM，WM 自己合成。这等价于 Wayland 的架构，而复古观感完全落在用户态（可热换主题、可截图、崩溃不带走内核）。

启动顺序：

1. UEFI stub → 内核初始化（mm / GDT+TSS+percpu / IDT / fb 控制台 / 输入 / ramfs←tar）。
2. 内核从 `/bin/init` 创建 `init` 进程，授予：根目录 handle、设备映射能力、控制台 handle。
3. `init` 依次 `spawn` `display-server`（唯一拿显示能力）、`shell`、`progman`，并 `wait` 回收。
4. 之后一切都是用户态。

---

## 3. 地址空间布局（4 级分页，48 位 VA）

| 内核 VA | 大小 | 用途 |
| --- | --- | --- |
| `0xFFFF_8000_0000_0000` | 512 GiB | **physmap**：`phys_to_virt(pa) = 0xFFFF_8000_0000_0000 + pa`，1 GiB 大页 |
| `0xFFFF_9000_0000_0000` | 16 GiB | 内核堆（按需映射） |
| `0xFFFF_A000_0000_0000` | 1 GiB | 设备映射 / fixmap（LFB、MMIO、IDT/GDT/TSS） |
| `0xFFFF_FFFF_8000_0000` | 2 GiB | **内核镜像**（`-mcmodel=kernel` 要求代码在最高 2 GiB） |
| `0xFFFF_FFFF_FF60_0000` | — | 每 CPU 数据、内核栈守护页等 |

physmap 用 1 GiB 大页把「UEFI 内存图里的所有 RAM」一次映完 —— 不再有 i686 时代 64 MiB 的紧箍咒。

| 用户 VA | 用途 |
| --- | --- |
| `0x0000_0000_0000_0000` | 永久不映射（空指针立刻 `#PF`） |
| `0x0000_0000_0000_1000..0x0000_0000_000F_FFFF` | vDSO 风格只读页（时钟/随机数快路径，v2） |
| `0x0000_0000_0040_0000` | **ELF 默认镜像基址**（4 MiB，`-mcmodel=small` 安全区内） |
| `0x0000_0000_1000_0000..0x0000_4000_0000_0000` | 堆 / 匿名 `mmap`（向上增长） |
| `0x0000_4000_0000_0000..0x0000_7000_0000_0000` | 共享内存 / 窗口 surface / IPC 环形缓冲 |
| `0x0000_7000_0000_0000..0x0000_7F00_0000_0000` | 线程栈（每个 64 KiB，下方 4 KiB guard，NX） |
| `0x0000_7F00_0000_0000..0x0000_7FFF_FFFF_FFFF` | 主线程栈 + `StartupBlock` + argv/envp |
| `0x0000_8000_0000_0000` | 非 canonical / 内核半区开始 |

用户页权限：文本 `present|user` + **NX=1 且不可写**；数据 `present|user|writable` + NX=1；栈 NX=1。
**NX 让 W^X 第一次真正成立**（i686 无 PAE 时做不到），`CR0.WP` 仍然开着，保证内核也写不进只读用户页。

---

## 4. 用户程序编译支持

### 4.1 工作区结构

```
user/
├── Cargo.toml                 # [workspace] members = lib/*, apps/*
├── targets/x86_64-rondos.json # 用户态 target spec（默认直接复用 x86_64-unknown-none）
├── user.ld                    # 用户程序链接脚本（基址 0x400000）
├── lib/
│   ├── rondos-abi/            # 内核+用户共享的 ABI 定义（唯一真相源）
│   ├── rondos-rt/             # _start / panic / 堆 / main 包装
│   ├── rondos-libc/           # C 程序用的最小 libc shim（P2）
│   ├── rondos-gfx/            # 32bpp surface、字体、位块传输、抖动
│   └── rondos-ui/             # 声明式 Win3.1 控件库
└── apps/
    ├── init/  display-server/  shell/  progman/  notepad/  calc/  paint/
```

内核通过 `path = "../../user/lib/rondos-abi"` 依赖同一个 crate（`default-features = false`，feature `kernel`），
用户态用 feature `user` 打开 syscall stub。这样 syscall 号、结构体布局、错误码不可能两边漂移。

### 4.2 target spec

直接用官方 `x86_64-unknown-none`（Tier 2，`-Z build-std` 下无需 `rustup target add`）：

```json
{
  "arch": "x86_64", "cpu": "x86-64", "code-model": "kernel",
  "disable-redzone": true,
  "features": "-mmx,-sse,-sse2,-sse3,-ssse3,-sse4.1,-sse4.2,-avx,-avx2,+soft-float",
  "linker": "rust-lld", "linker-flavor": "gnu-lld", "llvm-target": "x86_64-unknown-none-elf",
  "panic-strategy": "abort", "position-independent-executables": true,
  "rustc-abi": "softfloat", "target-pointer-width": 64
}
```

用户程序在 `user/targets/x86_64-rondos.json` 里以它为基础，改 `os: "rondos"`（便于 `cfg(target_os = "rondos")`）、
**打开 SSE2 并把 `rustc-abi` 从 `softfloat` 换成 `sysv64`**（`x86-64` cpu 基线自带 SSE2），
同时 `disable-redzone: false` 放开红区（用户态允许）。于是用户程序拿到硬件浮点，
代价是内核必须给每个线程保存 FPU 状态（§7.3）。
内核自身继续用上面的 soft-float spec：**内核代码永远不碰 XMM**，少一类难查的 bug。

构建命令：

```bash
cd user
cargo +nightly -Z json-target-spec -Z build-std=core,alloc,compiler_builtins \
      --target targets/x86_64-rondos.json -p notepad --release
```

链接固定地址（非 PIE）需要显式关掉默认的 PIE 行为：

```
-C relocation-model=static -C link-arg=--script=user.ld -C link-arg=--no-pie
```

### 4.3 链接脚本要点

```ld
ENTRY(_start)
PHDRS { text PT_LOAD FLAGS(5); data PT_LOAD FLAGS(6); note PT_NOTE; }
SECTIONS {
  . = 0x400000;
  .text   : { *(.text._start) *(.text*) } :text
  .rodata : ALIGN(4K) { *(.rodata*) *(.note.rondos) } :text :note
  .data   : ALIGN(4K) { *(.data*) } :data
  .bss    : ALIGN(4K) { *(.bss*) *(COMMON) } :data
  /DISCARD/ : { *(.eh_frame) *(.comment) *(.note.gnu.*) }
}
```

- 段页对齐、`p_vaddr == p_paddr`、`p_align = 0x1000`；
- 文本段 `FLAGS(5)` = R+X，数据段 `FLAGS(6)` = RW + **NX**（由内核装载器打 NX 位，见 §5.1）；
- `.note.rondos` 放进 **`PT_NOTE`**，这样 `--strip-all` 之后内核仍能读到 manifest。

### 4.4 运行时（rondos-rt）

```rust
// 入口约定：rdi = &StartupBlock，rsp = 栈顶（16B 对齐），其余寄存器未定义
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "and rsp, -16",
        "xor ebp, ebp",        // 断帧，便于崩溃回溯
        "call {init}",
        init = sym rt_init,
    )
}

#[no_mangle]
unsafe extern "C" fn rt_init(sb: *const StartupBlock) -> ! {
    abi::log(Level::Info, "rondos-rt: start");
    heap::init();                       // bump + 按需 sys_mem_map
    let args = startup::parse(sb);      // argv/envp/caps
    let code = app_main(args);
    sys::exit(code)
}
```

- panic handler：把 `{message, file, line, rip, cr2, backtrace(rbp 链)}` 写进调试 channel，然后 `sys_exit(101)`；
- 全局分配器：先「bump + 大块释放」，之后换 `talc`（no_std、无锁）；
- 无 libc。C 支持见 §4.5。

### 4.5 C 程序支持（P2）

提供 `rondos-libc` shim（`malloc/free/memcpy/printf/open/read/write/...` 薄封装 + `crt0`），
用 `clang --target=x86_64-unknown-none-elf -nostdlib` 或 `zig cc -target x86_64-freestanding` 交叉编译，
链接同一个 `user.ld`。**`rondos.h` 由 `xtask` 从 `rondos-abi` 生成**（不手抄，避免和冻结的 ABI 漂移），
C 侧同样用 `static_assert` 校验 `size_of`/`offset_of`。只保证「能编能跑 hello / 读写文件 / 开窗口」，
不追求 POSIX 兼容（没有 `fork`、信号、`pthread`）。

### 4.6 构建集成

`Makefile` 增加：

```make
user: $(shell find user -name '*.rs' -o -name '*.toml' -o -name '*.json' -o -name '*.ld')
	cd user && $(CARGO) +nightly -Z json-target-spec -Z build-std=core,alloc,compiler_builtins \
	    --target targets/x86_64-rondos.json --release
	@mkdir -p build/esp/rondos
	@for a in init display-server shell progman notepad calc paint; do \
	    cp user/target/x86_64-rondos/release/$$a build/esp/bin/$$a; done

boot.tar: user
	tar --format=ustar -C build/esp -cf $@ .

esp: boot.tar kernel uefi
	mkdir -p build/esp/EFI/BOOT build/esp/rondos
	cp boot/uefi/target/x86_64-unknown-uefi/release/rondos-boot.efi build/esp/EFI/BOOT/BOOTX64.EFI
	cp kernel/target/x86_64-unknown-none/debug/kernel build/esp/rondos/kernel.elf
	cp boot.tar build/esp/rondos/boot.tar
```

用户程序不再需要「塞进固定偏移的 tar」——它只是 ESP 上的一个文件，长度天然已知（见 §8）。

---

## 5. 可执行文件格式

### 5.1 格式：ELF64（`ET_EXEC` 静态）+ `.note.rondos` manifest

理由：UEFI stub 已经在解析 ELF64（内核自己就是）；rust-lld / ld / clang / zig 都能产出；不需要自研工具链。
**不自造容器格式**——元数据用标准 `PT_NOTE` 承载。

内核接受条件（不满足即 `ExecError`）：

| 检查 | 规则 |
| --- | --- |
| `e_ident` | `ELFCLASS64`、`ELFDATA2LSB`、`e_version == 1` |
| 类型 | `e_type == ET_EXEC`、`e_machine == EM_X86_64`；**拒绝 `ET_DYN`**（v1 无动态链接/重定位） |
| 段 | 至少一个 `PT_LOAD`；**拒绝 `PT_INTERP`**（Linux 动态可执行文件天然被挡） |
| 对齐 | `p_align == 0x1000`、`p_vaddr % 0x1000 == 0`、`p_filesz <= p_memsz` |
| 范围 | 全部落在用户半区（canonical、`< 0x0000_8000_0000_0000`）且不撞保留区 |
| 权限 | 不允许 `PF_W|PF_X` 同时置位；`PF_X` 的段映射为可执行，其余一律 **NX** |
| 入口 | `e_entry` 在某个可执行 `PT_LOAD` 内 |
| 大小 | 文件 ≤ 16 MiB，段数 ≤ 16 |

manifest **可选**：缺省 = 零能力控制台程序（方便直接跑裸 ELF）。真正危险的是动态链接和越权，靠上表拒绝即可。

### 5.2 manifest（`PT_NOTE` / `n_type = 0x524E4431`）

```rust
#[repr(C)]
pub struct StructHeader { pub size: u32, pub version: u32 }   // 所有跨边界结构体都这样开头

#[repr(C)]
pub struct Manifest {
    pub hdr: StructHeader,
    pub abi_min: u32, pub abi_max: u32,   // 需要的 ABI 版本区间
    pub kind: EntryKind,                  // Gui | Console | Service
    pub request: u64,                     // 申请的能力位（内核与父进程权限求交）
    pub window: WindowHint,               // 默认尺寸/标题/样式（Gui 程序）
    pub name: StrRef, pub desc: StrRef,   // 指向文件内的字符串
    pub icon: SectionRef,                 // 32x32 图标
    pub min_mem_kb: u32, pub flags: u32,
}
```

用户态用宏生成，编译期就能校验长度：

```rust
rondos_abi::manifest! {
    kind: EntryKind::Gui,
    name: "Notepad",
    window: WindowHint { w: 480, h: 320, title: "Untitled - Notepad", style: STYLE_RESIZABLE },
    request: Cap::WINDOW | Cap::FILESYSTEM,
}
```

宏展开成一个 `#[used] #[link_section = ".note.rondos"] static`，内容按 ELF note 规则 4 字节对齐
（`n_namesz = 7, name = "RondOS\0"` 补到 8）。

### 5.3 启动块（StartupBlock）

内核在新进程栈顶写入，`rdi` 指向它：

```rust
#[repr(C)]
pub struct StartupBlock {
    pub hdr: StructHeader,
    pub abi_version: u32, pub feature_bits: u64,
    pub entry: u64, pub image_base: u64,
    pub argv: Slice<StrRef>, pub envp: Slice<StrRef>,
    pub caps: Slice<CapDesc>,        // 实际授予的 handle（可能少于申请）
    pub window: Option<Handle>,      // Gui 程序：display-server 预创建的窗口
    pub random_seed: u64,
}
```

带 `hdr` 的好处：内核以后可以往后追加字段，老程序按 `size` 忽略尾巴。

### 5.4 装载流程（`exec::load`）

1. 从 fs handle 读文件进内核缓冲（上限 16 MiB）。
2. 走 §5.1 校验表；解析 `PT_NOTE` manifest；`abi_min <= ABI <= abi_max` 否则 `AbiMismatch`；申请能力 ⊆ 父进程可委托权限，否则 `CapabilityDenied`。
3. 建新地址空间（内核半区共享且不可变，见 §7）。
4. 逐 `PT_LOAD`：分配页框 → 以段权限 + NX 位映射到 `p_vaddr` → 拷贝文件字节 → `p_memsz-p_filesz` 清零。
5. 建用户栈（主线程 16 页 + 下方 guard 页，NX），写入 `StartupBlock`、argv/envp 字符串、能力表。
6. 建主线程：`TrapFrame { rip: e_entry, cs: 0x1b, ss: 0x23, rflags: 0x202, rsp: stack_top, rdi: &StartupBlock }`，16 KiB 内核栈。
7. 在进程表登记，返回 `Handle<Process>`（权限 `WAIT|KILL|STATUS`）给父进程，入调度队列。

v1 全量预装（不做 demand paging），但 VMA 列表同时建立，为后续按需分页/COW 留口。

### 5.5 演进路径

- **v2**：`ET_DYN` + `DT_RELA` 重定位 → ASLR（装载时随机基址）。
- **v3**：签名 + 压缩的薄容器（`[header][manifest][elf][resources]`），由 `xtask` 打包；对内核仍表现为 ELF。
- **可选**：ABI 前端无关化 —— 同一套类型化 syscall 用 wasm 解释器/AOT 再实现一遍，非受信插件跑 wasm（无需 ring 3）。

---

## 6. 系统调用 ABI

### 6.1 与 MS 风格的区别（明确写死）

| MS / DOS 风格 | RondOS |
| --- | --- |
| `INT 21h` + `AH=功能号`、每个服务一个中断向量 | **单一入口** `int 0x80`，`rax` = 类型化 `SyscallId` 枚举 |
| `CF`/`AX` 返回 DOS 错误码，`GetLastError()` 全局状态 | **两寄存器结果**：`rax` = `Status`（0=OK），`rdx` = 值；无隐式全局错误状态 |
| 大而平铺的 API（`wParam/lParam` 打包一切） | 每组少量 syscall，**对象化**：handle + 类型化请求结构体 |
| 全局命名空间、环境权威 | **capability**：handle 即权限，`spawn` 显式委托 |
| `HWND` 是内核对象、窗口消息由内核广播 | 窗口是用户态对象，事件是**类型化枚举**，经 channel 点对点投递 |

### 6.2 入口与返回约定

```
int 0x80
  rax = SyscallId
  rdi, rsi, rdx, r10, r8, r9 = arg0..arg5      （SysV 顺序，避让 syscall 会破坏的 rcx/r11）
返回：
  rax = Status（0 = Ok，>0 = 稳定错误码）
  rdx = 返回值 / 附加载荷
寄存器：rax、rdx 被破坏；其余全部原样保留（stub 整帧保存恢复）
```

- 参数寄存器顺序刻意用 SysV，这样 Rust 的 `extern "sysv64"` shim 和 C 编译器调用约定天然对齐；
- 结构体参数一律传**用户指针 + 长度**（`u64`），内核用 `copy_from_user/copy_to_user` 拷贝，不直接解引用；
- 指针必须完整落在同一个 VMA 内（内核查 VMA 表），否则 `Status::BadAddress`；
- 无负 errno、无 `errno` 全局变量。

### 6.3 统一 TrapFrame 与阻塞式 syscall

64 位没有 `pusha`，stub 必须手工压寄存器；特权级切换时 CPU 额外压 `ss/rsp`：

```rust
#[repr(C)]
pub struct TrapFrame {
    // 低地址（stub 压入）
    pub r15: u64, pub r14: u64, pub r13: u64, pub r12: u64,
    pub r11: u64, pub r10: u64, pub r9: u64, pub r8: u64,
    pub rdi: u64, pub rsi: u64, pub rbp: u64, pub rdx: u64,
    pub rcx: u64, pub rbx: u64, pub rax: u64,
    pub vector: u64, pub error: u64,                 // stub 规范化
    pub rip: u64, pub cs: u64, pub rflags: u64,      // CPU 压入
    pub rsp: u64, pub ss: u64,                       // 仅 CPL 变化时 CPU 压入
}   // 高地址
```

ring0→ring0 时 stub 补压假 `rsp/ss`，两种入口共用同一布局与同一 `iretq` 恢复路径。
`build_initial_frame` 对用户线程填 `cs=0x1b / ss=0x23 / rflags=0x202`，`rdi` 顺带就是 `StartupBlock` 指针。

**syscall 也能同步阻塞**：把 `pick_next` 提为 `schedule(cur_frame) -> next_frame`，
IRQ0 stub 和 syscall stub 都返回帧指针。需要阻塞时：

```rust
enum SyscallCont {                 // 唤醒后在内核里继续执行的续体
    ChanRecv { chan: Handle, buf: UserPtr, len: usize },
    Wait     { handles: Vec<Handle>, deadline_ns: u64 },
    ProcWait { target: Handle },
}
```

- 阶段 1：syscall 处理函数发现要阻塞 → 把用户帧存到线程的 `user_frame`，把 `frame.rip` 指向
  `syscall_resume_trampoline`、`frame.cs = 0x08`，`state = Blocked(cont)`，调用 `schedule`。
- 阶段 2：被唤醒后 trampoline 在内核态跑完剩余逻辑，把结果写进用户帧，再 `iretq` 回用户态。

这样一次 `read` 只花一次调度，不用「等到下一个 tick 再让用户态重试」。

### 6.4 Handle 与权限

```rust
#[repr(transparent)]
pub struct Handle(pub u64);       // index: u32 | generation: u32（编码冻结）
```

- 每个进程一张 `HandleTable`：`{ object: ObjRef, rights: Rights, generation: u32 }`；
- `close` 只把 generation +1 → 悬垂 handle 必然 `Status::BadHandle`；
- handle 是**进程私有**的，内核不接受别的进程的 handle；
- 每次操作都检查 `rights`（例如 `Memory::WRITE`、`File::READ`、`Process::KILL`）；
- IPC 可以传递 handle（`chan_send` 的 handle 数组），这就是 WM 拿到客户端 surface 的方式——没有全局命名。

### 6.5 syscall 表（v1）

| # | 名称 | 签名（概念） |
| --- | --- | --- |
| `0x00` | `sys_info` | `(&mut Info) -> ()`：ABI 版本、feature 位、`BootInfo`（fb/输入设备） |
| `0x10` | `sys_exit` | `(status: u32) -> !` |
| `0x11` | `sys_thread_exit` | `(status: u32) -> !` |
| `0x12` | `sys_thread_spawn` | `(entry, stack_top, arg) -> Handle` |
| `0x13` | `sys_yield` | `() -> ()` |
| `0x14` | `sys_sleep_ns` | `(ns: u64) -> ()` |
| `0x15` | `sys_clock_gettime` | `(kind: Clock) -> u64 ns` |
| `0x16` | `sys_log` | `(level, buf, len) -> n`：写串口 + 帧缓冲控制台 |
| `0x20` | `sys_spawn` | `(image: Handle<File>, argv, envp, caps[], flags) -> Handle<Process>` |
| `0x21` | `sys_wait` | `(handles[], n, timeout_ns) -> (index, reason)`：**水平触发**就绪等待 |
| `0x22` | `sys_proc_status` | `(Handle<Process>) -> ExitStatus` |
| `0x23` | `sys_kill` | `(Handle<Process>) -> ()`：置取消位，阻塞中的等待返回 `Cancelled` |
| `0x30` | `sys_mem_map` | `(len, flags) -> Handle<Memory>` |
| `0x31` | `sys_mem_unmap` | `(Handle<Memory>) -> ()` |
| `0x32` | `sys_mem_share` | `(Handle<Memory>, rights) -> Handle<Memory>` |
| `0x33` | `sys_mem_map_phys` | `(pa, len, cache) -> Handle<Memory>`：**需要 `Cap::DEVICE_MAP`** |
| `0x40` | `sys_chan_create` | `() -> [Handle<Chan>, Handle<Chan>]` |
| `0x41` | `sys_chan_send` | `(Handle<Chan>, buf, len, handles[], n) -> n` |
| `0x42` | `sys_chan_recv` | `(Handle<Chan>, buf, len, out_handles[], n) -> (n, nhandles)` |
| `0x43` | `sys_chan_close` | `(Handle<Chan>) -> ()` |
| `0x50` | `sys_open` | `(Handle<Dir>, path, flags) -> Handle<File>` |
| `0x51` | `sys_read` | `(Handle, buf, len) -> n` |
| `0x52` | `sys_write` | `(Handle, buf, len) -> n` |
| `0x53` | `sys_seek` | `(Handle<File>, off: i64, whence) -> u64` |
| `0x54` | `sys_stat` | `(Handle, &mut Stat) -> ()` |
| `0x55` | `sys_readdir` | `(Handle<Dir>, index) -> Option<DirEntry>` |
| `0x56` | `sys_close` | `(Handle) -> ()` |

图形只占 **2 个** syscall（`sys_info` 取 fb 描述、`sys_mem_map_phys` 映射 LFB），
其余（窗口、合成、主题、控件、事件）全在用户态。这是「内核小」的直接体现。

### 6.6 版本与特性协商

```rust
pub const ABI_VERSION: u32 = 1;

pub mod feature {
    pub const SYSCALL_FAST: u64 = 1 << 0;   // syscall/sysret 快路径
    pub const DEMAND_PAGING: u64 = 1 << 1;
    pub const ASLR: u64 = 1 << 2;
    pub const DEVICE_MAP: u64 = 1 << 3;
    pub const SSE: u64 = 1 << 4;            // 用户程序可用 SSE2（内核每线程 FXSAVE）
    pub const NUMA_HINT: u64 = 1 << 5;      // 预留，SMP 时代用
}
```

`rondos-rt` 在 `init` 里调一次 `sys_info`，把 feature 位存进静态变量；
syscall stub 是一个函数指针（快路径可用时指向 `syscall` 版本），避免每次调用都分支。

### 6.7 `syscall`/`sysret` 快路径（v2）

- `IA32_LSTAR` 设入口，`IA32_STAR` 设 CS/SS，`IA32_FMASK` 掩 IF/DF 等；
- **CPU 不切栈**：`syscall` 只把 `rip` 从 `LSTAR` 取走，`rsp` 还是用户栈 —— 入口必须立刻
  `swapgs` + 从 per-CPU 变量加载内核栈，且此时**不能发生中断/缺页**（`IA32_FMASK` 已经清 IF）；
- 返回用 `sysretq`，要求 `rcx` 是 canonical 用户地址（否则是著名的 `sysret` 漏洞面）；
- 由 `feature::SYSCALL_FAST` 协商，探测不到就退回 `int 0x80`。

### 6.8 ABI 稳定化规则（v1 冻结）

要求是「稳定 ABI，但不要学 Linux」——Linux 的稳定是**事后**稳定：编号随手分配、结构体留隐式 padding 洞、
`ioctl` 用魔数编码、`long` 随架构变宽、`errno` 全局状态。我们改成**事前**冻结：v1 定稿即不再破坏，只允许追加。

| 规则 | 说明 |
| --- | --- |
| 地址/长度/指针一律 `u64`，枚举/标志/计数用 `u32` | 禁用 `usize`/`isize`/裸指针出现在跨边界结构体；指针用 `u64` 用户 VA + 显式校验 |
| 禁用 `bool`、`char`、隐式 `repr` 枚举 | 用 `u32` 或 `#[repr(u32)]` + 显式判别值 |
| 禁用隐式 padding | 每个洞显式写 `_pad0: u32`，并 `const_assert!(size_of::<T>() == N)` |
| 每个结构体以 `StructHeader { size, version }` 开头 | 追加字段只允许在**尾部**；接收方按 `min(size, sizeof)` 读取 |
| 枚举只增不减 | 内核侧 `match` 必须带 `_ => Status::Unsupported`，不得对新值 panic |
| 字段带单位后缀 | `timeout_ns`、`len_bytes`、`offset_bytes` |
| 保留位显式声明 | `_reserved: [u64; 4]`，将来启用时必须保证旧程序传 0 也安全 |

语义冻结：

- syscall 号、`Status` 数值、`Rights` 位、handle 编码（`index:32 | generation:32`）**永久不复用**；
  删除的调用留 `_reserved_NN` 墓碑。
- 承诺「v1 程序能在任何未来内核上跑」：内核只加 v2 调用，不改 v1 语义。
- 破坏性变更只允许发生在新**主版本**（`ABI_VERSION` 大版本 +1）。
- **用户态服务协议可以演进而非冻结**：WM↔app 的窗口协议、文件服务协议由服务自己协商版本，
  旧程序用 compat shim 接住。内核 ABI 冻结 ≠ 用户态协议冻结。

防漂移机制：

- `rondos-abi` 里一组 `abi_layout` 测试断言所有结构体的 `size_of`/`offset_of`；
- `rondos.h` 由 `xtask` 从 `rondos-abi` **生成**（C 侧同样带 `static_assert`），不手抄；
- 内核 dispatch 用 `match syscall_id` 穷尽匹配；
- 每次发版跑「ABI 冻结检查表」：新增调用/字段是否只追加、是否显式 padding、是否有版本协商路径。

---

## 7. 内核迁移清单

### 7.1 迁移步骤（M0，先于用户态）

| 步骤 | 内容 | 验收 |
| --- | --- | --- |
| **M0.1 ✅** | `arch/x86_64/mod.rs`：in/out、CR0-4、MSR 读写、`cpuid`、`hlt/sti/cli`、`lgdt/lidt`、`invlpg` | 能编译并跑 `hlt` |
| **M0.2 ✅** | `arch/x86_64/paging.rs`：4 级分页 `PagingArch` 实现、1 GiB physmap、NX 位、大页拆分、`map_device` + cache 策略 | 虚拟内存自检通过（6 项冒烟测试全过，见下） |
| M0.3 | `gdt.rs` / `tss.rs` / `percpu.rs`：64 位 GDT（null/kcode 0x08/kdata 0x10/ucode 0x1b/udata 0x23/TSS 0x28 双槽）、`swapgs`、per-CPU 结构 | ring3 能进出 |
| M0.4 | `intr.rs`：64 位 IDT + 统一 `TrapFrame` + naked stub（手工压寄存器、`iretq`） | 定时器/异常正常 |
| M0.5 | `thread`：适配新 `TrapFrame`，`schedule(frame)` 抽出，`WaitQueue` | 抢占式轮转与阻塞自检通过 |
| M0.6 | `kernel.ld`（高半区 + `-mcmodel=kernel`）、`BootInfo`、`loader.rs` 退役 | 内核能从 `BootInfo` 拿内存图 |
| M0.7 | `boot/uefi/`：GOP 设模式 + 读 ESP 文件 + `ExitBootServices` + 建页表跳内核 | QEMU+OVMF 与**一台真机**都能起来 |
| M0.8 | 删除 `loader/stage1.s`/`stage2.s`/`loader.bin`、`i686-unknown-none.json` | 构建只剩一条路径 |

**M0.1/M0.2 已完成**（`kernel64/`，`make test64` 输出 `smoke: ALL PASS (6/6)`）：

| 冒烟测试 | 验证内容 |
| --- | --- |
| `physmap` | `PHYS_MAP_BASE + pa -> pa` 覆盖 0..4 GiB（1 GiB 大页），且内核线性别名一致 |
| `huge-split` | 在 1 GiB 大页内映射 4 KiB 页 → 连续两次拆分（1 GiB→2 MiB→4 KiB），邻居映射保持完好，重复映射被拒 |
| `addr-space` | 新地址空间只复制内核半区（不复制恒等映射），映射用户页、真正切 CR3、跨地址空间写入可见、销毁回收页表 |
| `w^x` | `executable=false` 置 NX、`executable=true` 清 NX；可写/用户位正确 |
| `device-map` | `map_device` 多页 + 写合并策略（GOP 帧缓冲路径） |
| `allocator` | 多页/单页分配、写读校验、释放 |

**临时引导路径**（M0.7 的 UEFI stub 到位后删除）：

QEMU 的 multiboot 只接受 32 位镜像，而内核是 64 位高半区 ELF，所以 `qemu -kernel kernel64` 不可行。
当前用 `boot/multiboot32.s`（NASM `-f bin`，multiboot a.out kludge 头）做 32 位跳板：
建好恒等映射 / physmap / 内核 2 MiB 页映射 → `CR4.PAE|PGE` → `EFER.LME|NXE` → `CR0.WP|PG`
→ `ljmp` 进 64 位 → 跳到内核入口（入口地址由 Makefile 从 ELF 读出后 `-D` 进去）。

```bash
make kernel64    # 构建 x86-64 内核
make run64       # QEMU：-kernel trampoline.bin -device loader,file=kernel64
make test64      # 无头启动 + grep 冒烟测试结果
```

两个坑值得记住（都已修）：

* `e & FLAG != 0` 在 Rust 里是 `e & (FLAG != 0)`（`!=` 优先级高于 `&`），会静默变成「测试最低位」——所有位测试都必须写 `(e & FLAG) != 0`；
* 拆分大页后，刚合成的 PTE 是原大页的副本，此时允许覆盖（`from_split`），否则 `map` 会误报 `AlreadyMapped`。

### 7.2 文件级清单

| 文件 | 改动 |
| --- | --- |
| `arch/x86/` | **删除**，替换为 `arch/x86_64/` |
| `mm/vm.rs` | `PageFlags` 增加 `user/executable/cache`；`map_device(pa, va, len, policy)`；VMA 列表 |
| `mm/page.rs` | physmap 基址改为 `0xFFFF_8000_0000_0000`，上限跟随 UEFI 内存图 |
| `mm/heap.rs` | `HEAP_START_VA` 移到 `0xFFFF_9000_0000_0000` |
| `thread/mod.rs` | 64 位 `TrapFrame`；`schedule(frame) -> frame`；`WaitQueue`；`Thread { proc, kind, kstack_top, cont }`；`syscall_stub` + resume trampoline |
| `io/vga.rs` | 保留为 fallback，新增 `io/fb.rs`：`BootInfo` 取 fb 描述、写合并映射、帧缓冲控制台（**实机无串口时的唯一调试手段**） |
| `io/input.rs`（新） | PS/2 键盘（+ 可选 aux 口）→ `InputEvent` 环形缓冲，作为可 `read` 的设备 handle |
| `proc/mod.rs`（新） | `Process`、`HandleTable`、进程表、`exec::load` |
| `bootinfo.rs`（新） | `BootInfo` 版本化结构与校验 |
| `boot/uefi/`（新） | `x86_64-unknown-uefi` stub，用 **`uefi-rs`**（已定：依赖不多、体积可控，省掉手写协议表） |

### 7.3 浮点 / SSE 策略

分工：**用户程序用 SSE2 硬件浮点，内核保持 soft-float。**
两者是独立的编译目标和独立的 ABI，syscall 边界只走 GPR、不传浮点，互不干扰。

内核要做的：

| 项 | 做法 |
| --- | --- |
| 使能 | `CR0.EM=0, MP=1, NE=1, TS=0`（先做急切保存，不上 lazy）；`CR4.OSFXSR=1, OSXMMEXCPT=1` |
| 每线程状态 | 一块 **64 字节对齐、1 KiB** 的缓冲（`fxsave`/`fxrstor`；留足空间给将来换 `xsave`） |
| 切换 | `schedule()` 里对**换出**的线程 `fxsave`、对**换入**的线程 `fxrstor`；200 Hz 下开销可忽略 |
| 新线程初始化 | 缓冲区清零后**必须写 `MXCSR = 0x1F80`、x87 控制字 `= 0x037F`** —— 全零的 MXCSR 会打开所有异常掩码，用户态第一条浮点指令就吃 `#XF`（最容易踩的坑） |
| 后续优化 | **不做 lazy**：lazy（`CR0.TS=1` + `#NM` 按需保存）只在「大量线程从不碰浮点」时才划算，代价是异常路径、内核态 `#NM` 分支、以及「状态在别处」的调试困难；现代内核都改回了急切保存。本项目对这点性能不敏感 ⇒ **急切保存定稿** |
| AVX | **不做**（性能不敏感）⇒ `fxsave`/`fxrstor` 足够，不需要 `xsave`/`xrstor` 与按 CPUID 取区域大小 |
| 内核自用 | 内核 soft-float ⇒ 内核不会在切换间隙污染 XMM，`fxsave` 里始终是用户的真实状态 |

注意 `fxsave` 要求 16 字节对齐（按 64 字节对齐更保险），且必须在 `CR0.EM=0`、`CR4.OSFXSR=1` 之后才可用。

---

## 8. 引导：UEFI 单路径

### 8.1 为什么不做 BIOS

| | UEFI + GOP | BIOS + VBE |
| --- | --- | --- |
| 实机可用性 | 2012 年后主板唯一路径 | 现代主板没有 CSM，跑不到 |
| 显示 | **GOP 是规范强制的「通用显示」**，所有固件都实现，零驱动 | 需要 `int 0x10 AX=4F02` 取 LFB，模式差异大 |
| 加载内核 | 直接从 FAT ESP 读文件，长度天然已知 | 需要自写多段 loader 解析 FAT/固定偏移 |
| x64 支持 | stub 直接 long mode 跳转 | 需要 16→32→64 位三段式 loader |
| 内存图 | `GetMemoryMap` 直接给 | 需要 e820 |
| 结论 | **v1 只做这条** | 不维护；将来真需要老机器再加 `VideoBackend` 实现 |

**「支持通用 VGA」这个需求由选择 UEFI 本身满足**：GOP 就是实机上的通用显示路径，
不用碰 VBE 寄存器、不用为不同显卡写驱动。内核只认 `BootInfo.framebuffer`，
将来若真给 BIOS-only 老机器加 VBE 路径，就是加一个生产者，很便宜。

### 8.2 UEFI stub

| 步骤 | 说明 |
| --- | --- |
| 1 | `OpenProtocol<GraphicsOutput>` → `QueryMode` 挑最接近 640×480 且 32bpp 的模式 → `SetMode` |
| 2 | `OpenProtocol<SimpleFileSystem>` → 读 `\rondos\kernel.elf` 与 `\rondos\boot.tar` |
| 3 | `GetMemoryMap` → 拷出内存描述符 |
| 4 | 从 UEFI 配置表取 ACPI RSDP |
| 5 | 分配一页放 `BootInfo`，填 fb/内存图/initrd/RSDP/cmdline |
| 6 | `ExitBootServices` |
| 7 | 建 4 级页表：physmap（1 GiB 页）+ 内核镜像（按 ELF `PT_LOAD`）+ 设备窗口；`EFER.NXE=1`；载入 CR3 |
| 8 | `jmp` 内核入口，`rdi = physmap 视角的 &BootInfo` |

注意点：

- **GOP 事实上只有 32bpp**（`PixelBlueGreenRedReserved8BitPerColor`，少数 24bpp）——8bpp 索引模式不存在，
  这就是「直接上真彩」的由来（§9.1）；
- GOP 帧缓冲可能在 4 GiB 以上，x64 无所谓（i686 就必须检查）；
- `ExitBootServices` 后固件的页表与启动服务全部失效，FB 是 MMIO，**要自己用写合并（PWT/PCD）映射**；
- 有些固件不提供 640×480，需要挑最近可用模式并让合成器处理缩放；
- `ExitBootServices` 之后**没有任何固件输入服务**（UEFI 模式下也没有 BIOS 的 legacy USB 模拟），
  键鼠必须自己驱动（§11）。

### 8.3 BootInfo

```rust
#[repr(C)]
pub struct BootInfo {
    pub hdr: StructHeader,                     // size / version
    pub memory_map: Slice<MemDesc>,            // UEFI 内存描述符（不再需要 e820）
    pub framebuffer: Option<FramebufferInfo>,  // pa / w / h / pitch / PixelFormat
    pub initrd: Slice<u8>,                     // boot.tar（physmap 视角）
    pub acpi_rsdp: u64,
    pub cmdline: StrRef,
    pub physmap_base: u64,                     // 0xFFFF_8000_0000_0000
    pub kernel_range: (u64, u64),
    pub boot_kind: BootKind,                   // Uefi | LegacyBios（预留）
}
```

### 8.4 ESP 构建（mtools / vvfat / 实机 U 盘）

`mtools` 是一组在 Unix 上**直接操作 FAT 文件系统**的工具（`mformat`/`mcopy`/`mdir`/`mmd`），
原理是用户态直接读写 FAT 镜像或设备文件的字节 —— **不需要 mount、不需要 root**：

```bash
mkfs.vfat -F 32 -n RONDOS esp.img          # 或 mformat -C -i esp.img ::
mcopy -i esp.img -s build/esp/EFI ::/EFI   # ESP: /EFI/BOOT/BOOTX64.EFI
mcopy -i esp.img build/kernel.elf build/boot.tar ::/rondos/
mdir  -i esp.img ::/EFI/BOOT               # 核对
dd if=esp.img of=/dev/sdX bs=4M status=progress   # 实机 U 盘
```

QEMU 里有个**免依赖替代**：`-drive file=fat:rw:build/esp,format=raw` —— QEMU 的 vvfat 块驱动
拿宿主目录**合成**一个 FAT 文件系统，改文件立刻生效，开发时完全不用 mtools。
（vvfat 对规范覆盖不全、写模式有已知怪癖，只适合开发。）

**已定：构建统一用 mtools**（`sudo apt install mtools`）。分工是：QEMU 用 vvfat 免依赖快速迭代，
`make usb` 用 mtools 出实机 U 盘镜像。选 mtools 的另一个收益是——**P5 的盘上文件系统就选 FAT**，
宿主能直接用 `mtools` 读写镜像（拖文件进去、看日志），省掉自研 mkfs 与宿主打包器。

```make
run: esp
	qemu-system-x86_64 -machine q35 -m 512 -smp 1 \
	  -bios /usr/share/OVMF/OVMF.fd \
	  -drive file=fat:rw:build/esp,format=raw \
	  -vga std -serial stdio -monitor none
```

### 8.5 Secure Boot

真机若开着 Secure Boot，未签名的 `BOOTX64.EFI` 不会被加载。三个选择：
关掉 Secure Boot（最简单）、自签并往固件密钥库注册（`sbctl`/`KeyTool`）、或做签名流水线。
文档默认「关掉」，并在 README 里写清楚。

---

## 9. 图形子系统：DOS / Win3.1 复古外观

### 9.1 视觉规格（把「复古」定死到数值）

**前提：复古是渲染策略，不是像素格式。** GOP 帧缓冲是 32bpp 真彩，所以「Win3.1 观感」由
**几何 + 配色表 + 字体 + 无抗锯齿**保证。好处：配色可写精确 RGB、截图可直接出 PNG、
以后想加扫描线/CRT 滤镜只是后处理。**不做省内存的 8bpp 路径**（硬件不是真复古）。

| 项 | 取值 |
| --- | --- |
| 分辨率 | 640×480 @ 32bpp（备用 800×600）；GOP 给什么用什么，不硬编码 |
| 配色 | 固定「时代配色表」常量：Win3.1 默认 16 色 + 16 级灰阶（精确 RGB，如 `#AA5500` 棕、`#008080` teal）；越界颜色用 2×2 有序抖动逼近 |
| 桌面 | 纯色 teal `#008080`，平铺无渐变 |
| 窗口边框 | 外 1px 黑；上/左 1px 白高光；下/右 1px 深灰阴影（raised）；客户区纯白 |
| 标题栏 | 高 18px，活动态深蓝底白字（居中），非活动态浅灰底深灰字；左侧 18×18 系统菜单盒，右侧最小化/最大化盒 |
| 菜单栏 | 高 20px，`File Edit View ...`，加速键字母带下划线，激活项 1px 凹陷框 |
| 字体 | 8×16（标题/菜单）+ 8×8（状态栏），VGA ROM 位图字体，**无抗锯齿、无字距调整** |
| 按钮 | 3D 斜面（上/左高光、下/右阴影），4px 内边距，焦点框 = 1px 50% 棋盘点线 |
| 滚动条 | 16px 宽，两端 chunky 箭头，滑块 3D 斜面 |
| 光标 | 经典 11×19 箭头，1-bit 掩码，热点 (1,1)；阻塞时沙漏 |
| Program Manager | 32×32 图标 + 图标下文字网格；最小化窗口变成底部 100×20 标题条（**这个细节最 Win3.1**） |
| 对话框 | 模态、居中、无最小化/最大化，底部 OK/Cancel |
| 控制台 | 窗口内模拟 80×25 文本模式（蓝底白字），DOS 既视感 |

主题数据化：`Theme { face: Rgb(0xC0,0xC0,0xC0), highlight: Rgb(0xFF,0xFF,0xFF), shadow: Rgb(0x80,0x80,0x80), title_active: Rgb(0x00,0x00,0x80), ... }`
全是 `const` 表，可热换（Win3.1 的 Hotdog Stand 主题、EGA 主题都是同一套控件换表）。
每像素 `u32`（GOP 通常是 BGRA，`PixelFormat` 由 `BootInfo` 带给合成器，不假设字节序）。

### 9.2 架构：内核只给裸设备，合成在用户态

```
display-server (唯一持有 Cap::DISPLAY 的进程)
├── 映射 LFB（sys_mem_map_phys，写合并 PCD/PWT）
├── 维护窗口列表 / z 序 / 焦点 / 拖拽 grab
├── 从 input 设备 handle 读 InputEvent → 命中测试 → 投递到目标窗口的 channel
├── damage rect 跟踪 → 按 z 序把各窗口 surface blit 进 LFB
└── 自己绘制窗口装饰（标题栏/边框/按钮），客户端只画客户区
客户端进程
├── sys_mem_map(len) 拿一块 32bpp surface，映射进自己地址空间
├── chan_send(wm, CreateWindow{ size, title, style, surface_handle, icon })
├── 在 surface 上自绘（libui/libgfx），改完 chan_send(wm, Damage{ rects })
└── 主循环：sys_wait([win_chan, app_chan], timeout) → match 类型化事件
```

要点：

- **surface 共享而非拷贝**：客户端拿到的 `Handle<Memory>` 同时被 WM 映射，blit 是 u32 行拷贝，零格式转换；
- **damage 驱动**：客户端上报脏矩形，WM 只重绘脏区；
- **事件类型化**：`UiEvent::Pointer{ x, y, buttons, mods, kind }`、`UiEvent::Key{ scancode, ch, mods, kind }`、
  `Focus(bool)`、`Resize{ w, h }`、`Close`、`Timer{ id }`、`ThemeChanged`——`match` 穷尽；
- **就绪模型水平触发**：`sys_wait` 返回后仍可能无数据（被取消/被抢占），客户端重试即可；
- **无 alpha、无抗锯齿**：位块传输用经典 ROP 子集（COPY/AND/OR/XOR/NOT/PATCOPY），
  图标和光标用 1-bit 掩码，越界颜色用 2×2 有序抖动——现代实现、复古语义。

### 9.3 输入

目标实机的情况：**有 PS/2 键盘，没有 PS/2 鼠标**。因此输入方案要照顾这一点：

- **键盘**：PS/2 IRQ1 scancode → 不再直接 `print!`，改为产生 `InputEvent::Key` 入队；
- **指针**：目标机没有 PS/2 鼠标，两条路并行，但**对上层是同一件事**：
  1. `display-server` 内建「键盘驱动虚拟指针」：方向键移动光标、`Enter`/`Space` 当左键，
     产生与真鼠标**完全相同的 `InputEvent::Pointer`**；
  2. 将来接 USB 鼠标走 xHCI（§11 M4）。
  因为指针事件统一，**GUI 与控件库只有一套交互模型**，不需要为「无鼠标」再写一遍键盘导航，
  真鼠标到位后直接多一个事件源即可；
- PS/2 aux 口初始化代码（`0x64`/`0x60`，`0xA8`、`0xF4`，IRQ12）仍然保留在驱动里（万一以后插兼容鼠标/触控板），
  但**不在关键路径上**；
- 内核只做「原始事件 → 环形缓冲」，不做焦点、不做加速曲线、不做双击判定——那是 WM 的策略；
- `display-server` 用 `sys_read(input_handle, &mut [InputEvent])` 阻塞读取。

### 9.4 声明式 UI（libui）

选型：**声明式 + 保留式树**（Flutter / Elm 那一套，而不是每帧全窗重绘的立即模式）：

```rust
enum Msg { Save, Open, TextChanged(String), Tick }

impl App for Notepad {
    fn view(&self, cx: &Cx) -> Widget {
        ui! {
            Window::new("Untitled - Notepad").size(480, 320) {
                MenuBar {
                    "File" => [ "New", "Open", "Save" -> Msg::Save, "Exit" -> cx.quit ],
                    "Edit" => [ "Undo", "Cut", "Copy", "Paste" ],
                }
                Editor::new(&self.text).on_change(Msg::TextChanged)
                StatusBar::new(format!("{} chars", self.text.len()))
            }
        }
    }
    fn update(&mut self, msg: Msg) { match msg { /* ... */ } }
}
```

- `view` 产出声明式描述，框架把它 **reconcile** 到上一棵保留式控件树上（按稳定 ID diff）；
- 只有变化的子树产生脏矩形 → 交给 damage 合成；
- 事件处理是 `match Msg`，不用消息映射表，也不用 `wParam/lParam` 拆包；
- 保留式树同时保住 Win3.1 该有的行为：焦点顺序、Tab 遍历、模态对话框、菜单跟踪、无效矩形重绘；
- 控件库规模控制在 ~1500 行：`Window/MenuBar/Button/CheckBox/Radio/Edit/List/ScrollBar/StatusBar/GroupBox/Dialog`，
  加一个布局 pass（绝对定位 + `Row`/`Col` 辅助）。

### 9.5 性能与内存

- 全屏 640×480×32bpp = **1.2 MiB**；一次窗口拖动只重绘「旧位置 + 新位置」两个矩形；
- QEMU TCG 下纯软件 blit 足以流畅拖动窗口；实机更是绰绰有余；
- 一块 surface 1.2 MiB × 8 窗口 ≈ 10 MiB —— x64 大内存下完全不是问题；
- `display-server` 可选输出 PNG 截图（自己实现极简编码器），CI 里能自动比对渲染结果。

---

## 10. 安全与健壮性

| 威胁 | 对策 |
| --- | --- |
| 用户传野指针 | 所有指针经 VMA 校验 + `copy_from_user`；拷贝期间若仍 `#PF`（竞态），按「内核态用户访问」标记杀进程，不 panic |
| 悬垂 handle | generation 计数；close 后旧值必失效 |
| 越权操作 | 每个 handle 带 rights 位；`spawn` 只继承显式子集；`sys_mem_map_phys`（映射 LFB/MMIO）需专属能力 |
| 用户态执行数据 | **NX 位**（x64 白送）：数据/栈页一律 NX，文本页只读+可执行，W^X 真正成立 |
| 用户态写内核 | 内核半区 PTE 无 U 位；`CR0.WP=1` 保证内核也写不进只读用户页，COW 才正确 |
| 恶意 manifest | 申请能力与父进程权限求交；ABI 版本不匹配直接拒绝装载 |
| 用户态崩溃 | `#PF/#GP/#UD/#DE` 在 CPL=3 时 → 杀进程、回收地址空间、父进程收到 `ExitStatus::Fault{ vector, rip, cr2 }` |
| 内核栈耗尽 | 每线程独立内核栈；`int 0x80` 靠 TSS.RSP0 切栈、每次调度更新；`syscall` 路径靠 per-CPU 栈 + `swapgs` |
| 死锁/丢唤醒 | 单 CPU 下 syscall 路径用 `cli` 临界区（大内核锁），唤醒路径在中断里也持同一把锁；`wait` 前先「入队再检查」 |
| 实机无法调试 | 帧缓冲控制台必须最先可用（M1 验收条件）；串口可选 |

---

## 11. 实机运行路线（真实工程量）

**引导路径是简单的部分，驱动才是真正的工作量。**

| 需求 | 实机现实 | 工作量 |
| --- | --- | --- |
| 显示 | GOP 给 LFB，**零驱动** | 小 |
| 内存图 | UEFI `GetMemoryMap`，e820 作废 | 小 |
| 键盘 | 目标实机**有 PS/2 键盘** ⇒ 键盘路径可用 | 小 |
| 指针 | 目标机**没有 PS/2 鼠标** ⇒ 先由 display-server 用键盘合成指针事件；真鼠标需 USB HID（xHCI） | 小（键盘合成）/ 大（xHCI） |
| 磁盘 | 现有 ATA PIO 走 0x1F0 legacy 端口，AHCI 控制器不暴露；**只做 AHCI，NVMe 不做** | 中 |
| 中断 | **沿用现有 8259+PIT**（已定）；现代平台想更稳再上 APIC/IOAPIC | 0（现在）/ 中（以后） |
| 关机/重启 | ACPI FADT（PM1a_CNT / reset register）——**只做这两件**，不解析 MADT | 小 |
| 调试 | 目标机串口要自己跳线 ⇒ **帧缓冲控制台是唯一默认调试手段**（必须最先可用） | 小 |
| Secure Boot | 必须关掉或自签，否则不加载 | 0（但要知道） |

分阶段（每阶段都能在真机上得到可验证结果）：

| 阶段 | 目标 | 说明 |
| --- | --- | --- |
| **M1** | x64 + UEFI + GOP 帧缓冲控制台，真机启动并打印自检 | **刻意不依赖任何输入设备**，把「能不能启动」与「输入能不能用」解耦；也不需要磁盘驱动（kernel+tar 从 ESP 进内存） |
| **M2** | PS/2 键盘输入 + 键盘合成指针 | 目标实机已确认有 PS/2 键盘；GUI 从这一刻起就能用（光标由方向键驱动） |
| **M3** | AHCI 磁盘 | 有盘上 FS 才有意义（P5 的 FAT 依赖它） |
| **M4** | USB HID 鼠标（xHCI + boot protocol）——**因缺 PS/2 鼠标而回到计划内** | 见下 |

**USB 工程量**：完整 xHCI（命令环/事件环/TRB、设备槽、endpoint context、控制传输、HID class）
约 **1200~2000 行**；只做「直接接在 USB 2 端口上的 HID boot protocol 鼠标/键盘」（不枚举 hub、
不支持 USB 3、不做热插拔、不做其他 class）可压到 **800~1000 行**，其中 xHCI 核心是主体、
HID boot protocol 只占一百多行。现代主板的 USB 3 端口同样由 xHCI 管理，所以真要做就做 xHCI。
M1~M3 不依赖它，GUI 也不会因为缺它而不可用。

---

## 12. 实施路线图

| 阶段 | 交付物 | 验收标准 |
| --- | --- | --- |
| **M0 迁移** | x86-64 + UEFI 单路径、4 级分页 + NX、64 位 trap/GDT/TSS、SSE 使能 + 每线程 FXSAVE、UEFI stub + `BootInfo`、帧缓冲控制台；删除 NASM loader | QEMU+OVMF 与**一台真机**都能启动并打印自检；现有调度/内存自检全过 |
| **P0 内核地基** | ring3、TSS.RSP0 随调度更新、`schedule(frame)`、`Process`/VMA/handle 表骨架、`sys_exit`+`sys_log` | 内核手工构造用户线程，ring3 打印一行再 `sys_exit`；用户态非法写触发 `#PF` 只杀该线程 |
| **P1 装载与编译** | `user/` 工作区、target spec、`user.ld`、`rondos-abi`/`rondos-rt`、ELF64 装载器、`sys_spawn`/`sys_wait`、`init` | 串口/帧缓冲控制台出现 `init: hello from ring 3`；两个用户进程并发，一个崩溃不影响另一个 |
| **P2 内存与 IPC** | `sys_mem_map/share`、用户堆、`chan_*`、`sys_wait` 多 handle、文件 handle、tmpfs 层、最小 C 支持 | echo 程序经 channel 回显；`/bin/*` 可读；一个 C 写的 hello 也能跑；`make test` grep `PASS` |
| **P3 显示** | GOP 640×480×32bpp + LFB 设备映射、PS/2 键盘 + 键盘合成指针、`display-server`、surface 共享、Win3.1 窗口装饰、控制台窗口 | 光标能拖动/聚焦窗口；控制台窗口里能跑 shell 命令 |
| **P4 控件与程序** | 声明式 `libui`（`view`/`update`）、`libgfx`、字体、主题、progman / notepad / calc / paint / minesweeper | 截图与 Win3.1 截图并排看「像」；ProgMan 双击图标启动程序 |
| **P5 打磨** | AHCI、APIC/IOAPIC、xHCI HID 鼠标、demand paging/COW、`ET_DYN`+ASLR、`syscall` 快路径、FAT 盘上 FS、wasm 前端 | 老 ABI 程序在新内核上照跑；实机可持久化存盘、可用真鼠标 |

验证链路：`make run`（QEMU+OVMF+vvfat）→ `-serial file:serial.log` → `make test` grep 关键字；
真机走 `make usb` + `dd` 到 U 盘。调试手段：**帧缓冲控制台（实机唯一默认手段）**、QEMU `-s -S` + GDB、
panic 打印用户态寄存器帧；串口在目标机上需要跳线，属可选项。

---

## 13. 已定决策与遗留问题

**已定**：

| # | 决策 | 结论 |
| --- | --- | --- |
| 1 | 架构 | **x86-64 + UEFI 单路径**，删除 i686/BIOS 路径；迁移在 ABI 冻结之前完成 |
| 2 | 像素格式 | **32bpp 真彩**（GOP 事实上不支持 8bpp 索引），复古靠时代配色表 + 抖动 + 位图字体 + 几何；不省内存 |
| 3 | 文件系统 | v1 = tar 启动镜像 + RAM tmpfs；**P5 盘上 FS 选 FAT**（与 mtools 构建链一致，宿主可直接读写镜像） |
| 4 | `libui` | **声明式 + 保留式树**（`view`/`update` + reconcile + 脏矩形） |
| 5 | ABI | **v1 冻结、只追加、事前设计**（§6.8） |
| 6 | C 支持 | Rust 先行，P2 补最小 C（`rondos.h` 由 ABI 生成 + `crt0` + 极简 libc） |
| 7 | 实机 | 目标机有 PS/2 键盘、**无 PS/2 鼠标** ⇒ M2 走 PS/2 键盘 + 键盘合成指针；真鼠标走 M4 的 xHCI；串口需跳线 ⇒ 帧缓冲控制台为默认调试手段 |
| 8 | BIOS/VBE | 不做；「通用显示」由 GOP 满足 |
| 9 | UEFI stub | 用 **`uefi-rs`**（依赖不多、体积可控） |
| 10 | ESP 构建 | 用 **mtools**（QEMU 侧用 vvfat 免依赖快速迭代） |
| 11 | 中断 | **沿用现有 8259+PIT**；APIC/IOAPIC 推到 P5 |
| 12 | 磁盘 | **只做 AHCI**，NVMe 不做 |
| 13 | 浮点 | **用户程序 SSE2 + `sysv64` ABI（硬件浮点），内核保持 soft-float**；每线程 FXSAVE、**急切保存定稿**（不做 lazy） |
| 14 | AVX | **不做** ⇒ `fxsave`/`fxrstor` 足够，不引入 `xsave` |
| 15 | ACPI | **只做关机/重启**（FADT），不解析 MADT |
| 16 | USB | **仅鼠标**（xHCI + HID boot protocol），排在 M4/P5；键盘用 PS/2 |

**遗留**：

1. **键盘合成指针的手感**：方向键移动速度、是否加加速曲线、`Enter` 还是 `Space` 当左键——纯调参，M2 时定。
2. **键盘导航规范**：即便指针由键盘驱动，`Tab` 焦点遍历、`Alt` 加速键、`Enter` 激活仍需在 `libui` 里定成规范（P4）。
3. **PS/2 键盘 + USB 鼠标共存**：M4 到位后两个事件源同时在线，WM 需要合成成一个光标（去抖/去重复）——到 M4 再处理。
