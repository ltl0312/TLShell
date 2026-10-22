# 壁纸两端的实采样：白底与黑底各落一帧，用像素算真实对比度。
#
# 为什么不是录屏：窗口常驻桌面，屏幕区域被别的窗口盖住，截屏拍不到自己；
# 录屏工具也没装。落盘的是「推给 UpdateLayeredWindow 的同一份像素」，
# 换个合成底色就等于换了壁纸——这就是最差/最好条件的实测。
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root 'target\debug\tlshell.exe'
$work = Join-Path $env:TEMP 'aegis-wallpaper'
$evidence = Join-Path $root 'docs\aegis\evidence\vis\M7'
$log = Join-Path $env:LOCALAPPDATA 'TLShell\logs\tlshell.log'

New-Item -ItemType Directory -Force -Path $work, $evidence | Out-Null
Get-Process tlshell -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 600
$env:TLSHELL_LOG = 'info'

Add-Type -AssemblyName System.Drawing

function Capture([string]$name, [string]$bg) {
    $cfg = Join-Path $work "config-$name.toml"
    $bmp = Join-Path $work "$name.bmp"
    Remove-Item $cfg, $bmp -ErrorAction SilentlyContinue
    $proc = Start-Process -FilePath $exe -ArgumentList @(
        '--config', $cfg, '--dump-frame', $bmp, '--dump-bg', $bg
    ) -PassThru
    # 首帧落盘在 render 之后 1.5s 触发，而启动时还要等 shell 起来；留足余量。
    Start-Sleep -Seconds 5
    $proc | Stop-Process -Force
    Start-Sleep -Milliseconds 400
    if (-not (Test-Path $bmp)) { throw "没有落盘: $bmp" }
    $png = Join-Path $evidence "05-wallpaper-$name.png"
    $img = [System.Drawing.Image]::FromFile($bmp)
    $img.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)
    $img.Dispose()
    return $bmp
}

$white = Capture 'white' 'ffffff'
$black = Capture 'black' '000000'

function Lum([int]$r, [int]$g, [int]$b) {
    $f = {
        param($v)
        $s = $v / 255.0
        if ($s -le 0.03928) { return $s / 12.92 }
        return [math]::Pow((($s + 0.055) / 1.055), 2.4)
    }
    return 0.2126 * (& $f $r) + 0.7152 * (& $f $g) + 0.0722 * (& $f $b)
}

function Contrast($a, $b) {
    $la = Lum $a[0] $a[1] $a[2]
    $lb = Lum $b[0] $b[1] $b[2]
    $hi = [math]::Max($la, $lb); $lo = [math]::Min($la, $lb)
    return [math]::Round(($hi + 0.05) / ($lo + 0.05), 2)
}

function Sample([string]$path, [int]$x, [int]$y) {
    $img = [System.Drawing.Bitmap]::FromFile($path)
    # BMP 行序自下而上，转 PNG 后已正常；直接按 PNG 读。
    $c = $img.GetPixel($x, $y)
    $img.Dispose()
    return @($c.R, $c.G, $c.B)
}

# 取样点：
#   背景 = 右下空白处的一个像素；
#   正文 = 第一行文字带里**最亮**的像素（抗锯齿边缘会有中间色，取笔画中心）。
# 直接取固定坐标会打到背景上（对比度 1），这是实测踩过的坑。
foreach ($pair in @(@('white', $white), @('black', $black))) {
    $name = $pair[0]; $bmp = $pair[1]
    $img = [System.Drawing.Bitmap]::FromFile($bmp)
    $bgp = @($img.GetPixel(600, 300).R, $img.GetPixel(600, 300).G, $img.GetPixel(600, 300).B)
    $best = $null
    for ($y = 8; $y -lt 34; $y++) {
        for ($x = 8; $x -lt 220; $x++) {
            $c = $img.GetPixel($x, $y)
            $l = Lum $c.R $c.G $c.B
            if ($null -eq $best -or $l -gt $best[0]) {
                $best = @($l, $c.R, $c.G, $c.B)
            }
        }
    }
    $img.Dispose()
    $fgp = @($best[1], $best[2], $best[3])
    "== $name =="
    "背景 = $($bgp -join ',')  正文最亮 = $($fgp -join ',')  对比度 = $(Contrast $fgp $bgp)"
}
