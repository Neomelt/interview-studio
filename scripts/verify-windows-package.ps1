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
# 先收完整输出再取第一行：Select-Object -First 1 会提前终止管道并把原生命令
# 一起掐掉，$LASTEXITCODE 于是非零——那是管道的锅，不是 ffmpeg 的。
$ver = @(& $ff -hide_banner -version 2>&1)
if ($LASTEXITCODE -ne 0) { throw "随包的 ffmpeg 起不来: $($ver -join "`n")" }
Write-Host "    $($ver[0])"

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
$streams = @(& $fp -v error -select_streams a -show_entries stream=index -of csv=p=0 $out)
if ($LASTEXITCODE -ne 0) { throw "随包的 ffprobe 起不来" }
if ($streams.Count -ne 2) { throw "产物应当是双轨，实际 $($streams.Count) 轨" }
Write-Host "    双轨 MKV 产出正常"

Write-Host "==> 可执行文件的 Windows 资源"
$exe = Join-Path $root "interview-studio.exe"

# 子系统必须是 GUI(2) 而不是 CUI(3)：CUI 会在双击运行时额外弹一个控制台黑框。
# 直接读 PE 头，不用把程序跑起来。
$bytes = [System.IO.File]::ReadAllBytes($exe)
$pe = [BitConverter]::ToInt32($bytes, 0x3C)
# PE 签名(4) + COFF 头(20) = 可选头起点；Subsystem 在可选头偏移 68，
# PE32 和 PE32+ 在这个位置上一致。
$subsystem = [BitConverter]::ToUInt16($bytes, $pe + 24 + 68)
if ($subsystem -ne 2) { throw "子系统是 $subsystem，应为 2 (GUI)，否则启动会带控制台" }
Write-Host "    子系统 GUI，不会弹控制台"

# 资源段里有版本信息，就说明 build.rs 的资源编译跑了——图标是同一次编进去的
$info = (Get-Item $exe).VersionInfo
if ($info.ProductName -ne "Interview Studio") {
    throw "可执行文件没有嵌入资源（ProductName='$($info.ProductName)'），图标也就没进去"
}
Write-Host "    已嵌入资源：$($info.ProductName)"

Write-Host "==> MSI 数据库"
# 装出来对不对，看 MSI 自己的表最直接，不用真装一遍。
$installer = New-Object -ComObject WindowsInstaller.Installer
$db = $installer.GetType().InvokeMember(
    "OpenDatabase", "InvokeMethod", $null, $installer, @($msi.FullName, 0))

function Get-Column($sql) {
    $view = $db.GetType().InvokeMember("OpenView", "InvokeMethod", $null, $db, @($sql))
    $view.GetType().InvokeMember("Execute", "InvokeMethod", $null, $view, $null) | Out-Null
    $rows = @()
    while ($true) {
        $rec = $view.GetType().InvokeMember("Fetch", "InvokeMethod", $null, $view, $null)
        if (-not $rec) { break }
        $rows += $rec.GetType().InvokeMember("StringData", "GetProperty", $null, $rec, 1)
    }
    $view.GetType().InvokeMember("Close", "InvokeMethod", $null, $view, $null) | Out-Null
    , $rows
}

function Assert-Contains($actual, $expected, $what) {
    foreach ($e in $expected) {
        if ($actual -notcontains $e) {
            throw "$what 里缺 $e（实际有：$($actual -join ', ')）"
        }
    }
    Write-Host "    $what ✓ $($expected -join ', ')"
}

# 快捷方式：开始菜单和桌面各一个，都可由用户勾掉
Assert-Contains (Get-Column "SELECT Shortcut FROM Shortcut") `
    @("StartMenuLink", "DesktopLink") "Shortcut 表"

# 拆成独立 Feature 才勾得掉；主程序必须是 disallow-absent
Assert-Contains (Get-Column "SELECT Feature FROM Feature") `
    @("MainProgram", "StartMenuFeature", "DesktopFeature") "Feature 表"

# CustomizeDlg 是能勾功能的那一页，BrowseDlg 是改安装路径的那一页。
# 少哪个就等于对应的选项在界面上根本出不来。
Assert-Contains (Get-Column "SELECT Dialog FROM Dialog") `
    @("CustomizeDlg", "BrowseDlg") "Dialog 表"

# 「浏览…」要改的是这个目录，没设的话按钮会指向别处
Assert-Contains (Get-Column "SELECT Property FROM Property") `
    @("WIXUI_INSTALLDIR", "ARPPRODUCTICON") "Property 表"

# heat 采集的是产出 ZIP 的同一个目录，数量对不上就说明采集漏了东西。
# 装出来少一个 DLL，用户要到按下停止那一刻才知道。
$msiFiles = Get-Column "SELECT FileName FROM File"
$staged = @(Get-ChildItem $root -File).Count
if ($msiFiles.Count -ne $staged) {
    throw "MSI 里 $($msiFiles.Count) 个文件，包目录里 $staged 个，对不上"
}
Write-Host "    File 表 ✓ $($msiFiles.Count) 个文件，与包目录一致"

$configurable = Get-Column "SELECT Directory_ FROM Feature WHERE Feature='MainProgram'"
if ($configurable[0] -ne "APPLICATIONFOLDER") {
    throw "MainProgram 的 ConfigurableDirectory 是 '$($configurable[0])'，浏览按钮改不了安装路径"
}
Write-Host "    安装路径可改 ✓ APPLICATIONFOLDER"

[void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($db)
Remove-Item -Recurse -Force $check -ErrorAction SilentlyContinue
Write-Host "校验通过"
