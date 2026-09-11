# 引导与实机调试

从 UEFI loader 交接给 x86-64 内核这一段，在没有串口的目标机上怎么定位问题。

这是 README 里原先「实机调试」一节的完整内容（含 2026-09 那次"蓝一下黑屏"的结论）；
README 只保留一段指引。想快速上手，先看下面的「一分钟流程」。

## 一分钟流程

1. 把新 loader / 内核写进 U 盘，启动，**看屏幕**：loader 会打印上一次启动留下的
   阶段、位掩码和 CPU fault；本机信息（CPU 特性、帧缓冲地址、identity/physmap、
   CR3）也都在同一屏。
2. 如果这次仍然挂：内核态 CPU 异常现在会被自己的 IDT **trap 住**（不再三重故障复位），
   屏幕会刷成**亮红**；重启后上面那行 `previous kernel CPU FAULT` 给出 vector/error。
3. 用 `make test QEMU_CPU=Nehalem,+pdpe1gb` 与 `make test` 分别验证 1 GiB / 2 MiB
   两条页表路径。

## CMOS stage 与位掩码

内核把启动进度写进 CMOS NVRAM：最后一个阶段在 0x2E（RTC 校验和不覆盖的扩展区，
写入后会读回校验，失败会依次回退到 0x34/0x35），**到达过哪些阶段**则记成两个
位掩码字节 0x36/0x37（bit N = stage N），加一个运行签名 0x38（0x5A）。
UEFI loader 在**下一次**启动时把这些打印在屏幕上：

```
  previous kernel boot RESET at stage 0x07  <-- see the stage table below
    stage 7 reached
    mark 0x12 reached
    mark 0x13 reached
```

一个字节只能说明"最后停在哪"，位掩码能说明"哪些阶段真的跑到了"——例如
`stage 0x15` 但 `stage 7` 没置位，就说明问题在 0x15 那一步而不是调度器整体。
签名不匹配时打印 `(no stage mask from the previous boot)`。

| stage | 含义 |
| --- | --- |
| 0x01 | 进入 `kmain` |
| 0x02 | `BootInfo` 已接收 |
| 0x03 | 内核自有页表已建立并切换 CR3 |
| 0x04 | 帧缓冲控制台已就绪 |
| 0x05 | `self_check()` 通过 |
| 0x06 | GDT/TSS/percpu/IDT 就绪（现在发生在 0x03 **之前**，见"先装 trap"一节） |
| 0x07 | 定时器 + 调度器初始化中 |
| 0x10 | ↳ 进入 `bring_up_scheduler` |
| 0x11 | ↳ PIC 已重映射、IRQ0/IRQ1 已解屏蔽 |
| 0x12 | ↳ PIT 已编程 |
| 0x13 | ↳ 调度器状态 + idle 栈就绪 |
| 0x08 | 中断已打开（`sti`） |
| 0x15 | ↳ `sti` 之后的第一次时钟中断即将到来 |
| 0x09 | 正在拉起 `/bin/init` |
| 0x20 | ↳ 即将建立内核自有地址空间 |
| 0x21 | ↳ 地址空间已建好，仍在 loader 页表上 |
| 0x22 | ↳ CR3 已切换，physmap 可用（能写 RAM 记录本身即证明） |
| 0xff | 系统进入 idle（正常） |

## 自证：构建指纹 + 满屏刷色

进入 `kmain` 之后立刻做两件可见的事，专门用来回答"内核到底跑了没有"：

1. 打印并逐字节校验自己的构建标签（内核和 loader 共用同一个字符串，
   loader 在加载 ELF 后也会在里面搜这个字符串并报出现次数，**0 次就说明
   U 盘上的 `kernel.elf` 不是这个 loader 配套的那个**）；
2. 用 loader 的恒等映射把**整屏刷成深蓝**、停一下；建好自己的页表并切完 CR3
   之后，再通过设备窗口把**整屏刷成深绿**、停一下。控制台起来后再把屏幕清掉。

所以实机上如果只看到深蓝就卡住 → 死在 CR3 切换附近；看到深绿 → 页表和设备映射
都没问题，继续往后找。停顿长度可以用 loader 的 `rondos.delay=N` 覆盖（默认
2000 万次自旋；测试里不能再大，否则 QEMU 逐条模拟会超时）。

