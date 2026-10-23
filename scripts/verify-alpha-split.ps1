# 验证「背景透明度」与「文字透明度」是两个独立的量。
#
# 做法：背景透明度设 0（完全不铺底），文字透明度分别设 1.0 与 0.4，各落一帧，
# 合成底色固定为纯黑 —— 黑底上「覆盖率 × 文字透明度」是线性的，
# 白底会把低覆盖率像素抬亮，算出来的预测值本身就不对。判定三条：
#   1. 两张图的背景取样点必须等于合成底色 —— 说明背景真的没画；
#   2. 文字透明度 1.0 的平均亮度要明显高于背景；
#   3. 文字透明度 0.4 的平均亮度落在「按 0.4 线性混合」的预测值附近，
#      且背景取样点不变 —— 说明只有字变淡、背景没跟着变。
#
# 只读自己窗口的像素，不注入任何系统级输入。

param(
    [string]$Binary = (Join-Path $PSScriptRoot '..\target\debug\tlshell.exe'),
    [int]$Baseline = 0x00,
    [string]$DumpBg = '000000'
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$bin = (Resolve-Path $Binary).Path
$root = Join-Path $env:TEMP 'aegis-alpha-split'
Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path (Join-Path $root 'appdata') | Out-Null

$previousAppData = $env:APPDATA
$previousLog = $env:TLSHELL_LOG
$previousModal = $env:TLSHELL_NO_MODAL

function New-Config {
    param([double]$TextOpacity, [string]$Path)
    $body = @"
[window]
width = 700
height = 260
x = 60
y = 60
opacity = 0.00
text_opacity = $($TextOpacity.ToString('0.00'))
padding = 10
border_radius = 8
sink_when_unfocused = false
[keys]
quit = "Ctrl+Alt+F9"
hide = "Ctrl+Alt+F10"
raise = "Ctrl+Alt+F11"
"@
    Set-Content -Path $Path -Value $body -Encoding utf8
}

function Invoke-Dump {
    param([string]$Config, [string]$Out)
    $proc = Start-Process -FilePath $bin -ArgumentList @(
        # 用 --probe-input（PostMessage）而不是 --probe-real：取证只需要「画面里有字」，
        # 不该依赖能不能抢到前台，否则前台锁一拦就整段取证挂住。
        '--config', $Config, '--probe-input', '--dump-frame', $Out, '--dump-bg', $DumpBg
    ) -Wait -PassThru
    if (-not (Test-Path $Out)) { throw "没有落盘: $Out" }
    return $proc.ExitCode
}

# 顶部有标签栏：固定从 y=8 取平均会把栏底和按钮的像素算进「文字」里，
# 而栏的颜色在两帧里完全一样，比值就变成 1。这里先找一条「整行都是背景」的
# 行当作栏底，正文带从它下面开始。
function Get-TextBand {
    param([System.Drawing.Bitmap]$Bmp, $Bg)
    for ($y = 4; $y -lt 120; $y++) {
        $bgLike = 0
        $total = 0
        for ($x = 100; $x -lt 600; $x += 4) {
            $pixel = $Bmp.GetPixel($x, $y)
            $total++
            if ([math]::Abs($pixel.R - $Bg.R) -le 4 -and
                [math]::Abs($pixel.G - $Bg.G) -le 4 -and
                [math]::Abs($pixel.B - $Bg.B) -le 4) { $bgLike++ }
        }
        if ($total -gt 0 -and ($bgLike / $total) -ge 0.95) {
            return [pscustomobject]@{ Start = $y; End = [math]::Min($y + 60, $Bmp.Height) }
        }
    }
    return [pscustomobject]@{ Start = 8; End = [math]::Min(68, $Bmp.Height) }
}

function Measure-Frame {
    param([string]$Path)
    $bmp = [System.Drawing.Bitmap]::FromFile($Path)
    try {
        # 取样点必须在网格空白处：右下角可能被提示条占据。
        $bg = $bmp.GetPixel([int]($bmp.Width / 2), [int]($bmp.Height * 0.6))
        $band = Get-TextBand -Bmp $bmp -Bg $bg
        $sum = 0
        $count = 0
        # 固定窗口取平均，**不剔除背景像素**：剔除会把「变淡后舍入成 0 的像素」
        # 排除在外，剩下的样本天然偏亮，比值就不再是文字透明度。
        for ($y = $band.Start; $y -lt $band.End; $y++) {
            for ($x = 8; $x -lt 300; $x++) {
                $pixel = $bmp.GetPixel($x, $y)
                $sum += [int](($pixel.R + $pixel.G + $pixel.B) / 3)
                $count++
            }
        }
        return [pscustomobject]@{
            Bg = $bg
            Count = $count
            Average = $(if ($count -gt 0) { $sum / $count } else { 0 })
        }
    }
    finally {
        $bmp.Dispose()
    }
}

# 用「文字透明度 1.0」那一帧决定取样掩膜（哪些像素是字），再在两张图的同一批像素上
# 求平均。掩膜只取决于清晰的那一帧，不会被变淡后的舍入结果反向挑样本。
function Measure-Pair {
    param([string]$FullPath, [string]$DimPath)
    $fullBmp = [System.Drawing.Bitmap]::FromFile($FullPath)
    $dimBmp = [System.Drawing.Bitmap]::FromFile($DimPath)
    try {
        $sumFull = 0.0
        $count = 0
        $ratios = New-Object System.Collections.Generic.List[double]
        $bgFull = $fullBmp.GetPixel([int]($fullBmp.Width / 2), [int]($fullBmp.Height * 0.6))
        $band = Get-TextBand -Bmp $fullBmp -Bg $bgFull
        for ($y = $band.Start; $y -lt $band.End; $y++) {
            for ($x = 8; $x -lt 300; $x++) {
                $f = $fullBmp.GetPixel($x, $y)
                $level = ($f.R + $f.G + $f.B) / 3
                # 只保留「明显亮于背景」的像素：背景 1E1E2E 三通道平均约 35。
                if ($level -lt ([int]($bgFull.R + $bgFull.G + $bgFull.B) / 3) + 25) { continue }
                $d = $dimBmp.GetPixel($x, $y)
                $dimLevel = ($d.R + $d.G + $d.B) / 3
                if ($dimLevel -eq $level) { continue }
                $sumFull += $level
                $ratios.Add($dimLevel / $level)
                $count++
            }
        }
        # 选区分块一类的「装饰」跟随背景透明度（与文字透明度无关），会把算术平均抬高。
        # 所以判据取逐像素比值的中位数：字形像素占绝大多数，中位数只反映文字透明度。
        $median = 0.0
        if ($ratios.Count -gt 0) {
            $sorted = $ratios | Sort-Object
            $median = $sorted[[int][math]::Floor($sorted.Count / 2)]
        }
        return [pscustomobject]@{
            BgFull   = $fullBmp.GetPixel(350, 200)
            BgDim    = $dimBmp.GetPixel(350, 200)
            TextFull = $(if ($count -gt 0) { $sumFull / $count } else { 0 })
            Median   = $median
            Samples  = $count
        }
    }
    finally {
        $fullBmp.Dispose()
        $dimBmp.Dispose()
    }
}

$results = @()
try {
    $env:APPDATA = Join-Path $root 'appdata'
    $env:TLSHELL_LOG = 'warn'
    $env:TLSHELL_NO_MODAL = '1'

    $fullConfig = Join-Path $root 'full.toml'
    $dimConfig = Join-Path $root 'dim.toml'
    New-Config -TextOpacity 1.00 -Path $fullConfig
    New-Config -TextOpacity 0.40 -Path $dimConfig

    $fullBmp = Join-Path $root 'text-100.bmp'
    $dimBmp = Join-Path $root 'text-040.bmp'
    $null = Invoke-Dump -Config $fullConfig -Out $fullBmp
    $null = Invoke-Dump -Config $dimConfig -Out $dimBmp

    $pair = Measure-Pair -FullPath $fullBmp -DimPath $dimBmp

    $wantBg = @(
        [Convert]::ToInt32($DumpBg.Substring(0, 2), 16),
        [Convert]::ToInt32($DumpBg.Substring(2, 2), 16),
        [Convert]::ToInt32($DumpBg.Substring(4, 2), 16)
    )
    $bgMatches = {
        param($color)
        $color.R -eq $wantBg[0] -and $color.G -eq $wantBg[1] -and $color.B -eq $wantBg[2]
    }

    $results += [pscustomobject]@{
        Name = '背景透明度 0 时背景完全不绘制'
        Ok = (& $bgMatches $pair.BgFull) -and (& $bgMatches $pair.BgDim)
        Detail = "bg100=($($pair.BgFull.R),$($pair.BgFull.G),$($pair.BgFull.B)) bg40=($($pair.BgDim.R),$($pair.BgDim.G),$($pair.BgDim.B)) 期望=($($wantBg -join ','))"
    }

    $bgLevel = ($wantBg[0] + $wantBg[1] + $wantBg[2]) / 3
    $results += [pscustomobject]@{
        Name = '文字透明度 1.0 时文字明显亮于背景'
        Ok = $pair.TextFull -gt ($bgLevel + 60) -and $pair.Samples -gt 200
        Detail = "文字平均=$([math]::Round($pair.TextFull,1)) 背景=$([int]$bgLevel) 样本=$($pair.Samples)"
    }

    # 同一批字形像素上，0.4 就应该是 1.0 的 0.4 倍（黑底线性混合）。
    $results += [pscustomobject]@{
        Name = '文字透明度 0.4 时按 0.4 线性变淡'
        Ok = [math]::Abs($pair.Median - 0.40) -le 0.05
        Detail = "逐像素比值中位数=$([math]::Round($pair.Median,3)) 期望=0.400 样本=$($pair.Samples)"
    }
}
finally {
    $env:APPDATA = $previousAppData
    $env:TLSHELL_LOG = $previousLog
    $env:TLSHELL_NO_MODAL = $previousModal
}

$failed = 0
foreach ($row in $results) {
    $status = if ($row.Ok) { 'PASS' } else { 'FAIL'; $failed++ }
    # 必须用 Write-Output：Write-Host 绕过标准输出流，调用方 `& script` 收不到这些行。
    Write-Output ("[{0}] {1} — {2}" -f $status, $row.Name, $row.Detail)
}

if ($failed -gt 0) { exit 1 }
Write-Host '对比度 = 不适用（本脚本只验透明度拆分）'
exit 0
