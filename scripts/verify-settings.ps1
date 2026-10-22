# 设置面板的真机验收：只用 PostMessage 往自己的窗口投按键，不注入系统级输入。
#
# 判据：① 面板打开日志出现；② ←→ 调整后值变了；③ Enter 后 config.toml 落盘为
# 调整后的值；④ 落盘帧里有面板像素（PNG 存进证据目录）。
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root 'target\debug\tlshell.exe'
$work = Join-Path $env:TEMP 'aegis-settings'
$cfg = Join-Path $work 'config.toml'
$bmp = Join-Path $work 'panel.bmp'
$evidence = Join-Path $root 'docs\aegis\evidence\vis\M7'
$log = Join-Path $env:LOCALAPPDATA 'TLShell\logs\tlshell.log'
$hwndFile = Join-Path $env:APPDATA 'TLShell\hwnd.txt'

New-Item -ItemType Directory -Force -Path $work, $evidence | Out-Null
Remove-Item $cfg, $bmp -ErrorAction SilentlyContinue
Get-Process tlshell -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 600

Add-Type -Namespace W -Name N -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool PostMessageW(System.IntPtr h, uint m, System.IntPtr w, System.IntPtr l);
'@

$env:TLSHELL_LOG = 'debug'
$proc = Start-Process -FilePath $exe -ArgumentList @('--config', $cfg, '--dump-frame', $bmp) -PassThru
Start-Sleep -Milliseconds 2500

$hwnd = [IntPtr]([int64](Get-Content $hwndFile -Raw).Trim())
"hwnd = 0x{0:x}" -f [int64]$hwnd

function Send-Key([int]$vk) {
    [void][W.N]::PostMessageW($hwnd, 0x0100, [IntPtr]$vk, [IntPtr]0x00010001)
    [void][W.N]::PostMessageW($hwnd, 0x0101, [IntPtr]$vk, [IntPtr]0xC0010001)
}

Send-Key 0x71          # F2 打开面板
Start-Sleep -Milliseconds 2600
Send-Key 0x27          # →
Send-Key 0x27          # →
Start-Sleep -Milliseconds 400
Send-Key 0x0D          # Enter 保存
Start-Sleep -Milliseconds 1200

if (-not (Test-Path $cfg)) { throw '配置没有被写出来' }
$saved = Get-Content $cfg -Raw
$opacity = [regex]::Match($saved, '(?m)^opacity\s*=\s*([0-9.]+)').Groups[1].Value
"opacity = $opacity"

$lines = Get-Content $log | Where-Object { $_ -match '设置|面板' } | Select-Object -Last 8
$lines

$proc | Stop-Process -Force
Start-Sleep -Milliseconds 300

# 帧 → PNG
Add-Type -AssemblyName System.Drawing
$bmpPath = $bmp
$png = Join-Path $evidence '04-settings-panel.png'
$img = [System.Drawing.Image]::FromFile($bmpPath)
$img.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)
$img.Dispose()
"png = $png"
(Get-Item $png).Length
