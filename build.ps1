$errorActionPreference = "Stop"

$release = $false
$run = $false

$args | ForEach-Object { 
    if ($_ -eq "release") { $release = $true }  
    if ($_ -eq "run") { $run = $true }  
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
if (Test-Path kernel.exe) {
    Remove-Item kernel.exe
    Write-Host "Removed Old Kernel"
}
if ($release) {
    Move-Item kernel/target/i686-pc-windows-msvc/release/kernel.exe kernel.exe
}
else {
    Move-Item kernel/target/i686-pc-windows-msvc/debug/kernel.exe kernel.exe
}
Write-Host "Copied Kernel"
python extract.py
Write-Host([string]::Format("Restructed Kernel Size: {0}(0x{0:x}) Bytes", (Get-Item .\kernel.bin).Length))
nasm -o loader.bin loader.s -l loader.lst
Write-Host "Built Loader"
Write-Host([string]::Format("Loader and Kernel Size: {0}(0x{0:x}) Bytes", (Get-Item .\loader.bin).Length))
nasm -o disk.img disk.s -l disk.lst
Write-Host "Built Disk"
if ($run) {
    ./bochsrc.bxrc
}
