# 标签栏 / 清屏按钮的真机验收。
#
# 只往**自己**的窗口 PostMessage，不注入系统级输入（不碰真实鼠标键盘），
# 因此不会抢用户正在用的窗口。
#
# 判据全部来自程序自己写的日志：
#   新建标签 / 切到标签 / 关闭标签 / 清屏 / 点击标签栏
param(
    [string]$Exe = '.\target\debug\tlshell.exe'
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$exe = (Resolve-Path $Exe).Path
$root = Join-Path $env:TEMP 'aegis-tabs-verify'
if (Test-Path $root) { Remove-Item -Recurse -Force $root }
$appData = Join-Path $root 'appdata'
$localData = Join-Path $root 'localappdata'
New-Item -ItemType Directory -Force -Path $appData, $localData | Out-Null

$config = Join-Path $root 'config.toml'
@'
shell = "pwsh.exe"

[window]
width = 900
height = 360
x = 40
y = 40
opacity = 0.92
text_opacity = 1.0
sink_when_unfocused = true

[font]
family = "Cascadia Mono"
size = 14.0
line_height = 1.25
'@ | Set-Content -Encoding utf8 $config

$logPath = Join-Path $localData 'TLShell\logs\tlshell.log'

Add-Type -Namespace Aegis -Name Msg -MemberDefinition @'
[DllImport("user32.dll", SetLastError = true)]
public static extern bool PostMessageW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

[DllImport("user32.dll", SetLastError = true)]
public static extern bool SetProcessDpiAwarenessContext(IntPtr value);

[DllImport("user32.dll")] public static extern IntPtr GetThreadDpiAwarenessContext();
[DllImport("user32.dll")] public static extern int GetAwarenessFromDpiAwarenessContext(IntPtr value);
'@

# DPI 感知必须在这里打开：PostMessage 的 WM_LBUTTONDOWN 坐标会按两个进程的
# DPI 感知上下文换算，pwsh 默认是不感知的，130 DPI 下 (11,10) 到达程序会变成
# (16,14)（×1.35）。不打开这一行，脚本点的是「离目标一格的地方」。
# 注意：DPI 感知是进程级、一次性的设置。run-all.ps1 用 `&` 在同一进程里反复调
# 本脚本，第二次调用必然返回 false（已经设过了）—— 那不是错误，回读上下文判断。
if (-not [Aegis.Msg]::SetProcessDpiAwarenessContext([IntPtr](-4))) {
    $ctx = [Aegis.Msg]::GetThreadDpiAwarenessContext()
    $awareness = [Aegis.Msg]::GetAwarenessFromDpiAwarenessContext($ctx)
    # 2 = DPI_AWARENESS_PER_MONITOR_AWARE
    if ($awareness -ne 2) {
        throw "SetProcessDpiAwarenessContext 失败（当前 DPI 感知=$awareness，需要 per-monitor）"
    }
}

function Wait-ForLogLine {
    param([string]$Pattern, [int]$TimeoutSeconds = 15)
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        if (Test-Path $logPath) {
            $text = Get-Content -Raw -Encoding utf8 $logPath
            if ($text -match $Pattern) { return $text }
        }
        Start-Sleep -Milliseconds 120
    }
    if (Test-Path $logPath) { return (Get-Content -Raw -Encoding utf8 $logPath) }
    return ''
}

function Post-Click {
    param([IntPtr]$Hwnd, [int]$X, [int]$Y)
    # 别用 `-shl`/`-bor` 拼：PowerShell 的位运算在 Int32/Int64 之间会按实际
    # 类型走，一旦高位参与就容易被符号扩展改写。整数乘法是明确的。
    $packed = [int64]$Y * 65536 + [int64]$X
    $lparam = [IntPtr]$packed
    $down = [Aegis.Msg]::PostMessageW($Hwnd, 0x0201, [IntPtr]1, $lparam)  # WM_LBUTTONDOWN
    $up = [Aegis.Msg]::PostMessageW($Hwnd, 0x0202, [IntPtr]0, $lparam)    # WM_LBUTTONUP
    if (-not $down) {
        Write-Host ("[warn] 投递失败 ({0},{1}) 错误={2}" -f $X, $Y, [Runtime.InteropServices.Marshal]::GetLastWin32Error())
    }
    Start-Sleep -Milliseconds 220
}

$env:TLSHELL_LOG = 'info'
$env:TLSHELL_NO_MODAL = '1'
$previousAppData = $env:APPDATA
$previousLocalData = $env:LOCALAPPDATA
$env:APPDATA = $appData
$env:LOCALAPPDATA = $localData

