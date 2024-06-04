$errorActionPreference = "Stop"

$release = $false
$run = $false
$useqemu = $false

$args | ForEach-Object { 
    if ($_ -eq "release") { $release = $true }  
    if ($_ -eq "run") { $run = $true }  
    if ($_ -eq "qemu") { $useqemu = $true }  
}

Set-Location kernel
try {
    if ($release) {
        cargo build --release 
    }
    else {
        cargo build
    }
}
finally {
    Set-Location ..
}
if (Test-Path kernel.bin) {
    Remove-Item kernel.bin
    Write-Host "Removed Old Kernel"
}
if ($release) {
    Copy-Item kernel/target/i686-unknown-none/release/kernel kernel.bin
}
else {
    Copy-Item kernel/target/i686-unknown-none/debug/kernel kernel.bin
}
Write-Host([string]::Format("Kernel Size: {0}(0x{0:x}) Bytes", (Get-Item .\kernel.bin).Length))
nasm -o loader.bin loader.s -l loader.lst
nasm loader.s -f elf -g -o loader.elf  # for gdb
Write-Host "Built Loader"
Write-Host([string]::Format("Loader and Kernel Size: {0}(0x{0:x}) Bytes", (Get-Item .\loader.bin).Length))
nasm -o disk.img disk.s -l disk.lst
Write-Host "Built Disk"
if ($run) {
    if ($useqemu) {
        qemu-system-x86_64.exe -drive format=raw,media=disk,file=disk.img -serial stdio
    } else {
        ./bochsrc.bxrc
    }
}
