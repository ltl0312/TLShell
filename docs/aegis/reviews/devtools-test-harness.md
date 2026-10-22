# 开发者工具评审：TLShell 全量功能测试执行器（可重复 N 轮）

- **日期**：2026-10-22
- **评审角色**：开发者工具工程师（CLI 交互 / 自动化可重复性 / 门禁工程）
- **触发问题**：`scripts/gate.ps1` 只有 `fmt → clippy → test → build`，**不跑 `--selftest`、不做 GUI 冒烟、不读日志**；122 个 `cargo test` 全绿也不能证明「窗口建起来了、能打字、能粘贴、能退出」。需要一套每轮输出 pass/fail 表、可跑 ≥6 轮、结果可 diff 的执行器。
- **输入证据**：`src/main.rs`（CLI 解析 60-105、`run_quit_request` 106-110、`run_selftest` 112-133、`resolve_rect` 135-170）、`src/app/selftest.rs`（全通过路径 11 项检查，`Report{passed, checks}`）、`src/app.rs`（启动序列 1385-1411、wndproc 1069-1253、`persist_state` 1034-1051、常量 57-83）、`src/diag.rs`（日志格式与 `TLSHELL_LOG`）、`src/config/mod.rs`（默认值与 `sanitize`）、`src/win/{single,workerw,hotkey,layered}.rs`、`scripts/gate.ps1`、`README.md` 门禁与交互契约表（**行号基于当前工作树**；`src/app.rs` 在工作树里有 10 行未提交改动，与本次评审无关）
- **结论**：12 个必测面里 **9 个可全自动**（进程内测试 / 自检 JSON / 日志断言 / P/Invoke 注入），**2 个半自动**（滚动位移、错误文本与提示条的「看起来对不对」需人看截图），**1 个只能人工**（真实 DPI/多显示器切换与纯视觉项）。关键突破口是**用「副作用探针」代替像素断言**：让 shell 写一个临时文件，就能在完全不看图的情况下验证「焦点 → 键盘 → WM_CHAR → PTY → shell 执行」整条链路。
- **本文不改源码**，只交付执行器设计与脚本骨架。

---

## 0. 三条决定设计形态的事实

### F1 `--quit` 是空操作，不能用它收尾

`run_quit_request()`（`src/main.rs:106-110`）只写一行 `diag::info("--quit：请求已有实例退出")` 就 `Ok(0)`，注释自己写着「单实例握手在 M5 接入」。而 `WM_CLOSE` 被显式忽略（`src/app.rs` wndproc，日志「忽略外部 WM_CLOSE」）。

因此**结束真实实例只有两条路**：注入 `Ctrl+Alt+Q`（`WM_HOTKEY` → `PostQuitMessage`，唯一温和退出），或 `Stop-Process`（强杀，不走正常清理）。执行器默认用前者，后者只在超时兜底。`--quit` 仅可用来测「它确实什么都没干」。

### F2 默认日志只有 warn/error，断言必须先开 `TLSHELL_LOG=info`

`src/diag.rs` 只在 `TLSHELL_LOG=info|debug` 时落 info/debug 行，且行格式严格 `[{level}] {message}`。绝大多数可断言事件（`启动完成`、`已挂到 WorkerW`、`已复制 N 个字符`、`配置已热重载`、`收到退出热键`）都是 **info** 级；选中/滚动/保存位置细节是 **debug** 级。**子进程启动前必须设 `TLSHELL_LOG=debug`**，否则一半用例会「查无此行」而不是失败——这是最容易踩的假阴性。

日志路径 `%LOCALAPPDATA%\TLShell\logs\tlshell.log`，>1MiB 轮转、保留 3 份。执行器用**字节偏移游标**读增量，并把「文件变短」当作轮转信号重置偏移。

### F3 热重载只认 `%APPDATA%\TLShell\config.toml`

`App::build` 里 `config_path` 硬取 `crate::config::config_path()`（`src/app.rs:187`），`--config` 的覆盖值不会写回 App。所以**热重载用例必须改默认路径的配置文件**，`--config` 只影响首次加载。执行器每轮把 `config.baseline.toml` 复制到默认路径，`--config` 参数在用例中单独覆盖（用于错误显示用例，见 T16 的取舍说明）。

---

## 1. 分层与轮次结构

| 档 | 手段 | 覆盖 | 每轮耗时 | 判定 |
|---|---|---|---|---|
| **A** | `cargo test` | 122 项：grid/paste/input/config/state/render 等纯逻辑 | 一次构建内复用 | exit code |
| **B** | `tlshell.exe --selftest --report <json>` | 全通过路径 11 项：module_handle / register_class / create_window / ex_style_toolwindow / ex_style_layered / layered_alpha_roundtrip / window_geometry / user_data_bound / dpi_query / hotkey_register / destroy_window | ~1s | JSON 逐项 |
| **C** | P/Invoke 驱动真实实例（`TLSHELL_LOG=debug`） | 17 个 GUI 用例：启动、桌面层、焦点键盘、选中复制、滚动、粘贴闸门、热重载、持久化、单实例、退出、DPI、错误显示、命中区 | ~40-60s | 日志 + P/Invoke 回读 + 副作用探针 |
| **D** | 人看截图 + 交互 | 视觉/时序/真实环境（见 §6） | 人工 | 清单勾选 |

**轮次语义**：一轮 = 完全一致的初始状态（baseline 配置 + 删除 `state.json`/`hwnd.txt`/剪贴板预置 + 日志游标归零）。A 档默认**只跑一次**（源码没变就没有新信息），用 `-UnitEveryRound` 可强制每轮跑；B、C 档每轮必跑。这样 6 轮 ≈ 8-10 分钟，适合挂在提交前。

**为什么 ≥6 轮有意义**：本项目的失败模式是**时序型**（`watch_config` 1s 轮询、`PASTE_CONFIRM_TTL=3s`、`OVERLAY_TTL=3s`、`TIMER_TICK` 驱动），单轮通过没有统计意义。执行器把每轮结果写成 JSON，最后算**每个用例的 flake 率**：`R1..R6` 里出现「过/不过混杂」的用例单独列出，这类用例要么加等待，要么就是真的 bug。

---

## 2. 用例矩阵

`类` 列：`AUTO` 全自动可判；`SEMI` 自动采集 + 人看截图；`MANUAL` 只能人工；`BLOCKED` 前置失败时的下沉状态（不算 FAIL，避免级联假阴性）。