## 交接：中断状态与帧缓冲位置

`ExitBootServices` **不保证**关中断，也不保证 8259 的屏蔽字是什么（OVMF 恰好两项
都干净，所以 QEMU 里永远看不到这个 bug）。而内核切到自己的页表后会丢掉 loader 的
低地址恒等映射，固件留下的 IDT/GDT 从那一刻起就不可达。两者叠加：只要固件留着
定时器中断没屏蔽，交接后第一拍中断就会 double fault → 三重故障 → 复位，屏幕上
正好是"loader 刷完蓝色、瞬间黑屏"。所以：

- loader 在 `main` 第一行、以及 `ExitBootServices` 返回之后，都执行 `cli` 并把
  8259 主/从两个屏蔽字写成 `0xff`；内核 `_start` 第一条指令再 `cli` 一次；
- 交接前 loader 打印 `handoff: interrupts off PIC mask 0xff/0xff`，实机上这一行
  对不上就是这里。

另一个同类陷阱是帧缓冲的位置。GOP 的线性帧缓冲不在固件内存图里，很多主板把它放在
4 GiB 甚至更高的 MMIO 空间。内核最早的整屏刷色走的是 loader 的恒等映射；帧缓冲一旦
落在恒等映射之外，第一条写像素的指令就 #PF，而此时 IDT 还没建立 —— 同样是三重故障
黑屏，且报错发生在 `jump_to_kernel`。现在 loader 把帧缓冲末尾也算进恒等映射大小
（`pages: identity N GiB` 会因此变大）；若超过单个 PDPT 的 512 GiB 上限，内核的
`paint_phys` 会自动改用设备窗口，绝不碰一个可能没映射的物理地址。

## 1 GiB 大页不是所有 CPU 都有（老机器黑屏的真正原因，已实机确认）

**这是最容易漏掉的一条，也是 E3 实机"蓝一下黑屏"的最终根因**：该机
`NX yes 1G-pages NO`，换成 2 MiB 页后一次点亮进系统。`PDPE1GB`（CPUID
`8000_0001H:EDX[26]`，PDPT 里的 1 GiB 页）是**可选**特性：2010 年前的 Intel 64
CPU（Core 2、Atom 等）没有它。在没有该特性的 CPU 上，PDPT 里 PS=1 的项是保留位，
第一次 `mov cr3` 之后的取指/取数就会 #PF；而处理这个 #PF 又要走同一套坏页表 →
double fault → 三重故障。表现就是 loader 刷完蓝色瞬间黑屏。loader 原来用 1 GiB 页
搭恒等映射和 physmap，于是在这类机器上必挂。

更坑的是 **QEMU 默认 CPU 也报 `1G-pages NO`，但 TCG 不检查这个保留位**，所以
`make test` 一直是绿的 —— 真机复现、QEMU 不复现。

现在 loader 在启动横幅里打印 `cpu: <厂商> family ... | NX ... 1G-pages ...`，
并据此选页表：有 1 GiB 页就用 1 GiB（`pages: ... (1 GiB pages)`），没有就用 2 MiB
（`pages: ... (2 MiB pages)`）；`NX` 也只在 CPU 支持时才写 `EFER.NXE`。内核的
`huge-split` 测试在没有 1 GiB 页时跳过（否则它自己就会造出保留项）。

想两条路径都测：`make test`（默认 CPU，走 2 MiB 回退）与
`make test QEMU_CPU=Nehalem,+pdpe1gb`（走 1 GiB 快路径）。

## 先装 trap，再切 CR3：让 fault 可捕获而不是三重故障

排查"切页表就黑屏"时最糟的是：**没有 IDT，任何 #PF 都直接三重故障/复位**，屏幕上
什么都不剩。所以内核现在把 `gdt/percpu/intr::init()`（以及所有 handler 的注册）放在
**第一次切 CR3 之前**——IDT 和 handler 都在内核窗口里，loader 映射了、内核自有根也
复制了，切前切后都可达。

由此，CPU 异常变成一个**可捕获的 trap**：

- `isr_dispatch` 在碰任何可能与分页有关的内存之前，先用**纯端口 I/O** 把 vector/error
  写进 CMOS `0x3B`/`0x3C`（第一次 fault 优先，之后的不覆盖）；这样即使 physmap 本身
  坏了，记录也能活过一次复位；
