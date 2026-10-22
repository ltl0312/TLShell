#Requires -Version 7.0
<#
.SYNOPSIS
    TLShell 分层全量测试：静态门禁 → 无界面自检 → 真机功能探针 → 行为负例，循环 N 轮。

.DESCRIPTION
    分四层，每层单独判定，失败不掩盖其它层的结果：

      A 静态门禁   cargo fmt --check / clippy -D warnings / test
      B 无界面自检 --selftest --report，逐项断言 ok
      C 真机探针   --probe-input --dump-frame，断言日志链路与落盘像素
      D 行为负例   单实例、--quit 空操作、坏参数、配置损坏

    判定分离：
      退出码 0 = 全通过；1 = 有 FAIL；2 = BLOCKED（环境缺失，如没有 cargo / 编译不了）。
    BLOCKED 不等于通过，报告里单独列。

    C 层只向自己的窗口 PostMessage，不注入系统级输入（不碰 SendInput/mouse_event），
    自检结束会把前台还给原来的窗口。文件级副作用探针用 %TEMP%\aegis-*.txt。

.PARAMETER Rounds
    连续跑几轮，默认 1。

.PARAMETER SkipBuild
    跳过 A 层的 cargo test/clippy（只跑 B/C/D），用于快速回归。
#>
[CmdletBinding()]
param(
    [int]$Rounds = 1,
    [switch]$SkipBuild,
    # 真实输入层：会移动光标、点击自己的窗口、临时抢前台。
    # 默认关闭 —— 常规回归绝不碰用户的鼠标键盘。
    [switch]$RealInput
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$exe = Join-Path $root 'target\debug\tlshell.exe'
$logPath = Join-Path $env:LOCALAPPDATA 'TLShell\logs\tlshell.log'
$evidenceDir = Join-Path $root 'docs\aegis\evidence\test\M6'
New-Item -ItemType Directory -Force -Path $evidenceDir | Out-Null

$script:results = @()
$script:layerFailures = @{ A = 0; B = 0; C = 0; D = 0; E = 0 }

function Add-Result {
    param(
        [string]$Layer,
        [string]$Name,
        [ValidateSet('PASS', 'FAIL', 'BLOCKED', 'SKIP')]
        [string]$Status,
        [string]$Detail = ''
    )
    $script:results += [pscustomobject]@{
        Layer  = $Layer
        Name   = $Name
        Status = $Status
        Detail = $Detail
    }
    if ($Status -eq 'FAIL') { $script:layerFailures[$Layer]++ }
    $colour = switch ($Status) {
        'PASS' { 'Green' }
        'FAIL' { 'Red' }
        'BLOCKED' { 'Yellow' }
        default { 'DarkGray' }
    }
    Write-Host ("[{0}] {1,-4} {2,-46} {3}" -f $Layer, $Status, $Name, $Detail) -ForegroundColor $colour
}

function Invoke-Capture {
    # GUI 子系统的可执行文件用 `&` 调用会立刻返回：进程还没退出，
    # 退出码与输出都是空的。必须 `Start-Process -Wait`。
    param([string]$File, [string[]]$Arguments, [int]$TimeoutSeconds = 30)
    $outFile = Join-Path $env:TEMP 'aegis-run.out.txt'
    $errFile = Join-Path $env:TEMP 'aegis-run.err.txt'
    Remove-Item $outFile, $errFile -ErrorAction SilentlyContinue
    $argList = @($Arguments | Where-Object { $null -ne $_ })
    $p = Start-Process -FilePath $File -ArgumentList $argList -PassThru -Wait:$false `
        -RedirectStandardOutput $outFile -RedirectStandardError $errFile `
        -Environment @{ TLSHELL_NO_MODAL = '1' }
    if (-not $p.WaitForExit($TimeoutSeconds * 1000)) {
        $p | Stop-Process -Force -ErrorAction SilentlyContinue
        return [pscustomobject]@{ Code = -1; Text = "超时 ${TimeoutSeconds}s 未退出"; TimedOut = $true }
    }
    $text = @()
    foreach ($f in @($outFile, $errFile)) {
        if (Test-Path $f) { $text += (Get-Content $f -Raw -ErrorAction SilentlyContinue) }
    }
    [pscustomobject]@{ Code = $p.ExitCode; Text = ($text -join "`n"); TimedOut = $false }
}

function Get-LogTail {
    param([int64]$Offset)
    if (-not (Test-Path $logPath)) { return '' }
    $stream = [System.IO.File]::Open($logPath, 'Open', 'Read', 'ReadWrite')
    try {
        if ($Offset -ge $stream.Length) { return '' }
        $null = $stream.Seek($Offset, 'Begin')
        $reader = New-Object System.IO.StreamReader($stream, [System.Text.Encoding]::UTF8)
        return $reader.ReadToEnd()
    }
    finally { $stream.Dispose() }
}

function Get-LogLength {
    if (-not (Test-Path $logPath)) { return 0 }
    return (Get-Item $logPath).Length
}

function Wait-ForLogLine {
    param([int64]$Offset, [string]$Pattern, [int]$TimeoutSeconds = 12)
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        $text = Get-LogTail -Offset $Offset
        if ($text -match $Pattern) { return $text }
        Start-Sleep -Milliseconds 200
    }
    return (Get-LogTail -Offset $Offset)
}