| ID | 必测面 | 手段 | 断言（可执行） | 类 |
|---|---|---|---|---|
| A | 逻辑层 | `cargo test` | exit 0 + `test result: ok. 122 passed` | AUTO |
| B | 窗口自检 | `--selftest --report` | `passed=true` 且 `checks[]` 每项 `ok=true` 且**行数 = 11**（少于 11 说明提前 return 截断）；exit 0 | AUTO |
| T01 | 窗口创建 | 读 `hwnd.txt` + `GetWindowLongPtrW(GWL_EXSTYLE)` + `GetClassNameW` | 日志 `启动完成: rect=WxH@(x,y); client=WxH; dpi=N`；exstyle 含 `0x80`(TOOLWINDOW) 与 `0x80000`(LAYERED)；类名非空 | AUTO |
| T02 | 桌面层挂载 | 日志 + `GetParent` + 父进程映像名 | 日志 `已挂到 WorkerW（strategy）`，strategy ∈ {progman-child, progman-child-first, top-level-workerw}；`GetParent(hwnd)` 类名 ∈ {WorkerW, Progman}；父进程 = `explorer.exe` | AUTO |
| T03 | 焦点链路（前置） | `PostMessage` 客户区点按 → 自激活 → `GetForegroundWindow` | 3 次重试内 `GetForegroundWindow() == hwnd`；否则本用例 FAIL 且 T04/T05/T06/T09/T10/T14 标 BLOCKED | AUTO |
| T04 | 键盘输入链路 | `SendInput`（`KEYEVENTF_UNICODE`）打一行 pwsh → 探针文件 | `Set-Content $env:TEMP\aegis-t04.txt OK` 执行后 8s 内文件出现且内容 `OK` | AUTO |
| T05 | 鼠标选中复制 | `PostMessage` 拖选 + `SendInput` Ctrl+C | 日志 `已复制 N 个字符`（N≥3）+ `Get-Clipboard` 含唯一 marker `AEGIS_SEL_<round>` | AUTO |
| T06 | Ctrl+C 无选区=中断 | `Start-Sleep -Seconds 20` → 无选区 Ctrl+C → 再打探针 | 中断生效则第二个探针文件 ≤5s 出现（未生效需等 20s，阈值 8s 可区分） | AUTO |
| T07 | 滚动回看 | `PostMessage WM_MOUSEWHEEL`（`wParam=(120<<16)`）+ 屏幕区域截图 | 截图 hash 变化（证明视口动了）+ 打字后回到基线（`display_offset` 归零的行为可由「打字后截图 ≈ 滚动前基线」近似）；**「一格 3 行」的量化交人看** | SEMI |
| T08 | 粘贴闸门①剥离/空 | `SetClipboardUnicode("\x01\x02\x1b")` + Ctrl+V | 1.5s 内**不出现** `粘贴请求:` 日志（负断言）+ 截图供人看 overlay | SEMI |
| T09 | 粘贴闸门②多行确认 | 两行剪贴板 + 首次 Ctrl+V + 3s 内二次 Ctrl+V | 依次出现 `粘贴请求: N 字符 2 行，paste_guard=true` → `多行粘贴：等待二次确认` → `写入粘贴内容 N 字节`；首次后 1.5s 内**无** `写入粘贴内容` | AUTO |
| T10 | 粘贴闸门③超时 + bracketed | 多行 → 首按 → 等 3.5s → 再按；另用 `\e[?2004h` 开括号模式后贴单行 | 超时后出现**第二条** `粘贴请求:` 而非直接 `写入`，第三次才 `写入`；括号模式下贴 `-AEGIS-BP-` 的探针文件内容无 `\x1b` 残留 | AUTO |
| T11 | 配置热重载 | 改 `%APPDATA%\TLShell\config.toml` 的 `opacity=0.72`/`font.size=16` → 等 3s；再写同内容；再写坏 TOML | `配置已热重载` + `GetLayeredWindowAttributes` alpha == `round(0.72*255)=184`；同内容 → `配置文件有改动但内容等价，跳过`；坏 TOML → 进程存活且功能仍可用 | AUTO |
| T12 | 位置持久化 | `SetWindowPos(120,160,700,300)` → `PostMessage WM_EXITSIZEMOVE` → 重启；再 `--reset-position` | `已保存位置: 700x300@120,160`（debug）+ `state.json` 字段一致；重启日志 `恢复上次位置: 700x300@120,160` + `GetWindowRect` 一致；`--reset-position` 时无 `恢复` 行、几何 = 900x420@40,40 | AUTO |
| T13 | 单实例唤醒 | 隐藏后第二次启动 `tlshell.exe` | 子进程 exit 0 ≤3s；其日志 `已有实例在运行，已尝试唤醒`；`Get-Process tlshell` 计数 = 1；`IsWindowVisible(hwnd)` 由 false 变 true | AUTO |
| T14 | 退出热键 / 忽略 WM_CLOSE | 先 `PostMessage WM_CLOSE`，再 `SendInput Ctrl+Alt+Q` | WM_CLOSE 后进程存活且日志 `忽略外部 WM_CLOSE`；注入热键后日志 `收到退出热键` 且进程 ≤5s 退出 | AUTO |
| T15 | DPI 变更 | `PostMessage WM_DPICHANGED`，`wParam=144|(144<<16)` | 日志 `DPI 变更为 144`；无 `尺寸同步失败`；客户区宽度 ≥ `scale(480,96,144)=720`（最小尺寸随 DPI 生效） | AUTO（仅代码路径） |
| T16 | 错误显示到界面 | 默认配置文件写非法 `shell.program` | `diag::error` 级 `shell 解析失败: …`；进程存活不闪退；截图存证供人看终端里那行红字 | SEMI |
| T17 | 命中区/缩放带 | `SendMessage WM_NCHITTEST`（`lParam` 必须换算成**屏幕坐标**） | 正文中心 → `HTCLIENT(1)`；左边缘 `x<5` → `HTLEFT(10)`；距顶 `y<max(scale(padding),12)` 且 `x≥16` → `HTCAPTION(2)`；`(8,8)` → `HTTOPLEFT(13)`（四角 `CORNER_BAND=16` 优先于边带） | AUTO |

> T17 是额外收获：README 说「拖内边距移动、拖四边缩放」，而这三件事的判定核心就是 `WM_NCHITTEST` 的返回值，`SendMessage` 一问就知道，不必真人拖。这条把「命中区手感」从 MANUAL 拉回 AUTO，只把「拖起来跟手不跟手」留人工。

---

## 3. 目录与调用方式

```
scripts/fulltest/
  run-all.ps1              # 编排器：N 轮 + 表格 + flake 统计 + 退出码
  lib/Win32.ps1            # 单文件 Add-Type：消息常量 / SendInput / 回读 / 截图 / 剪贴板
  lib/LogAssert.ps1        # 增量日志游标 + Wait/Assert 助手
  lib/App.ps1             # 实例生命周期：起停/找 hwnd/客户区点按/打字/和弦/配置写入
  stage-b-selftest.ps1     # B 档
  stage-c-gui.ps1          # C 档 17 个用例
  stage-d-manual.ps1       # 生成人工清单（含截图路径），不判定
  fixtures/config.baseline.toml
```

```powershell
# 推荐入口（6 轮，C 档必须在前台桌面会话里跑）
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\fulltest\run-all.ps1 -Rounds 6
# 单轮快速回归（提交前）
powershell -NoProfile -File scripts\fulltest\run-all.ps1 -Rounds 1 -SkipUnit
# 只跑某几个用例（调试执行器自身）
powershell -NoProfile -File scripts\fulltest\run-all.ps1 -Rounds 1 -Only T09,T10,T12
```

产出：`docs/aegis/reviews/fulltest-runs/<yyyyMMdd-HHmmss>/`
`report.md`（人读表格）、`result.json`（机器读，可 diff）、`r<k>/*.png`（截图证据）、`selftest-r<k>.json`。

---

## 4. 脚本骨架（可直接落地）

### 4.1 `scripts/fulltest/run-all.ps1`

```powershell
<#
  TLShell 全量功能测试执行器。
  退出码：0 = 全部 AUTO 通过；1 = 有 AUTO 失败；2 = 执行器自身异常（前置/环境）。
#>
[CmdletBinding()]
param(
  [int]   $Rounds = 6,
  [switch]$SkipUnit,          # 跳过 A 档
  [switch]$SkipGui,           # 跳过 C 档（无人值守会话只跑 A+B）
  [switch]$UnitEveryRound,    # A 档每轮都跑（默认只在开头跑一次）
  [switch]$RequireManual,     # 人工项未勾选也算失败（CI 不用）
  [string[]]$Only,            # 只跑指定用例 ID
  [int]   $CaseTimeoutSec = 30
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
Set-Location $root
$exe = Join-Path $root 'target\release\tlshell.exe'

. (Join-Path $PSScriptRoot 'lib\Win32.ps1')
. (Join-Path $PSScriptRoot 'lib\LogAssert.ps1')
. (Join-Path $PSScriptRoot 'lib\App.ps1')

$stamp   = Get-Date -Format 'yyyyMMdd-HHmmss'
$runDir  = Join-Path $root "docs\aegis\reviews\fulltest-runs\$stamp"
New-Item -ItemType Directory -Force -Path $runDir | Out-Null

if (-not (Test-Path $exe)) { & (Join-Path $root 'scripts\gate.ps1') }   # 顺带保证 release 构建

$rows = [System.Collections.Generic.List[object]]::new()

# ---- A 档：逻辑层 -------------------------------------------------------
if (-not $SkipUnit) {
  Write-Host '==> A  cargo test' -ForegroundColor Cyan
  $out = & cargo test 2>&1 | Out-String
  $out | Set-Content (Join-Path $runDir 'cargo-test.log')
  $ok = ($LASTEXITCODE -eq 0) -and ($out -match 'test result: ok\. (\d+) passed')
  $rows.Add([pscustomobject]@{ Id='A'; Round=0; Class='AUTO'; Status=$(if($ok){'PASS'}else{'FAIL'});
      Detail=$(if($out -match 'test result: ok\. (\d+) passed'){"$($Matches[1]) passed"}else{'见 cargo-test.log'}); Evidence='cargo-test.log' })
}

# ---- 每轮 ---------------------------------------------------------------
for ($r = 1; $r -le $Rounds; $r++) {
  $roundDir = Join-Path $runDir "r$r"
  New-Item -ItemType Directory -Force -Path $roundDir | Out-Null
  Write-Host "==> ROUND $r/$Rounds" -ForegroundColor Cyan

  $restore = Enter-CleanEnvironment -Root $root -RoundDir $roundDir   # 备份并铺 baseline
  try {
    if ($UnitEveryRound -and -not $SkipUnit) {
      $o = & cargo test 2>&1 | Out-String
      $rows.Add([pscustomobject]@{ Id='A'; Round=$r; Class='AUTO';
        Status=$(if($LASTEXITCODE -eq 0){'PASS'}else{'FAIL'}); Detail=''; Evidence="$roundDir\cargo-test.log" })
      $o | Set-Content "$roundDir\cargo-test.log"
    }

    $rows.AddRange([object[]](& (Join-Path $PSScriptRoot 'stage-b-selftest.ps1') -Exe $exe -RoundDir $roundDir -Round $r))
    if (-not $SkipGui) {
      $rows.AddRange([object[]](& (Join-Path $PSScriptRoot 'stage-c-gui.ps1') -Exe $exe -RoundDir $roundDir -Round $r `
                                  -Only $Only -TimeoutSec $CaseTimeoutSec))
    }
    $rows.AddRange([object[]](& (Join-Path $PSScriptRoot 'stage-d-manual.ps1')   -RoundDir $roundDir -Round $r))
  }
  finally {
    Exit-CleanEnvironment -Restore $restore     # 恢复 config/state/hwnd，杀掉残留实例
  }
}