- 内核态异常还会通过设备窗口把整屏刷成**亮红**（不经过 physmap / 文本控制台），
  实机上就是"抓到 fault 了、机器没复位"的照片证据；
- 下一次启动时 loader 会打印
  `previous kernel CPU FAULT: vector 0x0e (#PF) error 0x0000`，`0x2E` 那套 stage
  掩码/进度条仍然照常工作；正常走完的启动会在 `stage_done()` 里清掉 fault 记录。

顺带修了一个老 bug：内核 `raw_breadcrumb` 写的是 CMOS 寄存器 **0x0A**（`0x8a`，
其实是 RTC 分频寄存器），而 loader 读的是 **0x3A** —— 所以那行 `kernel breadcrumb`
永远是 `0x00`。现在两边都是 `0x3A`。

## 内存映射（重要）

`BootInfo` 里的固件内存映射原来只保留 **64 条**。OVMF 在这台机器上就报了 130 条，
有些主板更多；一旦被截断，内核看到的 RAM 就只剩前面一小段——8 GiB 的机器上
`RAM top` 会显示成 `0x01000000`（16 MiB），分配器随即失败。

现在上限是 **256 条**，并且：

- loader 打印 `RAM top: 0x... (usable N MiB, M entries)`，M 就是保留下来的条数；
- 紧接着打印前 6 条 `mem[i] 地址 长度 kind`，屏幕上直接能看到固件到底报了什么；
- 内核 `self_check()` 也会打印 `self-check: N MiB usable below 0x...`。

## 最低内存要求

内核镜像本身约 2.4 MiB，加上帧位图和页表，**至少需要 32 MiB 可用内存**。
低于这个数时 `self_check()` 会直接报出来：

```
self-check: 12 MiB usable below 0x01000000, kernel ends 0x0026b000
PANIC: system has only 12 MiB of usable RAM below 0x01000000; RondOS needs at least 32 MiB
```

（16 MiB 的老机器上，内核镜像就已经贴到 RAM 顶了，分配器一个页都拿不到。）
loader 的横幅里也会打印可用内存：`RAM top: 0x01000000  (usable 15 MiB)`。

## 屏幕上的进度条

除了 CMOS，内核还会**直接把进度画在帧缓冲上**（不经过文本控制台，控制台被下移
到进度条之下），所以重启前屏幕最后的样子就是证据：

- 左上角两行共 16 个方块，对应阶段 1..16，亮绿色 = 已到达，黄色 = 最后一个；
- 方块下面是最后阶段的两位十六进制大数字。

## U 盘上的记录文件

loader 每次启动都会在 ESP 上维护 `\rondos\bootlog.bin`：一条 40 字节的定长记录，
新的在前，最多 16 条。它**只读取/前移重写**，所以即使某次写入中途掉电，之前的记录
仍然可读。把文件拷出来用 `python3 tools/dumplog.py bootlog.bin` 就能列出每次启动
最后到达的阶段。loader 还会从 RAM 顶部那一页读取内核留下的 `ProgressRecord`
（DRAM 掉电前的内容能挺过一次热重启），把它写进记录。

## 串口：没有 COM1 时不要无限等待

**已定位（2026-09）**：真机上内核停在 `0x03` 的原因不是页表也不是中断，而是
**串口**。目标机器没有 COM1，浮空的总线让线路状态寄存器读回全 1（看起来
"发送器空闲"），而发送器永远不会真的空 —— 旧代码里没有上限的 `wait_for!` 就在
那里空转，机器看起来像"在 stage 3 重启"，实际上是卡死在日志那一行。

修法：`io/serial.rs` 里所有等待都加上次数上限，超时就把该端口永久标记为 dead，
后续字节直接丢弃，日志仍然照常进帧缓冲控制台。

如果那之后仍然在某个阶段失败，用上面的位掩码 / 进度条 / `bootlog.bin` 定位。

如果机器进入重置循环，看 loader 屏幕上这几行就能定位。loader 自己也会把它的
识别信息（identity/physmap/kernel 窗口大小、RAM 顶、CR3、帧缓冲参数）打印在
屏幕上，配合一起看。
