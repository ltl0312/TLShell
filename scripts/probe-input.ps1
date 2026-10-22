Add-Type -AssemblyName System.Windows.Forms | Out-Null
$src = @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public class W {
  [DllImport("user32.dll")] public static extern IntPtr GetParent(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool GetGUIThreadInfo(uint tid, ref GUITHREADINFO gti);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, UIntPtr e);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string cls, string win);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder sb, int n);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L,T,R,B; }
  [StructLayout(LayoutKind.Sequential)] public struct GUITHREADINFO {
    public int cbSize; public int flags; public IntPtr hwndActive; public IntPtr hwndFocus;
    public IntPtr hwndCapture; public IntPtr hwndMenuOwner; public IntPtr hwndMoveSize; public IntPtr hwndCaret;
    public RECT rcCaret;
  }
}
'@
Add-Type -TypeDefinition $src

$HWND_TOPMOST   = [IntPtr](-1)
$HWND_BOTTOM    = [IntPtr](1)
$SWP_NOMOVE     = 0x0002
$SWP_NOSIZE     = 0x0001
$SWP_NOACTIVATE = 0x0010

function Describe([IntPtr]$h) {
  if ($h -eq [IntPtr]::Zero) { return '<null>' }
  $sb = New-Object System.Text.StringBuilder 256
  [void][W]::GetClassName($h, $sb, 256)
  $owner = [uint32]0
  $t = [W]::GetWindowThreadProcessId($h, [ref]$owner)
  "0x{0:x} class=$sb pid=$owner tid=$t" -f $h.ToInt64()
}

$proc = Get-Process tlshell -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) { throw 'TLShell 未运行' }
$hwnd = $proc.MainWindowHandle
"pid/hwnd    = $($proc.Id) / $(Describe $hwnd)"
"parent      = $(Describe ([W]::GetParent($hwnd)))"

# 我们的窗口在 z-order 最底层，桌面上任何最大化的窗口都会盖住它。
# 测试时临时置顶，测完恢复沉底——这样才能真正点到它。
[void][W]::SetWindowPos($hwnd, $HWND_TOPMOST, 0, 0, 0, 0, $SWP_NOMOVE -bor $SWP_NOSIZE -bor $SWP_NOACTIVATE)
Start-Sleep -Milliseconds 400

$r = New-Object W+RECT
[void][W]::GetWindowRect($hwnd, [ref]$r)
$cx = [int](($r.L + $r.R) / 2); $cy = [int](($r.T + $r.B) / 2)
"rect        = $($r.L),$($r.T) - $($r.R),$($r.B)  center=$cx,$cy"

[void][W]::SetCursorPos($cx, $cy)
Start-Sleep -Milliseconds 150
[W]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 60
[W]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 500

$owner = [uint32]0
$tid = [W]::GetWindowThreadProcessId($hwnd, [ref]$owner)
$gti = New-Object W+GUITHREADINFO
$gti.cbSize = [System.Runtime.InteropServices.Marshal]::SizeOf($gti)
$ok = [W]::GetGUIThreadInfo($tid, [ref]$gti)
"gti ok      = $ok  (tid=$tid)"
"hwndActive  = $(Describe $gti.hwndActive)"
"hwndFocus   = $(Describe $gti.hwndFocus)"
"focus==win  = $($gti.hwndFocus -eq $hwnd)"
"foreground  = $(Describe ([W]::GetForegroundWindow()))"

# 端到端：真的按键，看命令是否被执行（文件是副作用探针）。
$probe = Join-Path $env:TEMP 'aegis-input-probe.txt'
if (Test-Path $probe) { Remove-Item $probe -Force }
[System.Windows.Forms.SendKeys]::SendWait("New-Item -ItemType File -Force -Path '$probe' | Out-Null{ENTER}")
Start-Sleep -Milliseconds 3500
"probe file  = $(Test-Path $probe)"

[void][W]::SetWindowPos($hwnd, $HWND_BOTTOM, 0, 0, 0, 0, $SWP_NOMOVE -bor $SWP_NOSIZE -bor $SWP_NOACTIVATE)
"restored    = bottom"