function Stop-AllInstances {
    Get-Process tlshell -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 300
}

# ---------------------------------------------------------------- A 层

# 每轮用一份不同的配置：不透明度、内边距、圆角、字号都随轮次变化。
# 目的是让「六轮」真的是六种参数组合，而不是同一套用例连跑六次。
function New-RoundConfig {
    param([int]$Round)
    $dir = Join-Path $env:TEMP ("aegis-round-$Round")
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $path = Join-Path $dir 'config.toml'
    $opacity = [math]::Round(0.80 + 0.02 * $Round, 2)
    $padding = 6 + 2 * $Round
    $radius = 4 + 2 * $Round
    $size = 12 + $Round
    @"
# 第 $Round 轮测试用配置
[window]
opacity = $opacity
padding = $padding
border_radius = $radius
width = 900
height = 420

[font]
size = $size
line_height = 1.25
"@ | Set-Content -Path $path -Encoding utf8
    return @{ Path = $path; Opacity = $opacity; Padding = $padding; Radius = $radius }
}

function Invoke-LayerA {
    if ($SkipBuild) {
        Add-Result A 'cargo fmt --check' SKIP '按 -SkipBuild 跳过'
        Add-Result A 'cargo clippy -D warnings' SKIP '按 -SkipBuild 跳过'
        Add-Result A 'cargo test' SKIP '按 -SkipBuild 跳过'
        return
    }

    $fmt = Invoke-Capture 'cargo' @('fmt', '--all', '--check')
    if ($fmt.Code -eq 0) {
        Add-Result A 'cargo fmt --check' PASS '格式一致'
    }
    else {
        $detail = (($fmt.Text -split "`n") | Select-Object -First 3) -join ' | '
        Add-Result A 'cargo fmt --check' FAIL $detail
    }

    $clippy = Invoke-Capture 'cargo' @('clippy', '--all-targets', '--offline', '--', '-D', 'warnings')
    if ($clippy.Code -eq 0) {
        Add-Result A 'cargo clippy -D warnings' PASS '无告警'
    }
    elseif ($clippy.Text -match 'error: could not compile') {
        Add-Result A 'cargo clippy -D warnings' FAIL '编译失败，见下方 B 层同因'
    }
    else {
        Add-Result A 'cargo clippy -D warnings' FAIL '存在告警'
    }

    $test = Invoke-Capture 'cargo' @('test', '--offline')
    if ($test.Code -eq 0 -and $test.Text -match 'test result: ok\. (\d+) passed') {
        $count = $Matches[1]
        Add-Result A 'cargo test' PASS "$count passed"
    }
    else {
        $detail = (($test.Text -split "`n") | Select-Object -Last 6) -join ' | '
        Add-Result A 'cargo test' FAIL $detail
    }
}

# ---------------------------------------------------------------- B 层

