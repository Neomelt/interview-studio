# 找到 WiX v3 的 heat/candle/light，把路径写进 GITHUB_ENV。
#
# runner 镜像里带没带 WiX 是会变的，不能假定。先找，找不到再装——
# 而不是无条件 choco install 白等一分钟。

$ErrorActionPreference = "Stop"

function Find-WixBin {
    $roots = @($env:WIX, "C:\Program Files (x86)", "C:\Program Files") |
        Where-Object { $_ -and (Test-Path $_) }
    foreach ($root in $roots) {
        # $env:WIX 直接指向安装根目录，下面就是 bin
        $direct = Join-Path $root "bin\candle.exe"
        if (Test-Path $direct) { return (Split-Path $direct -Parent) }

        Get-ChildItem $root -Filter "WiX Toolset*" -Directory -ErrorAction SilentlyContinue |
            Sort-Object Name -Descending |
            ForEach-Object {
                $bin = Join-Path $_.FullName "bin"
                if (Test-Path (Join-Path $bin "candle.exe")) { return $bin }
            } | Select-Object -First 1
    }
}

$bin = Find-WixBin
if (-not $bin) {
    Write-Host "==> 镜像里没有 WiX，装一个"
    choco install wixtoolset --version=3.11.2 -y --no-progress | Out-Null
    $bin = Find-WixBin
}
if (-not $bin) { throw "装完还是找不到 candle.exe" }

Write-Host "WiX: $bin"
foreach ($t in "heat", "candle", "light") {
    $exe = Join-Path $bin "$t.exe"
    if (-not (Test-Path $exe)) { throw "缺 $t.exe" }
    "WIX_$($t.ToUpper())=$exe" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
}