# ---- 汇总 ---------------------------------------------------------------
$ids = $rows.Id | Where-Object { $_ -ne 'A' } | Select-Object -Unique
$summary = foreach ($id in $ids) {
  $set = @($rows | Where-Object Id -eq $id)
  $cells = 1..$Rounds | ForEach-Object {
    $c = $set | Where-Object Round -eq $_ | Select-Object -First 1
    switch ($c.Status) { 'PASS' { 'PASS' } 'FAIL' { '**FAIL**' } 'BLOCKED' { 'blk' } 'SEMI' { 'semi' } 'PENDING' { 'pend' } default { '?' } }
  }
  $flake = (@($set.Status | Select-Object -Unique).Count -gt 1) -and ($set.Status -contains 'PASS') -and ($set.Status -contains 'FAIL')
  [pscustomobject]@{ Id=$id; Class=$set[0].Class; Cells=$cells; Flake=$flake
                     Detail=($set | Where-Object Status -eq 'FAIL' | Select-Object -First 1).Detail
                     Evidence=($set | Select-Object -Last 1).Evidence }
}

$head = '| 用例 | 类 | ' + ((1..$Rounds | ForEach-Object { "R$_" }) -join ' | ') + ' | flake | 备注 |'
$sep  = '|' + ('---|' * ($Rounds + 4))
$body = $summary | ForEach-Object {
  "| $($_.Id) | $($_.Class) | $($_.Cells -join ' | ') | $(if($_.Flake){'⚠'}else{''}) | $($_.Detail) |"
}
$md = @("# TLShell 全量功能测试 $stamp", '', "轮次：$Rounds　用例：$($summary.Count)", '', $head, $sep) + $body
$md | Set-Content (Join-Path $runDir 'report.md') -Encoding utf8
$rows | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $runDir 'result.json') -Encoding utf8
$md | ForEach-Object { Write-Host $_ }

$hardFail = @($rows | Where-Object { $_.Class -eq 'AUTO' -and $_.Status -eq 'FAIL' })
$manualOpen = @($rows | Where-Object { $_.Class -in 'MANUAL','SEMI' -and $_.Status -eq 'PENDING' })
if ($hardFail.Count) { Write-Host "FAIL: $($hardFail.Count) 个自动用例未通过 -> $runDir" -ForegroundColor Red; exit 1 }
if ($RequireManual -and $manualOpen.Count) { Write-Host "FAIL: $($manualOpen.Count) 个人工项未勾选" -ForegroundColor Red; exit 1 }
Write-Host "PASS（人工项 $($manualOpen.Count) 待勾选）-> $runDir" -ForegroundColor Green
exit 0
```

### 4.2 `scripts/fulltest/lib/Win32.ps1`（P/Invoke 与助手）

```powershell
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.Drawing