$proc = Start-Process -FilePath $exe -ArgumentList '--ui-test', '--config', $config -PassThru
try {
    $text = Wait-ForLogLine -Pattern '启动完成'
    if ($text -notmatch '启动完成') { throw '实例没有启动完成' }
    if ($text -notmatch 'UI 测试模式') { throw '没有进 UI 测试模式（单实例锁可能拦下了）' }
    if ($text -match '启动沉底') { throw '测试模式仍在沉底' }

    # 单元格尺寸从日志里读，坐标跟着它算，别写死。
    $m = [regex]::Match($text, '单元格 (\d+)x(\d+)')
    if (-not $m.Success) { throw '日志里没有单元格尺寸' }
    $cellW = [int]$m.Groups[1].Value
    $cellH = [int]$m.Groups[2].Value
    $barH = $cellH + 6

    $hwndFile = Join-Path $appData 'TLShell\hwnd.txt'
    if (-not (Test-Path $hwndFile)) { throw '没有 hwnd.txt' }
    $hwnd = [IntPtr][int64](Get-Content -Raw $hwndFile).Trim()

    function Slot-Width([string]$Title) { $Title.Length * $cellW + 16 }
    function New-Center([string[]]$Titles) {
    # run-all.ps1 开了 StrictMode：单个标签时 $Titles 是字符串，`.Count` 会抛错。
    if (@($Titles).Count -eq 0) { return 12 }
        $x = 4
        foreach ($t in $Titles) { $x += (Slot-Width $t) + 2 }
        return $x - 2 + 10
    }
    function Clear-Center([string[]]$Titles) { (New-Center $Titles) + $cellW * 2 + 10 }
    function Close-Center([string]$Title, [int]$Index) {
        $x = 4
        for ($i = 0; $i -lt $Index; $i++) { $x += (Slot-Width $script:tabs[$i]) + 2 }
        return $x + (Slot-Width $Title) - 8
    }
    function Tab-Center([int]$Index) {
        $x = 4
        for ($i = 0; $i -lt $Index; $i++) { $x += (Slot-Width $script:tabs[$i]) + 2 }
        return $x + 12
    }

    $script:tabs = @('shell 1')
    $y = [int]($barH / 2)
    $results = @()
    function Check([string]$Name, [bool]$Ok, [string]$Detail) {
        $script:results += [pscustomobject]@{ Name = $Name; Ok = $Ok; Detail = $Detail }
    }

    # 1) 点 + 新建标签
    Post-Click $hwnd (New-Center $script:tabs) $y
    $text = Wait-ForLogLine -Pattern '新建标签: shell 2'
    Check '点 + 新建标签' ($text -match '新建标签: shell 2') '期望日志「新建标签: shell 2」'
    $script:tabs = @('shell 1', 'shell 2')

    # 2) 点第一个标签切换
    Post-Click $hwnd (Tab-Center 0) $y
    $text = Wait-ForLogLine -Pattern '切到标签: shell 1'
    Check '点标签切换' ($text -match '切到标签: shell 1') '期望日志「切到标签: shell 1」'

    # 3) 点「清屏」
    Post-Click $hwnd (Clear-Center $script:tabs) $y
    $text = Wait-ForLogLine -Pattern '清屏'
    Check '点清屏按钮' ($text -match '清屏') '期望日志「清屏」'

    # 4) 点第二个标签的叉号关闭
    Post-Click $hwnd (Close-Center 'shell 2' 1) $y
    $text = Wait-ForLogLine -Pattern '关闭标签: shell 2'
    Check '点 × 关闭标签' ($text -match '关闭标签: shell 2') '期望日志「关闭标签: shell 2」'
    $script:tabs = @('shell 1')

    # 5) 再新建：序号必须复用 2，不能因为数组长度而跳到 3
    Post-Click $hwnd (New-Center $script:tabs) $y
    $text = Wait-ForLogLine -Pattern '新建标签: shell 2'
    $reused = ([regex]::Matches($text, '新建标签: shell 2')).Count -ge 2
    Check '关闭后新建复用序号' $reused '「新建标签: shell 2」应出现第二次'
    $script:tabs = @('shell 1', 'shell 2')

    # 6) 关到只剩一个再关：必须有顶替的 shell 1 且能起会话
    Post-Click $hwnd (Close-Center 'shell 2' 1) $y
    Wait-ForLogLine -Pattern '关闭标签: shell 2' | Out-Null
    Post-Click $hwnd (Close-Center 'shell 1' 0) $y
    $text = Wait-ForLogLine -Pattern '关闭标签: shell 1'
    Check '关到空自动补一个' ($text -match '没有终端了' -or $text -match '启动 shell') '关掉最后一个标签后应补 shell 并起会话'

    foreach ($r in $results) {
        $tag = if ($r.Ok) { '[PASS]' } else { '[FAIL]' }
        "$tag $($r.Name) — $($r.Detail)"
    }
    # @( )：全通过时 Where-Object 返回 $null，StrictMode 下 `.Count` 会抛错。
    $failed = @($results | Where-Object { -not $_.Ok }).Count
    "标签栏验收：通过 $(@($results).Count - $failed) 项，失败 $failed 项"
    if ($failed -gt 0) { exit 1 }
}
finally {
    if (-not $proc.HasExited) { $proc.Kill(); $proc.WaitForExit(3000) | Out-Null }
}
exit 0

# 必须把环境变量还回去：run-all.ps1 用 `&` 在同一进程里调本脚本，
# LOCALAPPDATA 一旦被改掉，父进程算好的日志路径就再也读不到新行。
$env:APPDATA = $previousAppData
$env:LOCALAPPDATA = $previousLocalData
