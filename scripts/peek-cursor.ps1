# 光标形状取证：把指针挪到「清屏」按钮与网格上，读回系统光标句柄，和 IDC_HAND / IDC_IBEAM 对比。
# 为什么不用截图：鼠标指针不参与窗口合成，截不到。GetCursorInfo 是唯一能看到它的地方。
param([string]$Exe = '.\target\debug\tlshell.exe')

$ErrorActionPreference = 'Stop'
$root = Join-Path $env:TEMP 'aegis-cursor-peek'
if (Test-Path $root) { Remove-Item $root -Recurse -Force }
$roaming = Join-Path $root 'roaming'
$local = Join-Path $root 'local'
New-Item -ItemType Directory -Path $roaming, $local -Force | Out-Null
$config = Join-Path $root 'config.toml'
@'
[shell]
program = "pwsh.exe"

[window]
width = 900
height = 360
x = 60
y = 60
opacity = 1.0
text_opacity = 1.0
sink_when_unfocused = false

[font]
family = "Cascadia Mono"
size = 14.0
line_height = 1.25
'@ | Set-Content -Path $config -Encoding utf8

$env:APPDATA = $roaming
$env:LOCALAPPDATA = $local
# diag::info 默认不落盘，取证要显式打开。
$env:TLSHELL_LOG = 'info'
$logPath = Join-Path $local 'TLShell\logs\tlshell.log'
$script:log = ''

