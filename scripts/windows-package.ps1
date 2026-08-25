# 组装 Windows 分发包：便携 ZIP 和 MSI。
#
# ffmpeg 随包分发而不是让用户自己装：Windows 上没有包管理器依赖可声明，
# 而缺了它录音会在按下按钮那一刻才失败。取 shared 构建（可执行文件小，
# 库单独放），不取 static —— 后者两个 exe 加起来 140MB。
#
# ffmpeg 版本钉死在一个具体的 autobuild tag 上。用 latest 的话，同一个
# 源码树在不同日子会打出装着不同 ffmpeg 的包，那就谈不上可复现。

param(
    [string]$Version = "0.0.0",
    [string]$OutDir = "dist"
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

$FfmpegTag = "autobuild-2026-08-25-13-06"
$FfmpegZip = "ffmpeg-n9.0.1-6-g9d4ca21220-win64-lgpl-shared-9.0"
$FfmpegUrl = "https://github.com/BtbN/FFmpeg-Builds/releases/download/$FfmpegTag/$FfmpegZip.zip"

$staging = Join-Path $OutDir "Interview Studio"
Remove-Item -Recurse -Force $OutDir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $staging | Out-Null

Write-Host "==> 取 ffmpeg ($FfmpegTag)"
$cache = Join-Path $env:TEMP "$FfmpegZip.zip"
if (-not (Test-Path $cache)) {
    Invoke-WebRequest -Uri $FfmpegUrl -OutFile $cache
}
$unpacked = Join-Path $env:TEMP "ffmpeg-unpacked"
Remove-Item -Recurse -Force $unpacked -ErrorAction SilentlyContinue
Expand-Archive -Path $cache -DestinationPath $unpacked
$bin = Join-Path $unpacked "$FfmpegZip\bin"

Write-Host "==> 组装"
Copy-Item "target\release\interview-studio.exe" $staging
# ffplay 用不上，18MB 白搭。ffmpeg 和 ffprobe 都要，DLL 一个不能少。
Copy-Item (Join-Path $bin "ffmpeg.exe") $staging
Copy-Item (Join-Path $bin "ffprobe.exe") $staging
Copy-Item (Join-Path $bin "*.dll") $staging
Copy-Item "README.md" $staging
Copy-Item "LICENSE" (Join-Path $staging "LICENSE.txt")
Copy-Item (Join-Path $unpacked "$FfmpegZip\LICENSE.txt") (Join-Path $staging "LICENSE-ffmpeg.txt")

$size = [math]::Round((Get-ChildItem $staging -Recurse | Measure-Object Length -Sum).Sum / 1MB, 1)
Write-Host "    $size MB"
Get-ChildItem $staging | Format-Table Name, @{n = "MB"; e = { [math]::Round($_.Length / 1MB, 2) } }

Write-Host "==> ZIP"
$zip = Join-Path $OutDir "interview-studio-v$Version-x86_64-windows.zip"
Compress-Archive -Path $staging -DestinationPath $zip
Write-Host "    $zip"

Write-Host "==> MSI"
# heat 采集文件清单：ffmpeg 的 DLL 会随版本改名，手写清单必然过期
& $env:WIX_HEAT dir $staging `
    -cg AppFiles -gg -sfrag -srd -sreg -scom `
    -dr APPLICATIONFOLDER -var var.SourceDir `
    -out "$OutDir\files.wxs"
if ($LASTEXITCODE -ne 0) { throw "heat 失败" }

# -arch x64 不能省：heat 产出的组件不带 Win64 属性，由 candle 的 arch 决定。
# 默认是 x86，而 APPLICATIONFOLDER 挂在 ProgramFiles64Folder 下，
# light 会以 ICE80（32 位组件用了 64 位目录）拒绝。
& $env:WIX_CANDLE -nologo -arch x64 "-dVersion=$Version" "-dSourceDir=$staging" `
    -out "$OutDir\" "packaging\windows\main.wxs" "$OutDir\files.wxs"
if ($LASTEXITCODE -ne 0) { throw "candle 失败" }

$msi = Join-Path $OutDir "interview-studio-v$Version-x86_64.msi"
& $env:WIX_LIGHT -nologo -ext WixUIExtension `
    -cultures:en-us `
    -out $msi "$OutDir\main.wixobj" "$OutDir\files.wixobj"
if ($LASTEXITCODE -ne 0) { throw "light 失败" }

Remove-Item "$OutDir\*.wixobj", "$OutDir\files.wxs", "$OutDir\*.wixpdb" -ErrorAction SilentlyContinue
Remove-Item -Recurse -Force $staging

Write-Host "==> 产物"
Get-ChildItem $OutDir | Format-Table Name, @{n = "MB"; e = { [math]::Round($_.Length / 1MB, 2) } }
