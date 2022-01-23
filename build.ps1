$errorActionPreference = 'Stop'

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
}
if ($release) {
    Move-Item kernel/target/i686-pc-windows-msvc/release/kernel.exe kernel.exe
}
else {
    Move-Item kernel/target/i686-pc-windows-msvc/debug/kernel.exe kernel.exe
}
python extract.py
nasm -o loader.bin loader.s -l loader.lst
nasm -o disk.img disk.s -l disk.lst
if ($run) {
    ./bochsrc.bxrc
}