if (-not ('Tlshell.W32' -as [type])) {
Add-Type -Language CSharp -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

namespace Tlshell {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT {
    public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo;
  }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT {
    public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo;
  }
  [StructLayout(LayoutKind.Sequential)] public struct HARDWAREINPUT { public uint uMsg; public ushort wParamL, wParamH; }
  // union 必须完整声明三个成员，否则 Marshal.SizeOf 得到 32 而系统要求 40，
  // SendInput 直接返回 0 / ERROR_INVALID_PARAMETER。
  [StructLayout(LayoutKind.Explicit)] public struct INPUTUNION {
    [FieldOffset(0)] public MOUSEINPUT mi;
    [FieldOffset(0)] public KEYBDINPUT ki;
    [FieldOffset(0)] public HARDWAREINPUT hi;
  }
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public INPUTUNION u; }

  public static class W32 {
    public const uint INPUT_KEYBOARD = 1, KEYEVENTF_KEYUP = 2, KEYEVENTF_UNICODE = 4;
    public const int  WM_SETFOCUS = 0x0007, WM_CLOSE = 0x0010, WM_KEYDOWN = 0x0100, WM_CHAR = 0x0102;

    // 结构体一律在 C# 侧装填：PowerShell 对 struct 的嵌套字段赋值不可靠
    [DllImport("user32.dll", SetLastError=true)] static extern uint SendInput(uint n, INPUT[] p, int cb);
    static int Cb { get { return Marshal.SizeOf(typeof(INPUT)); } }

    /// 逐字符注入（KEYEVENTF_UNICODE）：走 WM_KEYDOWN/WM_CHAR 的正规路径，
    /// 因此 app 的 current_modifiers() 与 suppress_next_char 去重逻辑都被真实触发。
    public static int SendUnicodeText(string text, int keyDelayMs) {
      int n = 0;
      foreach (char ch in text) {
        INPUT[] pair = new INPUT[2];
        pair[0].type = 1; pair[0].u.ki.wVk = 0; pair[0].u.ki.wScan = (ushort)ch; pair[0].u.ki.dwFlags = KEYEVENTF_UNICODE;
        pair[1].type = 1; pair[1].u.ki.wVk = 0; pair[1].u.ki.wScan = (ushort)ch; pair[1].u.ki.dwFlags = KEYEVENTF_UNICODE | KEYEVENTF_KEYUP;
        n += (int)SendInput(2, pair, Cb);
        if (keyDelayMs > 0) System.Threading.Thread.Sleep(keyDelayMs);
      }
      return n;
    }

    /// 和弦注入：按下顺序 = 数组顺序，抬起反序。Ctrl+Alt+Q 用 {0x11,0x12,0x51}。
    public static int SendVkChord(int[] vks, int holdMs) {
      int n = 0;
      foreach (int vk in vks) {
        INPUT[] a = new INPUT[1]; a[0].type = 1; a[0].u.ki.wVk = (ushort)vk; a[0].u.ki.dwFlags = 0;
        n += (int)SendInput(1, a, Cb); System.Threading.Thread.Sleep(15);
      }
      System.Threading.Thread.Sleep(holdMs);
      for (int i = vks.Length - 1; i >= 0; i--) {
        INPUT[] a = new INPUT[1]; a[0].type = 1; a[0].u.ki.wVk = (ushort)vks[i]; a[0].u.ki.dwFlags = KEYEVENTF_KEYUP;
        n += (int)SendInput(1, a, Cb); System.Threading.Thread.Sleep(15);
      }
      return n;
    }
    public const int  WM_MOUSEMOVE = 0x0200, WM_LBUTTONDOWN = 0x0201, WM_LBUTTONUP = 0x0202;
    public const int  WM_MOUSEWHEEL = 0x020A, WM_ENTERSIZEMOVE = 0x0231, WM_EXITSIZEMOVE = 0x0232;
    public const int  WM_NCHITTEST = 0x0084, WM_NCLBUTTONDOWN = 0x00A1, WM_DPICHANGED = 0x02E0;
    public const int  GWL_EXSTYLE = -20, LWA_ALPHA = 0x2, SWP_NOSENDCHANGING = 0x0400;

    [DllImport("user32.dll", SetLastError=true)] public static extern bool PostMessageW(IntPtr h, int msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, int msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetParent(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll", EntryPoint="GetWindowLongPtrW")] public static extern IntPtr GetWindowLongPtrW(IntPtr h, int i);
    [DllImport("user32.dll")] public static extern bool GetLayeredWindowAttributes(IntPtr h, out uint key, out byte alpha, out uint flags);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
    [DllImport("user32.dll", SetLastError=true)] public static extern bool OpenClipboard(IntPtr h);
    [DllImport("user32.dll")] public static extern bool CloseClipboard();
    [DllImport("user32.dll")] public static extern bool EmptyClipboard();
    [DllImport("user32.dll")] public static extern IntPtr SetClipboardData(uint fmt, IntPtr mem);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  }
}
'@
}

# ---- 进程/窗口 -----------------------------------------------------------
function Get-TlshellHwnd {
  param([Parameter(Mandatory)][string]$ConfigDir)
  $f = Join-Path $ConfigDir 'hwnd.txt'
  if (-not (Test-Path $f)) { return [IntPtr]::Zero }
  $raw = (Get-Content $f -Raw).Trim()
  $v = 0L; if (-not [long]::TryParse($raw, [ref]$v)) { return [IntPtr]::Zero }
  $h = [IntPtr]$v
  if (-not [Tlshell.W32]::IsWindow($h)) { return [IntPtr]::Zero }
  return $h
}

function Get-ExStyle([IntPtr]$Hwnd) { return [int64][Tlshell.W32]::GetWindowLongPtrW($Hwnd, [Tlshell.W32]::GWL_EXSTYLE) }

function Get-ClassOf([IntPtr]$Hwnd) {
  $sb = New-Object System.Text.StringBuilder 256
  $null = [Tlshell.W32]::GetClassNameW($Hwnd, $sb, 256)
  return $sb.ToString()
}

function Get-Alpha([IntPtr]$Hwnd) {
  $k = [uint32]0; $a = [byte]0; $f = [uint32]0
  if ([Tlshell.W32]::GetLayeredWindowAttributes($Hwnd, [ref]$k, [ref]$a, [ref]$f)) { return [int]$a }
  return -1
}

function Get-WindowRect([IntPtr]$Hwnd) {
  $r = New-Object Tlshell.RECT
  if (-not [Tlshell.W32]::GetWindowRect($Hwnd, [ref]$r)) { throw "GetWindowRect 失败" }
  return $r
}

# ---- 输入注入 -----------------------------------------------------------
# 结构装填全在 C# 侧（见上），PowerShell 只做参数搬运与错误翻译。
function Send-Text {
  [CmdletBinding()] param([Parameter(Mandatory)][AllowEmptyString()][string]$Text, [int]$KeyDelayMs = 12)
  $n = [Tlshell.W32]::SendUnicodeText($Text, $KeyDelayMs)
  if ($n -ne (2 * $Text.Length)) { throw "SendInput 只发出 $n/$(2*$Text.Length) 个事件（UIPI 提权阻断？）" }
}

# 真实键盘状态才被 app 的 current_modifiers() 采信 -> 和弦必须用 SendInput 而非 PostMessage
function Send-Chord {
  param([Parameter(Mandatory)][int[]]$Vks, [int]$HoldMs = 40)   # @(0x11,0x12,0x51) = Ctrl+Alt+Q
  $n = [Tlshell.W32]::SendVkChord($Vks, $HoldMs)
  if ($n -ne (2 * $Vks.Length)) { throw "SendVkChord 事件数不符：$n" }
}

# 客户区坐标点按：走 PostMessage 不需要真实光标；app 自己在 WM_LBUTTONDOWN 里 SetForegroundWindow
function Invoke-ClientClick {
  param([IntPtr]$Hwnd, [int]$X, [int]$Y)
  $lp = [IntPtr](($Y -shl 16) -bor ($X -band 0xFFFF))
  $null = [Tlshell.W32]::PostMessageW($Hwnd, [Tlshell.W32]::WM_LBUTTONDOWN, [IntPtr]1, $lp)
  Start-Sleep -Milliseconds 40
  $null = [Tlshell.W32]::PostMessageW($Hwnd, [Tlshell.W32]::WM_LBUTTONUP, [IntPtr]0, $lp)
}

function Invoke-ClientDrag {
  param([IntPtr]$Hwnd, [int]$X1, [int]$Y1, [int]$X2, [int]$Y2, [int]$Steps = 6)
  $mk = { param($x,$y) [IntPtr]((($y) -shl 16) -bor (($x) -band 0xFFFF)) }
  $null = [Tlshell.W32]::PostMessageW($Hwnd, [Tlshell.W32]::WM_LBUTTONDOWN, [IntPtr]1, (& $mk $X1 $Y1))
  for ($s = 1; $s -le $Steps; $s++) {
    $x = $X1 + [int](($X2 - $X1) * $s / $Steps); $y = $Y1 + [int](($Y2 - $Y1) * $s / $Steps)
    $null = [Tlshell.W32]::PostMessageW($Hwnd, [Tlshell.W32]::WM_MOUSEMOVE, [IntPtr]1, (& $mk $x $y))
    Start-Sleep -Milliseconds 25
  }
  $null = [Tlshell.W32]::PostMessageW($Hwnd, [Tlshell.W32]::WM_LBUTTONUP, [IntPtr]0, (& $mk $X2 $Y2))
}

function Invoke-Wheel {
  param([IntPtr]$Hwnd, [int]$Notches)          # 正=看历史（app: delta/120*3 行）
  $delta = [int](120 * $Notches)
  $w = [IntPtr](([int64]($delta -band 0xFFFF)) -shl 16)   # 低 16 位是按键状态，app 只取高 16 位
  $r = Get-WindowRect $Hwnd
  $lp = [IntPtr]((((($r.Top + $r.Bottom) / 2)) -shl 16) -bor (((($r.Left + $r.Right) / 2)) -band 0xFFFF))
  $null = [Tlshell.W32]::PostMessageW($Hwnd, [Tlshell.W32]::WM_MOUSEWHEEL, $w, $lp)
}

# WM_NCHITTEST 的 lParam 是**屏幕坐标**（app 侧 ScreenToClient 反算），必须转换后发
function Invoke-NcHitTest {
  param([IntPtr]$Hwnd, [int]$ClientX, [int]$ClientY)
  $pt = New-Object Tlshell.POINT
  $pt.X = $ClientX; $pt.Y = $ClientY
  if (-not [Tlshell.W32]::ClientToScreen($Hwnd, [ref]$pt)) { throw 'ClientToScreen 失败' }
  $lp = [IntPtr]((($pt.Y) -shl 16) -bor (($pt.X) -band 0xFFFF))
  return [int][Tlshell.W32]::SendMessageW($Hwnd, [Tlshell.W32]::WM_NCHITTEST, [IntPtr]0, $lp)
}

# ---- 剪贴板 -------------------------------------------------------------
function Set-ClipboardExact {
  param([Parameter(Mandatory)][AllowEmptyString()][string]$Text)
  for ($i = 0; $i -lt 10; $i++) {
    try { Set-Clipboard -Value $Text -ErrorAction Stop; Start-Sleep -Milliseconds 60; return } catch { Start-Sleep -Milliseconds 80 }
  }
  throw '设置剪贴板失败（被其它进程占用？）'
}

# ---- 截图（屏幕区域 = 合成后的真实观感，WorkerW 子窗口也能截）-------------
function Save-ScreenPng {
  param([IntPtr]$Hwnd, [Parameter(Mandatory)][string]$Path, [int]$Pad = 0)
  $r = Get-WindowRect $Hwnd
  $w = [Math]::Max(1, $r.Right - $r.Left + 2*$Pad); $h = [Math]::Max(1, $r.Bottom - $r.Top + 2*$Pad)
  $bmp = New-Object System.Drawing.Bitmap $w, $h
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  try {
    $g.CopyFromScreen($r.Left - $Pad, $r.Top - $Pad, 0, 0, (New-Object System.Drawing.Size $w, $h))
    $bmp.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
  } finally { $g.Dispose(); $bmp.Dispose() }
  return $Path
}

# 用于「视口动没动」的粗判；真正的 3 行/格量化仍交人工
function Get-ImageHash([string]$Path) { (Get-FileHash $Path -Algorithm SHA256).Hash.Substring(0,16) }
```

### 4.3 `scripts/fulltest/lib/LogAssert.ps1`

```powershell
Set-StrictMode -Version Latest

function Get-TlshellLogPath {
  $base = if ($env:LOCALAPPDATA) { $env:LOCALAPPDATA } else { $env:TEMP }
  return (Join-Path $base 'TLShell\logs\tlshell.log')
}

# 游标 = 启动前文件长度；读增量时若长度变短视为轮转，重置为 0
function New-LogCursor {
  $p = Get-TlshellLogPath
  $len = if (Test-Path $p) { (Get-Item $p).Length } else { 0 }
  return [pscustomobject]@{ Path = $p; Offset = [int64]$len }
}

function Read-LogSince {
  param([Parameter(Mandatory)]$Cursor)
  if (-not (Test-Path $Cursor.Path)) { return '' }
  $len = (Get-Item $Cursor.Path).Length
  if ($len -lt $Cursor.Offset) { $Cursor.Offset = 0 }
  $fs = [System.IO.File]::Open($Cursor.Path, 'Open', 'Read', 'ReadWrite')
  try {
    $null = $fs.Seek($Cursor.Offset, 'Begin')
    $sr = New-Object System.IO.StreamReader($fs)
    $text = $sr.ReadToEnd()
    $Cursor.Offset = $fs.Position
    return $text
  } finally { $fs.Dispose() }
}

function Wait-LogMatch {
  param([Parameter(Mandatory)]$Cursor, [Parameter(Mandatory)][string]$Pattern,
        [int]$TimeoutMs = 5000, [switch]$Collect)
  $deadline = (Get-Date).AddMilliseconds($TimeoutMs)
  $acc = ''
  do {
    Start-Sleep -Milliseconds 100
    $acc += Read-LogSince -Cursor $Cursor
    if ($acc -match $Pattern) { if ($Collect) { return $acc } else { return $true } }
  } while ((Get-Date) -lt $deadline)
  if ($Collect) { return $acc }
  return $false
}

# 负断言：在窗口期内不得出现
function Test-LogAbsent {
  param([Parameter(Mandatory)]$Cursor, [Parameter(Mandatory)][string]$Pattern, [int]$WindowMs = 1500)
  $acc = ''
  $deadline = (Get-Date).AddMilliseconds($WindowMs)
  do { Start-Sleep -Milliseconds 100; $acc += Read-LogSince -Cursor $Cursor } while ((Get-Date) -lt $deadline)
  return (-not ($acc -match $Pattern))
}

function Get-LogText([Parameter(Mandatory)]$Cursor) { Read-LogSince -Cursor $Cursor }
```

### 4.4 `scripts/fulltest/lib/App.ps1`（实例生命周期与干净环境）

```powershell
Set-StrictMode -Version Latest

function Get-AppDir {
  $base = if ($env:APPDATA) { $env:APPDATA } else { '.' }
  return (Join-Path $base 'TLShell')
}

# 每轮开始：备份用户真实状态，铺 baseline；结束恢复。
# 互斥体 Local\TLShell.SingleInstance 决定同一时刻只能有一个真实实例。
function Enter-CleanEnvironment {
  param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$RoundDir)
  $app = Get-AppDir
  New-Item -ItemType Directory -Force -Path $app | Out-Null
  $backup = Join-Path $RoundDir 'user-state'
  New-Item -ItemType Directory -Force -Path $backup | Out-Null
  foreach ($n in 'config.toml','state.json','hwnd.txt') {
    $p = Join-Path $app $n
    if (Test-Path $p) { Copy-Item $p (Join-Path $backup $n) -Force }
  }
  Copy-Item (Join-Path $Root 'scripts\fulltest\fixtures\config.baseline.toml') (Join-Path $app 'config.toml') -Force
  Remove-Item (Join-Path $app 'state.json'), (Join-Path $app 'hwnd.txt') -ErrorAction SilentlyContinue
  return [pscustomobject]@{ AppDir = $app; Backup = $backup }
}

