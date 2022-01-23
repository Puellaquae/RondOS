# RondOS 我儘のロンドＳ

自制的一个简易 x86 操作系统，bootloader 用的汇编，内核用的 Rust。目前还只是能进到 32 位保护模式显示些个字。

## 编译运行

我是用 Windows 做的开发，汇编器是 nasm ，内核的编译到 i686-pc-windows-msvc，拿 PowerShell 和 python 写了编译脚本，系统在 Bochs 上跑。

需要安装 nasm，nightly 版的 rust，msvc，python，python 的库 pefile，bochs。

运行 build.ps1 脚本进行编译，参数 `run` 可在编译后启动 Bochs，参数 `release` 可让内核以 release 版本编译。

## 参考资料

* PintOS
* Intel® 64 and IA-32 Architectures Software Developer’s Manual
* [Writing an OS in Rust](https://os.phil-opp.com/) 
* [rust-lang/compiler-builtins](https://github.com/rust-lang/compiler-builtins)
* [MauriceKayser/rs-windows-builtins](https://github.com/MauriceKayser/rs-windows-builtins)
* 《30 天自制操作系统》
* 《x86 汇编语言——从实模式到保护模式》
* 《Orange'S：一个操作系统的实现》