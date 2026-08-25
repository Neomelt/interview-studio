# 打出来了 ≠ 装得对。ZIP 里少一个 DLL，用户要到按下「停止」那一刻才会发现。
# 这里把「必须存在」的东西逐个点名，并且真的把 ffmpeg 跑一次。

$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

$zip = Get-ChildItem "dist\*.zip" | Select-Object -First 1
$msi = Get-ChildItem "dist\*.msi" | Select-Object -First 1
if (-not $zip) { throw "没有产出 ZIP" }
if (-not $msi) { throw "没有产出 MSI" }
Write-Host "ZIP $([math]::Round($zip.Length/1MB,1)) MB   MSI $([math]::Round($msi.Length/1MB,1)) MB"

$check = Join-Path $env:TEMP "is-verify"
Remove-Item -Recurse -Force $check -ErrorAction SilentlyContinue
Expand-Archive -Path $zip.FullName -DestinationPath $check
$root = Join-Path $check "Interview Studio"

$required = @(
    "interview-studio.exe", "ffmpeg.exe", "ffprobe.exe",
    "avcodec-63.dll", "avformat-63.dll", "avutil-61.dll",
    "avfilter-12.dll", "swresample-7.dll",
    "README.md", "LICENSE.txt", "LICENSE-ffmpeg.txt"
)
foreach ($f in $required) {
    if (-not (Test-Path (Join-Path $root $f))) { throw "包里缺 $f" }
}
if (Test-Path (Join-Path $root "ffplay.exe")) { throw "ffplay.exe 用不上，不该在包里" }

# 光是文件在不够：DLL 版本对不上时 ffmpeg.exe 会直接起不来。
# 用的是随包那份的绝对路径，不是 PATH 上的——正是程序运行时的调用方式。
Write-Host "==> 跑一下随包的 ffmpeg"
$ff = Join-Path $root "ffmpeg.exe"
$ver = & $ff -hide_banner -version 2>&1 | Select-Object -First 1
if ($LASTEXITCODE -ne 0) { throw "随包的 ffmpeg 起不来: $ver" }
Write-Host "    $ver"

# 真正走一遍产物格式：两路裸 PCM -> 双轨 FLAC 的 MKV，正是 Windows 录音
# 停止时做的事。跑得通才说明随包的这份 ffmpeg 带着我们需要的编码器。
Write-Host "==> 用随包的 ffmpeg 走一遍双轨封装"
$a = Join-Path $check "a.pcm"
$b = Join-Path $check "b.pcm"
$out = Join-Path $check "t.mkv"
[byte[]]$silence = New-Object byte[] (48000 * 2 * 2)  # 1 秒 48kHz 单声道 s16
[System.IO.File]::WriteAllBytes($a, $silence)
[System.IO.File]::WriteAllBytes($b, $silence)

& $ff -hide_banner -loglevel error -y `
    -f s16le -ar 48000 -ac 1 -i $a `
    -f s16le -ar 48000 -ac 1 -i $b `
    -map 0:a -map 1:a -c:a flac -sample_fmt s16 $out
if ($LASTEXITCODE -ne 0) { throw "双轨封装失败" }

$fp = Join-Path $root "ffprobe.exe"
$n = (& $fp -v error -select_streams a -show_entries stream=index -of csv=p=0 $out | Measure-Object).Count
if ($n -ne 2) { throw "产物应当是双轨，实际 $n 轨" }
Write-Host "    双轨 MKV 产出正常"

Remove-Item -Recurse -Force $check -ErrorAction SilentlyContinue
Write-Host "校验通过"