function Exit-CleanEnvironment {
  param([Parameter(Mandatory)]$Restore)
  Get-Process tlshell -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
  Start-Sleep -Milliseconds 300
  $app = $Restore.AppDir
  foreach ($n in 'config.toml','state.json','hwnd.txt') {
    $b = Join-Path $Restore.Backup $n
    if (Test-Path $b) { Copy-Item $b (Join-Path $app $n) -Force }
    else { Remove-Item (Join-Path $app $n) -ErrorAction SilentlyContinue }
  }
}

# 启动真实实例，等到「启动完成」日志行（含 rect/client/dpi）。
function Start-Instance {
  param([Parameter(Mandatory)][string]$Exe, [string[]]$ExtraArgs = @(), [int]$BootTimeoutMs = 15000, [switch]$SkipLogLevel)
  $env:TLSHELL_LOG = 'debug'                       # 见 F2：info/debug 必须显式开启
  $cursor = New-LogCursor
  $argList = @($ExtraArgs)                         # 注意：不能叫 $args，那是自动变量
  $p = Start-Process -FilePath $Exe -ArgumentList $argList -PassThru
  $boot = Wait-LogMatch -Cursor $cursor -Pattern '启动完成: rect=(\d+)x(\d+)@\((-?\d+),(-?\d+)\); client=(\d+)x(\d+); dpi=(\d+)' -TimeoutMs $BootTimeoutMs -Collect
  if ($boot -notmatch '启动完成') { throw "实例未在 ${BootTimeoutMs}ms 内完成启动：$boot" }
  $d = @{ RectW=[int]$Matches[1]; RectH=[int]$Matches[2]; X=[int]$Matches[3]; Y=[int]$Matches[4]
          ClientW=[int]$Matches[5]; ClientH=[int]$Matches[6]; Dpi=[int]$Matches[7] }
  $hwnd = [IntPtr]::Zero
  $deadline = (Get-Date).AddSeconds(5)
  while ($hwnd -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 100
    $hwnd = Get-TlshellHwnd -ConfigDir (Get-AppDir)
  }
  # 前台锁兜底：AttachThreadInput 后再试一次
  if (-not ($hwnd -eq [IntPtr]::Zero)) {
    for ($i = 0; $i -lt 3 -and [Tlshell.W32]::GetForegroundWindow() -ne $hwnd; $i++) {
      Invoke-ClientClick -Hwnd $hwnd -X 8 -Y 8
      if ([Tlshell.W32]::GetForegroundWindow() -ne $hwnd) {
        $tid = [Tlshell.W32]::GetCurrentThreadId(); [uint32]$pid = 0
        $wtid = [Tlshell.W32]::GetWindowThreadProcessId($hwnd, [ref]$pid)
        $null = [Tlshell.W32]::AttachThreadInput($tid, $wtid, $true)
        $null = [Tlshell.W32]::SetForegroundWindow($hwnd)
        $null = [Tlshell.W32]::AttachThreadInput($tid, $wtid, $false)
      }
      Start-Sleep -Milliseconds 150
    }
  }
  return [pscustomobject]@{ Process = $p; Hwnd = $hwnd; Cursor = $cursor; Boot = $d; Foreground = ([Tlshell.W32]::GetForegroundWindow() -eq $hwnd) }
}

# 唯一温和退出路径：注入 Ctrl+Alt+Q（WM_HOTKEY）。--quit 是空操作，WM_CLOSE 被忽略。
function Stop-InstanceSoft {
  param([Parameter(Mandatory)]$Inst, [int]$TimeoutMs = 6000)
  Send-Chord -Vks @(0x11, 0x12, 0x51)     # VK_CONTROL, VK_MENU, 'Q'
  $ok = Wait-LogMatch -Cursor $Inst.Cursor -Pattern '收到退出热键' -TimeoutMs 2000
  $exited = $Inst.Process.WaitForExit($TimeoutMs)
  return [pscustomobject]@{ Logged = $ok; Exited = $exited }
}

