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

function Post-Move {
    param([IntPtr]$Hwnd, [int]$X, [int]$Y)
    # WM_MOUSEMOVE 的 wParam 是按键状态位（0 = 没按），lParam 同 WM_LBUTTONDOWN。
    $packed = [int64]$Y * 65536 + [int64]$X
    [Aegis.Msg]::PostMessageW($Hwnd, 0x0200, [IntPtr]0, [IntPtr]$packed) | Out-Null
    Start-Sleep -Milliseconds 220
}

function Post-Text {
    param([IntPtr]$Hwnd, [string]$Text)
    # WM_CHAR 直接进 PTY：用来制造足够多的输出，才能验证回滚。
    foreach ($ch in $Text.ToCharArray()) {
        [Aegis.Msg]::PostMessageW($Hwnd, 0x0102, [IntPtr][int][char]$ch, [IntPtr]0) | Out-Null
        Start-Sleep -Milliseconds 12
    }
}

function Post-Wheel {
    param([IntPtr]$Hwnd, [int]$X, [int]$Y, [int]$Delta)
    # WM_MOUSEWHEEL：wParam 高 16 位是带符号的滚动量，lParam 是屏幕坐标。
    $wParam = [int64]($Delta -band 0xFFFF) * 65536
    $packed = [int64]$Y * 65536 + [int64]$X
    [Aegis.Msg]::PostMessageW($Hwnd, 0x020A, [IntPtr]$wParam, [IntPtr]$packed) | Out-Null
    Start-Sleep -Milliseconds 250
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

    # 几何全部从程序自己的日志里读。
    # 脚本里再算一遍就必然和产品漂移，而「点不准」的 bug 恰恰藏在第二份几何里
    # —— 上一版脚本正是自己算宽度，标签栏几何改了之后它还在点老坐标。
    $text = Wait-ForLogLine -Pattern '标签栏: 单元格'
    $m = [regex]::Match($text, '标签栏: 单元格=(\d+) 栏高=(\d+) 新建中心=(\d+) shell中心=(\d+) 清屏中心=(\d+) 标签数=(\d+) 可绘右界=(\d+)')
    if (-not $m.Success) { throw '日志里没有标签栏几何' }
    $cellW = [int]$m.Groups[1].Value
    $barH = [int]$m.Groups[2].Value
    $newCenter = [int]$m.Groups[3].Value
    $shellCenter = [int]$m.Groups[4].Value
    $clearCenter = [int]$m.Groups[5].Value
    $clipEnd = [int]$m.Groups[7].Value
    if ($newCenter -le 0 -or $clearCenter -le 0) { throw '标签栏几何不合理（按钮中心为 0）' }

    $hwndFile = Join-Path $appData 'TLShell\hwnd.txt'
    if (-not (Test-Path $hwndFile)) { throw '没有 hwnd.txt' }
    $hwnd = [IntPtr][int64](Get-Content -Raw $hwndFile).Trim()

    # 「标签[N]: 标题 起=… 止=… 叉=…」逐行解析，脚本不再自己算宽度。
    function Read-Slots {
        $log = Get-Content -Raw -Encoding utf8 $logPath
        $latest = @{}
        foreach ($line in ($log -split "`n")) {
            $sm = [regex]::Match($line, '标签\[(\d+)\]: (.+?) 起=(\d+) 止=(\d+) 叉=(\d+)')
            if ($sm.Success) {
                $latest[[int]$sm.Groups[1].Value] = [pscustomobject]@{
                    Index = [int]$sm.Groups[1].Value
                    Title = $sm.Groups[2].Value.Trim()
                    Start = [int]$sm.Groups[3].Value
                    End   = [int]$sm.Groups[4].Value
                    Close = [int]$sm.Groups[5].Value
                }
            }
        }
        return @($latest.Values | Sort-Object Index)
    }

    function Get-Slot([int]$Index) {
        $slot = @(Read-Slots) | Where-Object { $_.Index -eq $Index } | Select-Object -First 1
        if (-not $slot) { throw "日志里没有标签 $Index 的几何" }
        return $slot
    }

    function Tab-Title-Center([int]$Index) {
        $slot = Get-Slot $Index
        # 标题区中点：离叉号足够远，点下去只会切标签。
        return [int](($slot.Start + $slot.Close) / 2)
    }

    function Tab-Close-Center([int]$Index) { [int](Get-Slot $Index).Close }

    $script:tabs = @('shell 1')
    $y = [int]($barH / 2)
    $results = @()
    function Check([string]$Name, [bool]$Ok, [string]$Detail) {
        $script:results += [pscustomobject]@{ Name = $Name; Ok = $Ok; Detail = $Detail }
    }

    # 1) 点 + 新建标签
    Post-Click $hwnd $newCenter $y
    $text = Wait-ForLogLine -Pattern '新建标签: shell 2'
    Check '点 + 新建标签' ($text -match '新建标签: shell 2') '期望日志「新建标签: shell 2」'
    $script:tabs = @('shell 1', 'shell 2')

    # 2) 点第一个标签切换
    Post-Click $hwnd (Tab-Title-Center 0) $y
    $text = Wait-ForLogLine -Pattern '切到标签: shell 1'
    Check '点标签切换' ($text -match '切到标签: shell 1') '期望日志「切到标签: shell 1」'

    # 3) 点「清屏」
    Post-Click $hwnd $clearCenter $y
    $text = Wait-ForLogLine -Pattern '清屏'
    Check '点清屏按钮' ($text -match '清屏') '期望日志「清屏」'

    # 4) 点第二个标签的叉号关闭
    Post-Click $hwnd (Tab-Close-Center 1) $y
    $text = Wait-ForLogLine -Pattern '关闭标签: shell 2'
    Check '点 × 关闭标签' ($text -match '关闭标签: shell 2') '期望日志「关闭标签: shell 2」'
    $script:tabs = @('shell 1')

    # 5) 再新建：序号必须复用 2，不能因为数组长度而跳到 3
    Post-Click $hwnd $newCenter $y
    $text = Wait-ForLogLine -Pattern '新建标签: shell 2'
    $reused = ([regex]::Matches($text, '新建标签: shell 2')).Count -ge 2
    Check '关闭后新建复用序号' $reused '「新建标签: shell 2」应出现第二次'
    $script:tabs = @('shell 1', 'shell 2')

    # 6) 关到只剩一个再关：必须有顶替的 shell 1 且能起会话
    Post-Click $hwnd (Tab-Close-Center 1) $y
    Wait-ForLogLine -Pattern '关闭标签: shell 2' | Out-Null
    Post-Click $hwnd (Tab-Close-Center 0) $y
    $text = Wait-ForLogLine -Pattern '关闭标签: shell 1'
    Check '关到空自动补一个' ($text -match '没有终端了' -or $text -match '启动 shell') '关掉最后一个标签后应补 shell 并起会话'

    # 7) 悬停反馈：鼠标移到「清屏」上，必须打出悬停日志并重绘
    Post-Move $hwnd $clearCenter $y
    $text = Wait-ForLogLine -Pattern '悬停: Clear'
    Check '悬停清屏有反馈' ($text -match '悬停: Clear') '期望日志「悬停: Clear」'

    # 8) shell 类型按钮：点一下必须切换并写回配置
    $before = Get-Content -Raw -Encoding utf8 $logPath
    Post-Click $hwnd $shellCenter $y
    $text = Wait-ForLogLine -Pattern 'shell 已切换为'
    Check '点 shell 按钮切换类型' ($text -match 'shell 已切换为') '期望日志「shell 已切换为 …」'

    # 9) 回滚：先造出比一屏更多的输出，再往上滚一格，必须进入回滚状态
    Post-Click $hwnd ([int]($clipEnd / 2)) ([int]($barH + 40))
    Post-Text $hwnd '1..80|%{$_}'
    # 回车必须走 WM_KEYDOWN：WM_CHAR 的 0x0D 会被 handle_char 当控制字符丢掉，
    # 真实回车也是先到 WM_KEYDOWN 再由按键编码路径写成 CR 。
    $scan = [int64]0x1C -shl 16
    [Aegis.Msg]::PostMessageW($hwnd, 0x0100, [IntPtr]13, [IntPtr]$scan) | Out-Null
    Start-Sleep -Milliseconds 2000
    Post-Wheel $hwnd ([int]($clipEnd / 2)) ([int]($barH + 40)) 120
    $text = Wait-ForLogLine -Pattern '回滚: 偏移='
    $rm = [regex]::Match($text, '回滚: 偏移=(\d+) 历史=(\d+)')
    Check '滚轮回滚进历史' ($rm.Success -and [int]$rm.Groups[1].Value -gt 0 -and [int]$rm.Groups[2].Value -gt 0) "期望「回滚: 偏移=>0 历史=>0」，实际：$($rm.Value)"

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
