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
[shell]
program = "pwsh.exe"

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

function Post-Key {
    param([IntPtr]$Hwnd, [int]$VirtualKey, [int]$ScanCode)
    # lParam 照真实按键拼：低 16 位重复次数=1，16..23 位扫描码，30 位「前一状态=按下」。
    # 用整数乘加而不是 -shl/-bor：PowerShell 的位运算会跟着 Int32/Int64 走，
    # 高位一旦参与就容易被符号扩展改写。
    $lparam = [int64]1 + [int64]$ScanCode * 65536 + [int64]1073741824
    [Aegis.Msg]::PostMessageW($Hwnd, 0x0100, [IntPtr]$VirtualKey, [IntPtr]$lparam) | Out-Null
    Start-Sleep -Milliseconds 220
}

function Post-Enter { param([IntPtr]$Hwnd) Post-Key $Hwnd 13 0x1C }

function Post-Escape { param([IntPtr]$Hwnd) Post-Key $Hwnd 27 0x01 }

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

    # 8) shell 类型按钮：必须弹窗让用户选，不能点一下就偷偷换掉
    Post-Click $hwnd $shellCenter $y
    $text = Wait-ForLogLine -Pattern '打开切换终端弹窗: '
    $pm = [regex]::Match($text, '打开切换终端弹窗: (\d+) 个候选，当前 (\S+)，行中心=\[(\d+), (\d+)\]，行高=(\d+)')
    Check '点 shell 按钮弹出选择弹窗' ($pm.Success) '期望日志「打开切换终端弹窗: … 行中心=[…]」'
    if (-not $pm.Success) { throw '弹窗没有打开，后面几个用例都无从下手' }
    # 行的竖直中心也从日志里取：脚本不再自己算弹窗几何。
    $rowX = [int]($clipEnd / 2)
    $row0 = [int]$pm.Groups[3].Value
    $row1 = [int]$pm.Groups[4].Value

    # 8a) 鼠标点「方式」行：状态必须在两种方式之间翻面
    Post-Click $hwnd $rowX $row1
    $text = Wait-ForLogLine -Pattern '切换终端弹窗: 点第 2 行'
    Check '弹窗行能被点中' ($text -match '方式=新开标签') '点「方式」行应把方式翻成新开标签'
    Post-Click $hwnd $rowX $row1
    # 等待条件必须是「只有第二次点击才会产生」的那串字，否则等第一次的同名日志会立刻返回、读到旧内容。
    $text = Wait-ForLogLine -Pattern '方式=原地切换'
    Check '弹窗行可翻回' ($text -match '方式=原地切换') '再点一次应翻回原地切换'

    # 8b) Esc 取消：关掉弹窗，什么都不做
    Post-Escape $hwnd
    $text = Wait-ForLogLine -Pattern '关闭切换终端弹窗'
    Check 'Esc 取消弹窗' ($text -match '关闭切换终端弹窗（未确认）') '期望日志「关闭切换终端弹窗（未确认）」'

    # 8c) 默认回车确认：选的就是当前终端，必须认成「已经是」，不能白重启一次
    Post-Click $hwnd $shellCenter $y
    Start-Sleep -Milliseconds 250
    Post-Enter $hwnd
    $text = Wait-ForLogLine -Pattern '原地切换: 当前已经是'
    Check '确认当前终端不重启' ($text -match '未重启') '期望日志「原地切换: 当前已经是 …，未重启」'

    # 9) 先造出比一屏更多的输出并滚进历史：这是下面「切换不清记录」的基线
    Post-Click $hwnd ([int]($clipEnd / 2)) ([int]($barH + 40))
    Post-Text $hwnd '1..80|%{$_}'
    # 回车必须走 WM_KEYDOWN：WM_CHAR 的 0x0D 会被 handle_char 当控制字符丢掉，
    # 真实回车也是先到 WM_KEYDOWN 再由按键编码路径写成 CR 。
    Post-Enter $hwnd
    Start-Sleep -Milliseconds 2000
    Post-Wheel $hwnd ([int]($clipEnd / 2)) ([int]($barH + 40)) 120
    $text = Wait-ForLogLine -Pattern '回滚: 偏移='
    $rm = [regex]::Match($text, '回滚: 偏移=(\d+) 历史=(\d+)')
    Check '滚轮回滚进历史' ($rm.Success -and [int]$rm.Groups[1].Value -gt 0 -and [int]$rm.Groups[2].Value -gt 0) "期望「回滚: 偏移=>0 历史=>0」，实际：$($rm.Value)"
    $historyBefore = if ($rm.Success) { [int]$rm.Groups[2].Value } else { 0 }

    # 10) 换成另一类终端并原地确认：不能新开标签，也不能把历史清掉
    $tabsBefore = @(Read-Slots).Count
    Post-Click $hwnd $shellCenter $y
    Start-Sleep -Milliseconds 250
    Post-Click $hwnd $rowX $row0
    $text = Wait-ForLogLine -Pattern '切换终端弹窗: 点第 1 行'
    Check '弹窗终端行可点' ($text -match '终端=') '点「终端」行应换到下一个候选'
    Post-Enter $hwnd
    $text = Wait-ForLogLine -Pattern '屏幕与回滚历史保留'
    $hm = [regex]::Match($text, '原地切换: (\S+)，屏幕与回滚历史保留（历史=(\d+) 行，切换前=(\d+) 行）')
    Check '原地切换保留历史' ($hm.Success -and [int]$hm.Groups[2].Value -ge $historyBefore -and [int]$hm.Groups[2].Value -gt 0) "切换前历史=$historyBefore，切换后应不小于它且大于 0"
    $tabsAfter = @(Read-Slots).Count
    Check '原地切换不开新标签' ($tabsAfter -eq $tabsBefore) "标签数应保持 $tabsBefore，实际 $tabsAfter"

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