function Set-BaselineConfig {
  param([Parameter(Mandatory)][string]$Root, [hashtable]$Replace = @{})
  $app = Get-AppDir
  $text = Get-Content (Join-Path $Root 'scripts\fulltest\fixtures\config.baseline.toml') -Raw
  foreach ($k in $Replace.Keys) { $text = $text.Replace($k, [string]$Replace[$k]) }
  Set-Content -Path (Join-Path $app 'config.toml') -Value $text -Encoding utf8 -NoNewline
}
```

### 4.5 `scripts/fulltest/fixtures/config.baseline.toml`

```toml
# 执行器基线：与 Config::default() 一致，只把 shell 换成可被探针驱动的最小 pwsh 调用。
[shell]
program = "pwsh.exe"
args = []

[window]
width = 900
height = 420
x = 40
y = 40
opacity = 0.88
padding = 10
border_radius = 8

[font]
family = "Cascadia Mono"
size = 14.0
line_height = 1.25
fallback_family = ""

[colors]
background = "#1e1e2e"
foreground = "#cdd6f4"
cursor = "#f5e0dc"
selection = "#585b70"
focus_ring = "#89b4fa"

[keys]
quit = "Ctrl+Alt+Q"
hide = "Ctrl+Alt+H"

[security]
osc52 = false
allow_custom_shell = false
paste_guard = true
```

### 4.6 `scripts/fulltest/stage-b-selftest.ps1`

```powershell
[CmdletBinding()] param([string]$Exe, [string]$RoundDir, [int]$Round)
Set-StrictMode -Version Latest
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
. (Join-Path $PSScriptRoot 'lib\Win32.ps1')