function Invoke-LayerB {
    if (-not (Test-Path $exe)) {
        Add-Result B 'selftest' BLOCKED '找不到 tlshell.exe（先跑 A 层编译）'
        return
    }

    $report = Join-Path $env:TEMP 'aegis-selftest.json'
    Remove-Item $report -ErrorAction SilentlyContinue
    $run = Invoke-Capture $exe @('--selftest', '--report', $report)
    $deadline = (Get-Date).AddSeconds(15)
    while (-not (Test-Path $report) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 150 }
    if (-not (Test-Path $report)) {
        Add-Result B 'selftest 产出报告' BLOCKED "未生成 JSON（exit=$($run.Code)）"
        return
    }

    $json = Get-Content $report -Raw | ConvertFrom-Json
    $total = $json.checks.Count
    $failed = @($json.checks | Where-Object { -not $_.ok })
    if ($json.passed -and $failed.Count -eq 0) {
        Add-Result B 'selftest 全部通过' PASS "$total 项"
    }
    else {
        $names = ($failed | ForEach-Object { $_.name }) -join ', '
        Add-Result B 'selftest 全部通过' FAIL "$($failed.Count)/$total 失败: $names"
    }
    Add-Result B 'selftest 退出码' $(if ($run.Code -eq 0) { 'PASS' } else { 'FAIL' }) "exit=$($run.Code)"
}

# ---------------------------------------------------------------- C 层

function Get-BmpPixels {
    param([string]$Path, [hashtable]$Points)
    Add-Type -AssemblyName System.Drawing
    $img = [System.Drawing.Bitmap]::FromFile($Path)
    try {
        $samples = [ordered]@{}
        foreach ($key in $Points.Keys) {
            $xy = $Points[$key]
            $c = $img.GetPixel($xy[0], $xy[1])
            $samples[$key] = @($c.R, $c.G, $c.B)
        }
        $samples['size'] = @($img.Width, $img.Height)
        return $samples
    }
    finally { $img.Dispose() }
}