Add-Type -Namespace Aegis -Name Cur -MemberDefinition @'
[System.Runtime.InteropServices.StructLayout(System.Runtime.InteropServices.LayoutKind.Sequential)]
public struct POINT { public int X; public int Y; }
[System.Runtime.InteropServices.StructLayout(System.Runtime.InteropServices.LayoutKind.Sequential)]
public struct CURSORINFO { public int cbSize; public int flags; public System.IntPtr hCursor; public POINT ptScreenPos; }
[System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true)]
public static extern bool PostMessageW(System.IntPtr hWnd, uint Msg, System.IntPtr wParam, System.IntPtr lParam);
[System.Runtime.InteropServices.DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[System.Runtime.InteropServices.DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
[System.Runtime.InteropServices.DllImport("user32.dll")] public static extern bool GetCursorInfo(ref CURSORINFO info);
[System.Runtime.InteropServices.DllImport("user32.dll")] public static extern bool ClientToScreen(System.IntPtr hWnd, ref POINT p);
[System.Runtime.InteropServices.DllImport("user32.dll", CharSet = System.Runtime.InteropServices.CharSet.Unicode)]
public static extern System.IntPtr LoadCursorW(System.IntPtr hInstance, System.IntPtr name);
[System.Runtime.InteropServices.DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(System.IntPtr value);
'@

if (-not [Aegis.Cur]::SetProcessDpiAwarenessContext([IntPtr](-4))) {
    Write-Host '[warn] DPI 感知设置失败，坐标可能被缩放'
}

function Update-Log {
    if (Test-Path $logPath) {
        try { $script:log = Get-Content -Path $logPath -Raw -ErrorAction Stop } catch { }
    }
}
function Wait-Log([string]$Pattern, [int]$Timeout = 15000) {
    $deadline = (Get-Date).AddMilliseconds($Timeout)
    while ((Get-Date) -lt $deadline) {
        Update-Log
        if ($script:log -match $Pattern) { return $Matches }
        Start-Sleep -Milliseconds 100
    }
    throw "超时未出现日志: $Pattern。日志尾部:`n$($script:log.Substring([Math]::Max(0, $script:log.Length - 600)))"
}
function Get-CursorHandle {
    $info = New-Object Aegis.Cur+CURSORINFO
    $info.cbSize = [System.Runtime.InteropServices.Marshal]::SizeOf([type][Aegis.Cur+CURSORINFO])
    if (-not [Aegis.Cur]::GetCursorInfo([ref]$info)) { throw 'GetCursorInfo 失败' }
    $info.hCursor.ToInt64()
}
function Probe-Cursor([IntPtr]$Hwnd, [int]$X, [int]$Y, [string]$Name) {
    $p = New-Object Aegis.Cur+POINT
    $p.X = $X; $p.Y = $Y
    [Aegis.Cur]::ClientToScreen($Hwnd, [ref]$p) | Out-Null
    [Aegis.Cur]::SetCursorPos($p.X, $p.Y) | Out-Null
    Start-Sleep -Milliseconds 60
    # 低 16 位是命中码（HTCLIENT=1）：给默认过程看的那条老路径需要它。
    [Aegis.Cur]::PostMessageW($Hwnd, 0x0020, [IntPtr]::Zero, [IntPtr]1) | Out-Null
    Start-Sleep -Milliseconds 150
    $script:handles[$Name] = Get-CursorHandle
    Write-Host ("{0,-10} 客户区({1},{2}) → 屏幕({3},{4}) 光标=0x{5:x}" -f $Name, $X, $Y, $p.X, $p.Y, $script:handles[$Name])
}

$script:handles = @{}
$before = New-Object Aegis.Cur+POINT
[Aegis.Cur]::GetCursorPos([ref]$before) | Out-Null

$proc = Start-Process -FilePath $Exe -ArgumentList '--ui-test', '--config', $config -PassThru
try {
    # $Matches 只在跑 -match 的那个函数作用域里有效，必须用返回值。
    $m = Wait-Log '标签栏: 单元格=(\d+) 栏高=(\d+) 新建中心=(\d+) shell中心=(\d+) 清屏中心=(\d+) 标签数=(\d+) 可绘右界=(\d+)'
    $cellW = [int]$m[1]; $barH = [int]$m[2]
    $new = [int]$m[3]; $shell = [int]$m[4]; $clear = [int]$m[5]
    $clipEnd = [int]$m[7]
    $w = Wait-Log 'window hwnd=0x([0-9a-fA-F]+)'
    $hwnd = [IntPtr][Convert]::ToInt64($w[1], 16)
    Write-Host ("栏高={0} 单元格={1} shell中心={2} 清屏中心={3}" -f $barH, $cellW, $shell, $clear)

    $y = [int]($barH / 2)
    Probe-Cursor $hwnd $clear $y '按钮'
    Probe-Cursor $hwnd $new $y '新建'
    # 空白必须挑真空白：按钮各自的命中圈外、也不落在任何标签上。
    Probe-Cursor $hwnd ([int](($shell + $clear) / 2)) $y '空白'
    Probe-Cursor $hwnd ([int]($clear / 2)) ($barH + 40) '网格'
    Probe-Cursor $hwnd 2 ([int]($barH + 60)) '左边框'

    $hand = [Aegis.Cur]::LoadCursorW([IntPtr]::Zero, [IntPtr]32649).ToInt64()
    $beam = [Aegis.Cur]::LoadCursorW([IntPtr]::Zero, [IntPtr]32513).ToInt64()
    $sizewe = [Aegis.Cur]::LoadCursorW([IntPtr]::Zero, [IntPtr]32644).ToInt64()
    $arrow = [Aegis.Cur]::LoadCursorW([IntPtr]::Zero, [IntPtr]32512).ToInt64()

    $fail = 0
    if ($script:handles['按钮'] -ne $hand) { Write-Host '[FAIL] 按钮上应是手型 (IDC_HAND)'; $fail++ } else { Write-Host '[PASS] 按钮上是手型' }
    if ($script:handles['新建'] -ne $hand) { Write-Host '[FAIL] 新建按钮上应是手型'; $fail++ } else { Write-Host '[PASS] 新建按钮上是手型' }
    if ($script:handles['空白'] -ne $arrow) { Write-Host '[FAIL] 标签栏空白处应是普通箭头'; $fail++ } else { Write-Host '[PASS] 标签栏空白处是普通箭头' }
    if ($script:handles['网格'] -ne $beam) { Write-Host '[FAIL] 网格上应是 I-beam'; $fail++ } else { Write-Host '[PASS] 网格上是文本插入符' }
    if ($script:handles['左边框'] -ne $sizewe) { Write-Host '[FAIL] 左边框应是水平缩放'; $fail++ } else { Write-Host '[PASS] 左边框是水平缩放' }
    Write-Host ("光标取证：失败 {0} 项" -f $fail)
}
finally {
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    [Aegis.Cur]::SetCursorPos($before.X, $before.Y) | Out-Null
}
