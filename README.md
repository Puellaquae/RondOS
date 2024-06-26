# RondOS 我儘のロンドＳ

自制的一个简易 x86 操作系统，内核使用 Rust 开发，用汇编编写了一个简单的 bootloader。

## 编译运行

需要安装 nasm，nightly 版的 rust，虚拟机可以选择 bochs 或者 qemu。

运行 build.ps1 脚本进行编译，参数 `run` 可在编译后启动 Bochs，参数 `release` 可让内核以 release 版本编译（debug 版本体积太大可能无法运行），参数 `qemu` 使用 Qemu 进行模拟。

## 参考资料

* PintOS
* Intel® 64 and IA-32 Architectures Software Developer’s Manual
* [Writing an OS in Rust](https://os.phil-opp.com/) 
* [rust-lang/compiler-builtins](https://github.com/rust-lang/compiler-builtins)
* [MauriceKayser/rs-windows-builtins](https://github.com/MauriceKayser/rs-windows-builtins)
* 《30 天自制操作系统》
* 《x86 汇编语言——从实模式到保护模式》
* 《Orange'S：一个操作系统的实现》