function Invoke-LayerC {
    if (-not (Test-Path $exe)) {
        Add-Result C '真机探针' BLOCKED '找不到 tlshell.exe'
        return
    }

    Stop-AllInstances
    $frame = Join-Path $env:TEMP 'aegis-frame.bmp'
    Remove-Item $frame -ErrorAction SilentlyContinue
    # 文件级副作用探针：让 shell 真的写一个文件，证明 PTY 双向通。
    $probeFile = Join-Path $env:TEMP 'aegis-side-effect.txt'
    Remove-Item $probeFile -ErrorAction SilentlyContinue

    $offset = Get-LogLength
    $env:TLSHELL_LOG = 'debug'
    $proc = Start-Process -FilePath $exe -ArgumentList '--config', $script:roundConfig.Path, '--probe-input', '--dump-frame', $frame -PassThru

    try {
        $log = Wait-ForLogLine -Offset $offset -Pattern '输入自检\[结算\]' -TimeoutSeconds 20
        if (-not $log) {
            Add-Result C '进程启动' FAIL '20 秒内没有任何自检日志'
            return
        }

        $expectations = [ordered]@{
            '窗口创建为顶层'      = 'window hwnd=0x'
            'WorkerW 挂载已删除'  = $(if ($log -notmatch 'attach 回读') { 'window hwnd=0x' } else { 'NO_MATCH' })
            'shell 启动成功'      = '启动 shell:'
            '子进程环境白名单'    = '子进程环境: \d+ 项，启动目录'
            '字体度量就绪'        = '字体就绪:.*单元格 \d+x\d+'
            '点击命中并取焦点'    = '收到 WM_LBUTTONDOWN'
            '获得键盘焦点'        = '获得键盘焦点'
            '拖选扩展'            = '扩展选择 ->'
            'ASCII 写入 PTY'      = '自检写入 1 字节'
            'CJK 三字节写入'      = '自检写入 3 字节'
            '代理对四字节写入'    = '自检写入 4 字节'
            '结算键盘就绪'        = '输入自检\[结算\].*已写入 PTY 14 字节'
            '前台已归还'          = '前台已还给|没有可归还的前台|已不存在，跳过归还'
        }
        foreach ($name in $expectations.Keys) {
            $pattern = $expectations[$name]
            if ($pattern -eq 'NO_MATCH') {
                Add-Result C $name FAIL '日志里仍有 attach 回读'
            }
            elseif ($log -match $pattern) {
                Add-Result C $name PASS $pattern
            }
            else {
                Add-Result C $name FAIL "缺少: $pattern"
            }
        }

        # 启动可见性：这是「看不见也点不到」那个缺陷的回归断言。
        # 正常路径（用户从资源管理器启动）会拿到前台；后台启动被前台锁挡住时
        # 必须保持 topmost 抬升，命中自检要落在自己窗口上。
        $focusedOk = $log -match '启动取焦点: foreground=true'
        $raisedOk = $log -match '命中自检: \([^)]*\) -> 自己:TLShellWindow'
        Add-Result C '启动即可见可点' $(if ($focusedOk -or $raisedOk) { 'PASS' } else { 'FAIL' }) `
            "取前台=$focusedOk 顶层抬升=$raisedOk"

        # 首帧落盘 + 像素断言（白底最差条件，逐点对应解析解）。
        $frameLog = Wait-ForLogLine -Offset $offset -Pattern '首帧已落盘' -TimeoutSeconds 10
        if (-not (Test-Path $frame)) {
            Add-Result C '首帧落盘' FAIL '没有生成 BMP'
        }
        else {
            Add-Result C '首帧落盘' PASS (Split-Path -Leaf $frame)
            $points = @{
                'background'   = @(450, 300)
                'statusbar'    = @(450, 405)
                'corner'       = @(1, 1)
                'top_edge'     = @(450, 1)
            }
            $px = Get-BmpPixels -Path $frame -Points $points
            if ($px.size[0] -eq 900 -and $px.size[1] -eq 420) {
                Add-Result C 'BMP 尺寸' PASS '900x420'
            }
            else {
                Add-Result C 'BMP 尺寸' FAIL "$($px.size[0])x$($px.size[1])"
            }
            # 空白处 = 主题底色 #1e1e2e 按本轮 opacity 叠在白底上。
            # 期望值必须按当轮 opacity 算：写死 57/57/71 只对 0.88 成立，
            # 换一轮参数就会误报（本轮就踩过）。
            $a = $script:roundConfig.Opacity
            $wantR = [int][math]::Round(30 * $a + 255 * (1 - $a))
            $wantB = [int][math]::Round(46 * $a + 255 * (1 - $a))
            $bg = $px['background']
            $bgOk = [math]::Abs($bg[0] - $wantR) -le 2 -and [math]::Abs($bg[1] - $wantR) -le 2 -and [math]::Abs($bg[2] - $wantB) -le 2
            Add-Result C '通道序 R/B 未颠倒' $(if ($bgOk) { 'PASS' } else { 'FAIL' }) "bg=R$($bg[0]) G$($bg[1]) B$($bg[2])，期望 $wantR/$wantR/$wantB（opacity=$a）"
            # 圆角外 = 完全透明 → 合成成白底。
            $corner = $px['corner']
            $cornerOk = $corner[0] -eq 255 -and $corner[1] -eq 255 -and $corner[2] -eq 255
            Add-Result C '圆角外透明(V-19)' $(if ($cornerOk) { 'PASS' } else { 'FAIL' }) "corner=R$($corner[0]) G$($corner[1]) B$($corner[2])"
            $edge = $px['top_edge']
            Add-Result C '圆角内不透明' $(if ($edge[0] -lt 200) { 'PASS' } else { 'FAIL' }) "top_edge=R$($edge[0]) G$($edge[1]) B$($edge[2])"
        }

        # 单实例：第二个进程必须立刻退出。
        $second = Invoke-Capture $exe @()
        Add-Result C '单实例判重' $(if ($second.Code -eq 0) { 'PASS' } else { 'FAIL' }) "第二个实例 exit=$($second.Code)"

        # 设置面板：往自己的窗口投 F2 → ←→ 调整 → Enter 保存，验证值真的落盘。
        # 只 PostMessage，不注入系统级输入。
        $hwndFile = Join-Path $env:APPDATA 'TLShell\hwnd.txt'
        if (Test-Path $hwndFile) {
            Add-Type -Namespace Aegis -Name Key -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool PostMessageW(System.IntPtr h, uint m, System.IntPtr w, System.IntPtr l);
'@ -ErrorAction SilentlyContinue
            $hwnd = [IntPtr]([int64](Get-Content $hwndFile -Raw).Trim())
            $key = {
                param([int]$vk)
                [void][Aegis.Key]::PostMessageW($hwnd, 0x0100, [IntPtr]$vk, [IntPtr]0x00010001)
                [void][Aegis.Key]::PostMessageW($hwnd, 0x0101, [IntPtr]$vk, [IntPtr]0xC0010001)
            }
            $offset2 = Get-LogLength
            & $key 0x71
            $opened = Wait-ForLogLine -Offset $offset2 -Pattern '打开设置面板' -TimeoutSeconds 6
            Add-Result C '设置面板打开' $(if ($opened) { 'PASS' } else { 'FAIL' }) 'F2'
            & $key 0x27; & $key 0x27
            $offset3 = Get-LogLength
            & $key 0x0D
            $saved = Wait-ForLogLine -Offset $offset3 -Pattern '设置已写入' -TimeoutSeconds 6
            Add-Result C '设置写入落盘' $(if ($saved) { 'PASS' } else { 'FAIL' }) 'Enter'
            $text = Get-Content $script:roundConfig.Path -Raw
            $m = [regex]::Match($text, '(?m)^opacity\s*=\s*([0-9.]+)')
            $want = [math]::Round($script:roundConfig.Opacity + 0.04, 2)
            $got = if ($m.Success) { [double]$m.Groups[1].Value } else { -1 }
            Add-Result C '设置值真的改了两档' $(if ([math]::Abs($got - $want) -le 0.001) { 'PASS' } else { 'FAIL' }) "opacity=$got 期望 $want"

            # 唤起热键：沉底/被遮挡时唯一的自救路径，处理器必须真的跑。
            $offset4 = Get-LogLength
            [void][Aegis.Key]::PostMessageW($hwnd, 0x0312, [IntPtr]3, [IntPtr]0)
            $raised = Wait-ForLogLine -Offset $offset4 -Pattern '收到唤起热键' -TimeoutSeconds 6
            Add-Result C '唤起热键生效' $(if ($raised) { 'PASS' } else { 'FAIL' }) 'Ctrl+Alt+T'

        }
        else {
            Add-Result C '设置面板打开' BLOCKED '没有 hwnd.txt'
        }
    }
    finally {
        Remove-Item Env:\TLSHELL_LOG -ErrorAction SilentlyContinue
        if (-not $proc.HasExited) { $proc | Stop-Process -Force -ErrorAction SilentlyContinue }
        Stop-AllInstances
    }
}

# ---------------------------------------------------------------- D 层

function Invoke-LayerD {
    if (-not (Test-Path $exe)) {
        Add-Result D '行为负例' BLOCKED '找不到 tlshell.exe'
        return
    }
    Stop-AllInstances

    # 没有实例时 --quit 退 0（不能弹窗、不能报错）。
    $quit = Invoke-Capture $exe @('--quit')
    Add-Result D '--quit 无实例退 0' $(if ($quit.Code -eq 0) { 'PASS' } else { 'FAIL' }) "exit=$($quit.Code)"

    # 有实例时 --quit 必须真的把它关掉（这条曾经是「空操作」的假通过）。
    $proc = Start-Process -FilePath $exe -ArgumentList '--config', $script:roundConfig.Path -PassThru
    Start-Sleep -Seconds 3
    $aliveBefore = -not $proc.HasExited
    $null = Invoke-Capture $exe @('--quit')
    $closed = $proc.WaitForExit(6000)
    Add-Result D '--quit 真的退出实例' $(if ($aliveBefore -and $closed) { 'PASS' } else { 'FAIL' }) "aliveBefore=$aliveBefore closed=$closed"
    Stop-AllInstances

    # 壁纸两端：白底与黑底各落一帧，正文对比度都必须 ≥ 4.5:1。
    # 没有录屏工具链也能验：落盘的是推给 UpdateLayeredWindow 的同一份像素，
    # 换合成底色就等于换壁纸。
    $wall = & (Join-Path $PSScriptRoot 'verify-wallpaper.ps1') 2>&1
    $ratios = @()
    foreach ($line in $wall) {
        $m = [regex]::Match($line, '对比度 = ([0-9.]+)')
        if ($m.Success) { $ratios += [double]$m.Groups[1].Value }
    }
    $worst = if ($ratios.Count -gt 0) { ($ratios | Measure-Object -Minimum).Minimum } else { 0 }
    Add-Result D '白/黑壁纸对比度 ≥ 4.5:1' $(if ($ratios.Count -ge 2 -and $worst -ge 4.5) { 'PASS' } else { 'FAIL' }) "测得 $($ratios -join '/')，最差 $worst"

    # 非管理员时 --restart-unelevated 必须是「无需降权」而不是报错。
    $restart = Invoke-Capture $exe @('--restart-unelevated')
    Add-Result D '降权重启在非提权下退 0' $(if ($restart.Code -eq 0) { 'PASS' } else { 'FAIL' }) "exit=$($restart.Code)"

    # 坏参数：必须拒绝启动（不静默忽略）。
    $bad = Invoke-Capture $exe @('--nonsense-flag')
    Add-Result D '未知参数被拒绝' $(if ($bad.Code -ne 0) { 'PASS' } else { 'FAIL' }) "exit=$($bad.Code)"

    # 损坏的配置文件：回退默认值启动，不崩。
    $cfgDir = Join-Path $env:APPDATA 'TLShell'
    $cfgPath = Join-Path $cfgDir 'config.toml'
    $backup = Join-Path $env:TEMP 'aegis-config-backup.toml'
    $hadConfig = Test-Path $cfgPath
    if ($hadConfig) { Copy-Item $cfgPath $backup -Force }
    try {
        New-Item -ItemType Directory -Force -Path $cfgDir | Out-Null
        Set-Content -Path $cfgPath -Value 'this is not toml [[[' -Encoding utf8
        $offset = Get-LogLength
        $proc = Start-Process -FilePath $exe -ArgumentList '--dump-frame', (Join-Path $env:TEMP 'aegis-badcfg.bmp') -PassThru -Environment @{ TLSHELL_NO_MODAL = '1' }
        Start-Sleep -Seconds 4
        $alive = -not $proc.HasExited
        Add-Result D '坏配置仍能启动' $(if ($alive) { 'PASS' } else { 'FAIL' }) "alive=$alive"
        if (-not $proc.HasExited) { $proc | Stop-Process -Force -ErrorAction SilentlyContinue }
    }
    finally {
        if ($hadConfig) { Copy-Item $backup $cfgPath -Force } else { Remove-Item $cfgPath -ErrorAction SilentlyContinue }
        Stop-AllInstances
    }

    # 缺 USERPROFILE 必须拒绝启动（不能静默继承自身 cwd）。
    # 用 ProcessStartInfo 才能真正把变量从子进程环境里删掉：PowerShell 里
    # `$env:USERPROFILE = $null` 只是清空自身可见值，Start-Process 仍会继承。
    $offset = Get-LogLength
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.UseShellExecute = $false
    $psi.Arguments = "--dump-frame `"$(Join-Path $env:TEMP 'aegis-noprofile.bmp')`""
    [void]$psi.EnvironmentVariables.Remove('USERPROFILE')
    $psi.EnvironmentVariables['TLSHELL_NO_MODAL'] = '1'
    $psi.EnvironmentVariables['TLSHELL_LOG'] = 'info'
    $proc = [System.Diagnostics.Process]::Start($psi)
    $refused = $proc.WaitForExit(8000)
    if (-not $refused) { $proc.Kill() }
    $code = if ($refused) { $proc.ExitCode } else { -1 }
    Add-Result D '缺 USERPROFILE 拒绝启动' $(if ($refused -and $code -ne 0) { 'PASS' } else { 'FAIL' }) "exited=$refused exit=$code"
    Stop-AllInstances
}

# ---------------------------------------------------------------- 报告

function Write-RoundReport {
    param([int]$Round, [string]$Path)

    $lines = @()
    $lines += "# 第 $Round 轮全量测试（$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss zzz')）"
    if ($script:roundConfig) {
        $lines += ""
        $lines += "本轮参数：opacity=$($script:roundConfig.Opacity) padding=$($script:roundConfig.Padding) radius=$($script:roundConfig.Radius)"
    }
    $lines += ''
    $lines += "环境：Windows $([System.Environment]::OSVersion.Version)，PowerShell $($PSVersionTable.PSVersion)。"
    $lines += ''
    $lines += '| 层 | 项 | 结果 | 说明 |'
    $lines += '|---|---|---|---|'
    foreach ($r in $script:results) {
        $detail = ($r.Detail -replace '\|', '\|' -replace "`r?`n", ' ')
        $lines += "| $($r.Layer) | $($r.Name) | $($r.Status) | $detail |"
    }
    $lines += ''
    $counts = $script:results | Group-Object Status | ForEach-Object { "$($_.Name)=$($_.Count)" }
    $lines += "汇总：$($counts -join '，')"
    $lines += ''
    $lines += 'C 层只向自己的窗口 `PostMessage`，不注入系统级输入；自检结束把前台还给原窗口。'
    Set-Content -Path $Path -Value ($lines -join "`n") -Encoding utf8
}

# ---------------------------------------------------------------- E 层（可选）

function Invoke-LayerE {
    if (-not (Test-Path $exe)) {
        Add-Result E '真实输入链路' BLOCKED '找不到 tlshell.exe'
        return
    }
    Stop-AllInstances
    $offset = Get-LogLength
    $env:TLSHELL_LOG = 'debug'
    $proc = Start-Process -FilePath $exe -ArgumentList '--config', $script:roundConfig.Path, '--probe-real' -PassThru
    try {
        $log = Wait-ForLogLine -Offset $offset -Pattern '真实输入结算' -TimeoutSeconds 25
        if (-not $log) {
            Add-Result E '真实输入链路' FAIL '25 秒内没有结算日志'
            return
        }
        $checks = [ordered]@{
            '真实点击命中自己' = '真实点击@.*moved=true down=true up=true'
            '真实点击拿到焦点' = '点击取焦点: foreground=true focus_set=true keyboard_ready=true'
            '真实拖选出选区'   = '扩展选择 -> line=\d+ col=\d+ 选区=Some'
            '真实按键写进 PTY' = '真实输入结算: 点击=\d+ 按键=[1-9]\d* 写入 PTY=[1-9]\d* 字节'
            '前台已归还'       = '前台已还给|没有可归还的前台'
        }
        foreach ($name in $checks.Keys) {
            $pattern = $checks[$name]
            Add-Result E $name $(if ($log -match $pattern) { 'PASS' } else { 'FAIL' }) $pattern
        }
    }
    finally {
        if (-not $proc.HasExited) {
            Invoke-Capture $exe @('--quit') | Out-Null
            if (-not $proc.WaitForExit(6000)) { $proc.Kill() }
        }
    }
}

$overallFail = 0
$overallBlocked = 0

for ($round = 1; $round -le $Rounds; $round++) {
    Write-Host ""
    Write-Host "===== 第 $round/$Rounds 轮 =====" -ForegroundColor Cyan
    $script:results = @()
    $script:layerFailures = @{ A = 0; B = 0; C = 0; D = 0; E = 0 }

    $script:roundConfig = New-RoundConfig -Round $round
    Invoke-LayerA
    Invoke-LayerB
    Invoke-LayerC
    Invoke-LayerD
    if ($RealInput) { Invoke-LayerE }

    $reportPath = Join-Path $evidenceDir ("round-{0}.md" -f $round)
    Write-RoundReport -Round $round -Path $reportPath
    Write-Host "报告：$reportPath"

    $fail = @($script:results | Where-Object Status -eq 'FAIL').Count
    $blocked = @($script:results | Where-Object Status -eq 'BLOCKED').Count
    $overallFail += $fail
    $overallBlocked += $blocked
    $pass = @($script:results | Where-Object Status -eq 'PASS').Count
    Write-Host "第 $round 轮：PASS=$pass FAIL=$fail BLOCKED=$blocked" -ForegroundColor $(if ($fail -eq 0) { 'Green' } else { 'Red' })
}

Stop-AllInstances

if ($overallFail -gt 0) {
    Write-Host "总结：有 $overallFail 项 FAIL（退出码 1）" -ForegroundColor Red
    exit 1
}
if ($overallBlocked -gt 0) {
    Write-Host "总结：$overallBlocked 项 BLOCKED，环境缺失不算通过（退出码 2）" -ForegroundColor Yellow
    exit 2
}
Write-Host "总结：全部通过" -ForegroundColor Green
exit 0
