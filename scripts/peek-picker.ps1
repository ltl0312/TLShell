# 一次性取证脚本：把「切换终端」弹窗真的点出来，截一张图看看排版。
# 不进验收流程，只供人工核对弹窗行距/对齐；用完可以删。
param([string]$Exe = '.\target\debug\tlshell.exe')

$ErrorActionPreference = 'Stop'
$root = Join-Path $env:TEMP 'aegis-picker-peek'
if (Test-Path $root) { Remove-Item $root -Recurse -Force }
$appData = Join-Path $root 'roaming'
$localData = Join-Path $root 'local'
New-Item -ItemType Directory -Force -Path $appData, $localData | Out-Null

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

[font]
family = "Cascadia Mono"
size = 14.0
line_height = 1.25
'@ | Set-Content -Path $config -Encoding utf8

$logPath = Join-Path $localData 'TLShell\logs\tlshell.log'
$shot = Join-Path $root 'picker.png'

Add-Type -Namespace Aegis -Name Msg -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true)]
public static extern bool PostMessageW(System.IntPtr hWnd, uint Msg, System.IntPtr wParam, System.IntPtr lParam);
[System.Runtime.InteropServices.DllImport("user32.dll")]
public static extern bool SetProcessDpiAwarenessContext(System.IntPtr value);
'@

# 进程自己不是 per-monitor 感知时，Windows 会把投递进来的坐标按 DPI 虚拟化：
# 发 (812,15) 到 130% 缩放的应用会变成 (1100,21)，点击就落到按钮外面。
if (-not [Aegis.Msg]::SetProcessDpiAwarenessContext([IntPtr](-4))) {
    Write-Host '[wwarn] 进程 DPI 感知设置失败，截图与坐标可能被缩放'
}
# 结构体和 P/Invoke 必须在同一份源码里：分两次 Add-Type 时，先编译的那份看不到后定义的 RECT。
Add-Type -TypeDefinition @'
namespace Aegis {
    [System.Runtime.InteropServices.StructLayout(System.Runtime.InteropServices.LayoutKind.Sequential)]
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }

    public static class Ui {
        [System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true, CharSet = System.Runtime.InteropServices.CharSet.Unicode)]
        public static extern System.IntPtr FindWindowW(string lpClassName, string lpWindowName);

        [System.Runtime.InteropServices.DllImport("user32.dll")]
        public static extern bool GetWindowRect(System.IntPtr hWnd, out RECT lpRect);
    }
}
'@

$env:TLSHELL_LOG = 'info'
$env:TLSHELL_NO_MODAL = '1'
$env:APPDATA = $appData
$env:LOCALAPPDATA = $localData

# --ui-test：不沉底，窗口才会被截到（沉底模式下 CopyFromScreen 截到的是浏览器）。
$proc = Start-Process -FilePath $Exe -ArgumentList '--ui-test' -PassThru
try {
    # 窗口挂在桌面层（Progman 之下）时 FindWindowW 找不到：直接在日志里取 hwnd。
    $hwnd = [IntPtr]::Zero
    for ($i = 0; $i -lt 100; $i++) {
        Start-Sleep -Milliseconds 100
        if (-not (Test-Path $logPath)) { continue }
        $found = Get-Content -Raw -Encoding utf8 $logPath | Select-String -Pattern 'window hwnd=(0x[0-9a-fA-F]+)'
        if ($found) {
            $hwnd = [IntPtr][Convert]::ToInt64($found.Matches[0].Groups[1].Value, 16)
            break
        }
    }
    if ($hwnd -eq [IntPtr]::Zero) { throw '日志里没有窗口句柄' }
    Start-Sleep -Milliseconds 1800

    $line = Get-Content -Raw -Encoding utf8 $logPath |
        Select-String -Pattern '标签栏: 单元格=(\d+) 栏高=(\d+) 新建中心=(\d+) shell中心=(\d+) 清屏中心=(\d+)' |
        Select-Object -Last 1
    if (-not $line) { throw '日志里没有标签栏几何' }
    $cellW = [int]$line.Matches[0].Groups[1].Value
    $barH = [int]$line.Matches[0].Groups[2].Value
    $shellCenter = [int]$line.Matches[0].Groups[4].Value
    Write-Host "栏高=$barH 单元格=$cellW shell中心=$shellCenter"

    $packed = [int64]([int]($barH / 2)) * 65536 + [int64]$shellCenter
    [Aegis.Msg]::PostMessageW($hwnd, 0x0201, [IntPtr]1, [IntPtr]$packed) | Out-Null
    [Aegis.Msg]::PostMessageW($hwnd, 0x0202, [IntPtr]0, [IntPtr]$packed) | Out-Null
    Start-Sleep -Milliseconds 900

    $rect = New-Object Aegis.RECT
    [Aegis.Ui]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
    Add-Type -AssemblyName System.Drawing
    $w = $rect.Right - $rect.Left
    $h = $rect.Bottom - $rect.Top
    $bmp = New-Object System.Drawing.Bitmap($w, $h)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, (New-Object System.Drawing.Size($w, $h)))
    $bmp.Save($shot, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose()
    $bmp.Dispose()
    Write-Host "截图: $shot (${w}x${h})"

    Get-Content -Encoding utf8 $logPath | Select-String -Pattern '打开切换终端弹窗|切换终端弹窗:' | ForEach-Object { $_.Line }
}
finally {
    if (-not $proc.HasExited) { $proc.Kill() }
}