$report = Join-Path $RoundDir 'selftest.json'
Remove-Item $report -ErrorAction SilentlyContinue
$p = Start-Process -FilePath $Exe -ArgumentList @('--selftest','--report',"`"$report`"") -PassThru -Wait
$rows = [System.Collections.Generic.List[object]]::new()

if (-not (Test-Path $report)) {
  $rows.Add([pscustomobject]@{ Id='B'; Round=$Round; Class='AUTO'; Status='FAIL'
    Detail="未生成报告（exit=$($p.ExitCode)）"; Evidence=$RoundDir }); return $rows
}
$json = Get-Content $report -Raw | ConvertFrom-Json
foreach ($c in $json.checks) {
  # 全通过路径固定 11 项：module_handle … destroy_window；任一检查失败会提前 finish，
  # 所以「行数 < 11」本身就是信号（且此时 passed=false）。hotkey_parse 只在 parse 失败时出现。
  $rows.Add([pscustomobject]@{
    Id = "B:$($c.name)"; Round = $Round; Class = 'AUTO'
    Status = $(if ($c.ok) { 'PASS' } else { 'FAIL' })
    Detail = $(if ($c.ok) { '' } else { [string]$c.detail }); Evidence = 'selftest.json' })
}
if ($p.ExitCode -ne 0 -and -not ($rows | Where-Object Status -eq 'FAIL')) {
  $rows.Add([pscustomobject]@{ Id='B:exit'; Round=$Round; Class='AUTO'; Status='FAIL'; Detail="exit=$($p.ExitCode)"; Evidence='selftest.json' })
}
# 提前 return 会截断 checks：行数不足 11 = 有检查没跑到，必须显式报出来
if ($json.checks.Count -ne 11) {
  $rows.Add([pscustomobject]@{ Id='B:count'; Round=$Round; Class='AUTO'; Status='FAIL'
    Detail="checks=$($json.checks.Count)（期望 11，可能有检查项被提前 return 截断）"; Evidence='selftest.json' })
}
if (-not $json.passed) {
  $rows.Add([pscustomobject]@{ Id='B:passed'; Round=$Round; Class='AUTO'; Status='FAIL'
    Detail='Report.passed=false'; Evidence='selftest.json' })
}
return $rows
```

### 4.7 `scripts/fulltest/stage-c-gui.ps1`（骨架 + 三个代表用例）

```powershell
[CmdletBinding()] param([string]$Exe, [string]$RoundDir, [int]$Round,
                         [string[]]$Only, [int]$TimeoutSec = 30)
Set-StrictMode -Version Latest
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
. (Join-Path $PSScriptRoot 'lib\Win32.ps1')
. (Join-Path $PSScriptRoot 'lib\LogAssert.ps1')
. (Join-Path $PSScriptRoot 'lib\App.ps1')

$rows  = [System.Collections.Generic.List[object]]::new()
$app   = Get-AppDir
$marker = "AEGIS_$Round"
$probe  = Join-Path $env:TEMP "aegis-$Round-t04.txt"

function Add-Row($id, $status, $class, $detail, $evidence) {
  $rows.Add([pscustomobject]@{ Id=$id; Round=$Round; Class=$class; Status=$status; Detail=$detail; Evidence=$evidence })
}
function Want($id) { return (-not $Only) -or ($Only -contains $id) }

$inst = $null
try {
  Remove-Item $probe -ErrorAction SilentlyContinue
  $inst = Start-Instance -Exe $Exe

  # --- T01 窗口创建与样式 -------------------------------------------------
  if (Want 'T01') {
    $ex = Get-ExStyle $inst.Hwnd
    $cls = Get-ClassOf $inst.Hwnd
    $ok = ($inst.Hwnd -ne [IntPtr]::Zero) -and (($ex -band 0x80) -ne 0) -and (($ex -band 0x80000) -ne 0) -and $cls
    Add-Row 'T01' $(if ($ok) {'PASS'} else {'FAIL'}) 'AUTO' "exstyle=0x$('{0:x}' -f $ex) class=$cls rect=$($inst.Boot.RectW)x$($inst.Boot.RectH)@$($inst.Boot.X),$($inst.Boot.Y) dpi=$($inst.Boot.Dpi)" $RoundDir
  }

  # --- T02 桌面层挂载 ------------------------------------------------------
  if (Want 'T02') {
    $attached = Wait-LogMatch -Cursor $inst.Cursor -Pattern '已挂到 WorkerW（(progman-child|progman-child-first|top-level-workerw)）' -TimeoutMs 3000 -Collect
    $parent = [Tlshell.W32]::GetParent($inst.Hwnd)
    $pcls = if ($parent -ne [IntPtr]::Zero) { Get-ClassOf $parent } else { '' }
    $owner = ''
    if ($parent -ne [IntPtr]::Zero) {
      [uint32]$pid = 0; $null = [Tlshell.W32]::GetWindowThreadProcessId($parent, [ref]$pid)
      $owner = (Get-Process -Id $pid -ErrorAction SilentlyContinue).ProcessName
    }
    $ok = ($attached -match '已挂到 WorkerW') -and ($pcls -in @('WorkerW','Progman')) -and ($owner -eq 'explorer')
    Add-Row 'T02' $(if ($ok) {'PASS'} else {'FAIL'}) 'AUTO' "parent=$pcls owner=$owner 日志=$($attached.Trim())" $RoundDir
    if (-not $ok) { Add-Row 'T02:fallback' 'FAIL' 'AUTO' '挂载降级为普通窗口（日志：挂桌面层失败）' $RoundDir }
  }

  # --- T03 焦点链路（后续用例前置）----------------------------------------
  $fg = $false
  if (Want 'T03') {
    Invoke-ClientClick -Hwnd $inst.Hwnd -X 40 -Y 40
    for ($i = 0; $i -lt 20 -and -not $fg; $i++) {
      Start-Sleep -Milliseconds 100
      $fg = [Tlshell.W32]::GetForegroundWindow() -eq $inst.Hwnd
    }
    Add-Row 'T03' $(if ($fg) {'PASS'} else {'FAIL'}) 'AUTO' "foreground=$fg" $RoundDir
  }

  # --- T04 键盘链路（副作用探针）------------------------------------------
  if (Want 'T04' -and $fg) {
    Send-Text "Set-Content -Path '$probe' -Value OK`r"
    $ok = $false; $deadline = (Get-Date).AddSeconds(8)
    do { Start-Sleep -Milliseconds 150; if (Test-Path $probe) { $ok = ((Get-Content $probe -Raw).Trim() -eq 'OK') } } while (-not $ok -and (Get-Date) -lt $deadline)
    Add-Row 'T04' $(if ($ok) {'PASS'} else {'FAIL'}) 'AUTO' "probe=$probe exists=$(Test-Path $probe)" $RoundDir
  } elseif (Want 'T04') { Add-Row 'T04' 'BLOCKED' 'AUTO' 'T03 未取得前台' $RoundDir }

  # --- T09 粘贴闸门②：多行确认 -------------------------------------------
  if (Want 'T09' -and $fg) {
    Set-ClipboardExact "line-A-$marker`r`nline-B-$marker"
    Send-Chord @(0x11, 0x56)                         # Ctrl+V
    $r1 = Wait-LogMatch -Cursor $inst.Cursor -Pattern '粘贴请求: \d+ 字符 2 行，paste_guard=true' -TimeoutMs 2500
    $r2 = Wait-LogMatch -Cursor $inst.Cursor -Pattern '多行粘贴：等待二次确认' -TimeoutMs 1500
    $noWriteYet = Test-LogAbsent -Cursor $inst.Cursor -Pattern '写入粘贴内容' -WindowMs 1200
    Start-Sleep -Milliseconds 300
    Send-Chord @(0x11, 0x56)                         # 3s 内二次确认
    $r3 = Wait-LogMatch -Cursor $inst.Cursor -Pattern '写入粘贴内容 \d+ 字节' -TimeoutMs 3000
    $ok = $r1 -and $r2 -and $noWriteYet -and $r3
    Add-Row 'T09' $(if ($ok) {'PASS'} else {'FAIL'}) 'AUTO' "请求=$r1 待确认=$r2 未提前写入=$noWriteYet 最终写入=$r3" $RoundDir
  } elseif (Want 'T09') { Add-Row 'T09' 'BLOCKED' 'AUTO' 'T03 未取得前台' $RoundDir }

  # --- T12 位置持久化 ------------------------------------------------------
  if (Want 'T12') {
    $null = [Tlshell.W32]::SetWindowPos($inst.Hwnd, [IntPtr]::Zero, 120, 160, 700, 300, 0)
    Start-Sleep -Milliseconds 200
    $null = [Tlshell.W32]::PostMessageW($inst.Hwnd, [Tlshell.W32]::WM_EXITSIZEMOVE, [IntPtr]::Zero, [IntPtr]::Zero)
    $saved = Wait-LogMatch -Cursor $inst.Cursor -Pattern '已保存位置: 700x300@120,160' -TimeoutMs 2500 -Collect
    $stateFile = Join-Path $app 'state.json'
    $state = if (Test-Path $stateFile) { Get-Content $stateFile -Raw | ConvertFrom-Json } else { $null }
    $okSave = ($saved -match '已保存位置: 700x300@120,160') -and $state -and
              ([int]$state.window.width -eq 700) -and ([int]$state.window.height -eq 300)
    $soft = Stop-InstanceSoft -Inst $inst
    $inst2 = Start-Instance -Exe $Exe
    $restored = Wait-LogMatch -Cursor $inst2.Cursor -Pattern '恢复上次位置: 700x300@120,160' -TimeoutMs 3000
    $r = Get-WindowRect $inst2.Hwnd
    $okRect = ($r.Left -eq 120) -and ($r.Top -eq 160) -and (($r.Right-$r.Left) -eq 700)
    $inst = $inst2
    Add-Row 'T12' $(if ($okSave -and $restored -and $okRect -and $soft.Exited) {'PASS'} else {'FAIL'}) 'AUTO' `
      "保存=$okSave 恢复=$restored rect=$($r.Left),$($r.Top) 退出=$($soft.Exited)" $RoundDir
  }

  # --- T14 退出热键 / 忽略 WM_CLOSE --------------------------------------
  if (Want 'T14') {
    $null = [Tlshell.W32]::PostMessageW($inst.Hwnd, [Tlshell.W32]::WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero)
    $ignored = Wait-LogMatch -Cursor $inst.Cursor -Pattern '忽略外部 WM_CLOSE' -TimeoutMs 1500
    $alive = -not $inst.Process.HasExited
    Invoke-ClientClick -Hwnd $inst.Hwnd -X 40 -Y 40        # 退出热键是全局的，但先确保窗口在前台以便观察
    Send-Chord @(0x11, 0x12, 0x51)
    $hk = Wait-LogMatch -Cursor $inst.Cursor -Pattern '收到退出热键' -TimeoutMs 2000
    $exited = $inst.Process.WaitForExit(6000)
    $ok = $ignored -and $alive -and $hk -and $exited
    Add-Row 'T14' $(if ($ok) {'PASS'} else {'FAIL'}) 'AUTO' "忽略CLOSE=$ignored 仍存活=$alive 热键日志=$hk 退出=$exited" $RoundDir
    $inst = $null
  }

  # --- 其余用例（T05..T08,T10,T11,T13,T15,T16,T17）同构，见 §5 的要点 ------
  foreach ($id in 'T05','T06','T07','T08','T10','T11','T13','T15','T16','T17') {
    if ((Want $id) -and -not ($rows | Where-Object Id -eq $id)) {
      Add-Row $id 'PENDING' 'SEMI' '骨架已就位，按 §5 补齐断言后转 AUTO' $RoundDir
    }
  }
}
catch {
  Add-Row 'STAGE-C' 'FAIL' 'AUTO' "$($_.Exception.Message)" $RoundDir
}
finally {
  if ($inst -and -not $inst.Process.HasExited) {
    $null = Stop-InstanceSoft -Inst $inst
    if (-not $inst.Process.HasExited) { $inst.Process | Stop-Process -Force -ErrorAction SilentlyContinue }
  }
  Get-Process tlshell -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}
return $rows
```

### 4.8 `scripts/fulltest/stage-d-manual.ps1`

```powershell
[CmdletBinding()] param([string]$RoundDir, [int]$Round)
# 只生成清单，不判定；-RequireManual 时才把未勾选当失败。
$items = @(
  'opacity=0.88 下正文与壁纸的对比度是否 ≥4.5:1（白/深壁纸各一张）'
  '圆角边缘有无锯齿或亮边（放大 2x 看）'
  '桌面图标是否压在窗口之上（层级是否符合预期）'
  'Win+D 后窗口是否随桌面一起隐藏/恢复'
  '壁纸程序（Wallpaper Engine 等）运行时是否抢层'
  '全屏游戏/视频是否遮挡窗口'
  'IME 候选框位置是否贴合光标、候选条样式是否正常'
  'CJK 与 Emoji 字形是否回落正常、不出现豆腐块'
  '提示条文字样式；3s（overlay）与 30s（提权提示）消失节奏'
  '真实 DPI 切换（100%→150%）与多显示器拖动后的观感'
)
$lines = @("# 人工清单 R$Round", '', "截图目录：$RoundDir", '')
$i = 0
foreach ($t in $items) { $i++; $lines += "- [ ] M$i $t" }
$lines | Set-Content (Join-Path $RoundDir 'manual.md') -Encoding utf8
return [pscustomobject]@{ Id='D'; Round=$Round; Class='MANUAL'; Status='PENDING'
  Detail="$($items.Count) 项待人工勾选"; Evidence="$RoundDir\manual.md" }
```

---

## 5. 待补齐用例的落地要点（按 §2 断言照做即可）

- **T05 选中复制**：`Send-Text "Write-Output '$marker'`r"` → 等 500ms → 行扫描定位：对可见区每一行 `y = pad + i*lineH` 依次 `Invoke-ClientDrag (pad+2,y) → (clientW-8,y)` 再 `Send-Chord @(0x11,0x43)`，命中判据是 `已复制 N 个字符` 且 `Get-Clipboard` 含 `$marker`；失败尝试行数记进 Detail（这个「扫描」本身就是对网格行高不变量的间接验证，与长期记忆里的 8px/18px 字形网格一致时通常第 1-2 行就命中）。
- **T06 中断**：`Send-Text "Start-Sleep -Seconds 20`r"` → 无选区 `Ctrl+C` → `Send-Text "Set-Content '$probe2' OK`r"`；8s 内探针出现 = 中断生效。
- **T07 滚动**：`Save-ScreenPng` 基线 → `Invoke-Wheel 1` → 再截 → hash 不同；`Invoke-Wheel -1` 回底 → 打字 → 截图与基线距离明显变小。**「一格 3 行」的量化留人工**（要在截图上做行偏移相关，属于 SEMI 的语义边界）。
- **T08 空/控制字符**：`Set-ClipboardExact ([string][char]1 + [char]2 + [char]27)` → `Ctrl+V` → `Test-LogAbsent '粘贴请求:'`；同时 `Save-ScreenPng` 供人看 overlay 文案。
- **T10 超时 + bracketed**：多行 → `Ctrl+V` → `Start-Sleep 3.5` → `Ctrl+V` → 断言**第二条** `粘贴请求:` 出现且此时 `写入` 仍未出现 → 再 `Ctrl+V` → `写入`。bracketed：`Send-Text "[Console]::Write(""$([char]27)[?2004h"")`r"` 后贴单行 `-AEGIS-BP-`，让 shell 把它写进探针文件，断言文件内容无 `\x1b`。**注意**：`?2004h` 由 ConPTY 吸收，能验证的是「没有被包装字符污染」，不能直接观测 `ESC[200~` 字节（日志按设计不记 PTY 字节）。
- **T11 热重载**：`Set-BaselineConfig -Replace @{ 'opacity = 0.88' = 'opacity = 0.72' }` → 等 `配置已热重载`（≤3s）→ `Get-Alpha` 应等于 `[int][Math]::Round(0.72*255)=184`。注意**基线文本里那行必须精确**，否则 `Replace` 静默不生效——这本身要断言（写入后回读文件确认含 `opacity = 0.72`）。再写**相同内容**验证 `内容等价，跳过`（debug 级），最后写 `[window` 坏 TOML 验证不闪退。
- **T13 单实例唤醒**：先 `Ctrl+Alt+H` 隐藏（日志 `收到隐藏热键`）→ `IsWindowVisible` 变 false → `Start-Process $Exe` → 子进程 3s 内 exit 0 → 日志 `已有实例在运行，已尝试唤醒` → `IsWindowVisible` 变 true → `Get-Process tlshell` 计数 = 1。**注意**：`WAKE_MESSAGE` 的处理本身不打日志，唤醒结论只能由「可见性/前台变化」推断，这是实现缺口而非执行器缺口。
- **T15 DPI**：`$w=[IntPtr]((144) -bor (144 -shl 16))`；`PostMessage(hwnd, 0x02E0, $w, [IntPtr]::Zero)` → 日志 `DPI 变更为 144`；再断言无 `尺寸同步失败` 且 `GetClientRect.Width ≥ 720`。真实 DPI 只在 MANUAL 清单里。
- **T16 错误显示**：`Set-BaselineConfig -Replace @{ 'program = "pwsh.exe"' = 'program = "definitely-not-a-shell.exe"' }` → 起实例 → 断言 `shell 解析失败`（error 级，默认日志就有）且进程存活；`Save-ScreenPng` 存证。**必须在用例结束时把配置改回去并重启实例**，否则后续用例全部连锁失败（执行器用 `Enter-CleanEnvironment` 的备份恢复兜底）。
- **T17 命中区**：`src/app.rs:962-1027` 的判定顺序是「四角(`CORNER_BAND=16`) → 四边(`RESIZE_BAND=5`) → 内边距带(`max(scale(padding),12)`) → Alt → `HTCLIENT`」。所以**别拿 `(4,4)` 试 HTCAPTION**，它会先被四角吃掉。实测三点：`(clientW/2, clientH/2)`→1、`(2, clientH/2)`→10、`(clientW/2, max(12,scale(10,$dpi))) - 4`→2，再加 `(8,8)`→13 作为四角优先的回归守卫。`lParam` 是屏幕坐标，用 `ClientToScreen` 换算。

---

## 6. 只能人工 / 半自动的清单

**只能人工目视（MANUAL，10 项，见 `stage-d-manual.ps1`）**：对比度与整窗 alpha 观感、圆角锯齿、桌面图标层级、Win+D、壁纸程序抢层、全屏游戏遮挡、IME 候选框位置与样式、CJK/Emoji 字形、提示条样式与 3s/30s 节奏、真实 DPI 切换与多显示器。理由统一：**这些量要么不在进程可读状态里（壁纸/游戏层级、真实 DPI），要么判定标准是「看起来对不对」（字形、锯齿、节奏）**，`Readme` 也把 V-19/V-30/V-32 与对比度/录屏类标为未通过或未验证，执行器不该假装能替它们签字。

**半自动（SEMI，采集 + 人看）**：T07 滚动后的画面正确性、T08 的空剪贴板 overlay 文案、T16 的终端错误文本排版。这三项执行器负责产出 PNG 与日志切片，人在 `manual.md` 里勾。

**明确不能自动化的「假动作」**：`--quit`（空操作，见 F1）、`WM_CLOSE`（被忽略，是**负例**而非退出手段）、`PrintWindow` 直对 hwnd（WorkerW 子窗口 + layered，取不到内容；用屏幕区域 `CopyFromScreen` 或对父窗口截图）。

---

## 7. 已知坑（写进脚本注释，别让下一个人再踩）

1. **漏设 `TLSHELL_LOG`** → info/debug 全丢，一半断言变假阴性。`Start-Instance` 强制设 `debug`。
2. **`#![windows_subsystem="windows"]`** → 没有 stdout；一切结论只能来自 exit code、`--report` JSON、日志文件、窗口状态回读。
3. **单实例互斥体** → 上一轮残留进程会让下一轮「静默 exit 0」。`Exit-CleanEnvironment` 每轮强杀 + `hwnd.txt` 删除，缺一不可。
4. **`hwnd.txt` 是十进制 `isize`** → 解析要 `[long]::TryParse`，别当十六进制。
5. **日志会轮转**（>1MiB，保 3 份）→ 长跑必须用字节游标并在长度变短时重置，否则断言读到旧内容或永远读不到。
6. **前台锁** → `PostMessage` 点按让 app 自己 `SetForegroundWindow` 是主路径，`AttachThreadInput` 是兜底；拿不到前台就把依赖用例标 `BLOCKED`，不要报 FAIL。
7. **`SendInput` 的 `INPUT` 结构在 x64 上 union 偏移是 8** → 用 `[StructLayout(LayoutKind.Explicit)]` + `Marshal.SizeOf` 传，别手写 40 字节。
8. **`Invoke-Wheel` 的 `wParam`** 只取高 16 位（app 侧 `delta = (wparam >> 16) as i16`），低 16 位填按键状态即可。
9. **热重载只认默认路径**（F3）→ 改 `--config` 指定文件不会触发重载，别写成「改了没反应」的 bug 报告。
10. **`state.json` 只在 `WM_EXITSIZEMOVE` 落盘** → 程序化 `SetWindowPos` 之后必须补发 `WM_EXITSIZEMOVE`，否则永远等不到 `已保存位置`。

---

## 8. 与门禁的接法（不改源码）

`scripts/gate.ps1` 保持原样（它要求「runnable anywhere」，GUI 用例需要交互式桌面会话）。新增两条：

```powershell
# 提交前（有人坐在机器前）：完整 6 轮
powershell -NoProfile -File scripts\fulltest\run-all.ps1 -Rounds 6
# 结构冒烟（可在无人值守会话跑）：A + B 两档，跳过 C
powershell -NoProfile -File scripts\fulltest\run-all.ps1 -Rounds 1 -SkipGui
```

建议把 `docs/aegis/reviews/fulltest-runs/*/report.md` 里 **flake=⚠ 的用例**设为「下一次评审必须先看」的输入——它比「这一轮过没过」信息量大得多。

---

## 9. 交付边界

- 本文与 `scripts/fulltest/**` 骨架**只读源码、不动源码**；所有断言都基于现有日志字符串、现有 CLI 与 Win32 回读，不要求新增埋点。
- 执行器自身的失败（找不到 `hwnd.txt`、`SendInput` 被 UIPI 挡住、前台拿不到）**必须与产品缺陷分开报**：前者 exit 2 / `BLOCKED`，后者 exit 1 / `FAIL`。混在一起会让这套东西两周后就没人信了。
