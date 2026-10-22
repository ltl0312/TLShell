# TLShell

**挂在 Windows 桌面层（WorkerW）的常驻终端。**

TLShell 是一个 Rust 写的终端模拟器，它的窗口不是普通顶层窗口，而是 `SetParent` 到桌面的
`WorkerW` 上——因此按 `Win+D` 回到桌面时，终端依然在桌面上可见。

终端内核复用 Alacritty 的 `alacritty_terminal`（网格 + VT 状态机）与 WezTerm 的
`portable-pty`（ConPTY），TLShell 自己只负责三件事：**桌面层挂载、位置持久化、玻璃外观**。

## 状态

| 里程碑 | 内容 | 状态 |
|---|---|---|
| M0 | 骨架与窗口（消息循环、样式、全局热键） | 完成 |
| M1 | PTY 与渲染（ConPTY、VT 状态机、字形绘制） | 完成 |
| M2 | 桌面层挂载（WorkerW 父窗口、拖动与缩放） | 完成 |
| M3 | 配置与位置持久化（config.toml、state.json） | 完成 |
| M4 | 玻璃外观（圆角裁剪、透明度、配置热重载） | 完成 |
| M5 | 打磨与稳定性（滚动回看、选中复制、IME、单实例） | 完成 |

## 构建

```powershell
cargo build --release
```

产物：`target\release\tlshell.exe`。

工具链版本由 `rust-toolchain.toml` 锁定（Rust 1.97.1 + `x86_64-pc-windows-msvc`）。

## 运行

```powershell
.	lshell.exe                          # 启动
.	lshell.exe --quit                   # 让已有实例退出（真的退出，不是空操作）
.	lshell.exe --restart-unelevated     # 以管理员运行时，经 explorer 降权重启
.	lshell.exe --reset-position         # 忘掉记住的位置，回到默认 40,40
.	lshell.exe --config PATH            # 用指定配置文件（保存也写回这份）
.	lshell.exe --selftest --report PATH # 无界面自检，写 JSON 报告
.	lshell.exe --probe-input            # 合成消息自检：点击/拖选/ASCII/CJK/emoji
.	lshell.exe --probe-real             # 真实 SendInput 自检（短暂抬到最前，结束归还前台）
.	lshell.exe --dump-frame PATH        # 首帧落盘为 BMP，用于像素级验收
.	lshell.exe --dump-bg RRGGBB         # 落盘时的合成底色（默认白，用于模拟浅色壁纸）
```

`--probe-input` / `--dump-frame` 只向自己的窗口 `PostMessage`，不注入系统级输入。
`--probe-real` 是唯一会动真实鼠标键盘的开关，仅在显式指定时启用，且每个发送函数都先确认
前台仍是自己，否则一个事件都不发。

## 交互

| 操作 | 行为 |
|---|---|
| 拖动内边距 | 移动窗口 |
| 拖四边/四角 | 缩放（最小 480×240 逻辑像素） |
| 滚轮 | 回看历史 3 行/格；打字自动回到底部 |
| 拖选 | 选中文本，`Ctrl+C` 复制（无选区时 `Ctrl+C` 仍是中断） |
| `Ctrl+V` | 粘贴。剥离 C0/C1 控制字符；含多行时底部提示条要求 3 秒内再按一次确认；终端支持时用 bracketed paste 包裹 |
| `F2` 或 `Ctrl+,` | 打开设置面板（↑↓ 选择、←→ 调整、`Enter` 保存、`Esc` 取消） |
| `Ctrl+Alt+T` | 把窗口唤到前台并取键盘焦点（沉底模式下用它叫回来） |
| `Ctrl+Alt+Q` | 退出 |
| `Ctrl+Alt+H` | 隐藏 |

底部提示条（一行，3 秒后消失，启动告警留 30 秒）用于：粘贴确认、剪贴板失败、
热键注册失败、以管理员权限运行。

**提权态有常驻标记**：进程以管理员权限运行时，边框内侧会一直有一圈两像素的黄色警示线
（不随提示条消失）。无边框窗口没有标题栏可以提示完整性级别，只靠 30 秒提示条不够。

## 已知限制

这些是**设计取舍**，不是缺陷：

1. **Win+D 后焦点丢失**：需要点击终端才能继续输入。系统行为，无法绕过（Rainmeter 的输入框同样如此）。
2. **点击终端会激活它**：这是能打字的必要条件。TLShell 不加 `WS_EX_NOACTIVATE`。
3. **只有整窗 alpha，没有模糊**：无边框分层窗口在 `UpdateLayeredWindow` 下不享受 DWM 的
   亚克力/Mica 背景特效。整窗 alpha 是「文字和背景一起变透」，`opacity` 调到 `0.70` 以下
   正文对比度会跌破 4.5:1（`OPACITY_WARN_BELOW` 会在启动时告警，但不阻止）。
   要做「背景透、文字不透」需要把文字层单独合成，属于另一条渲染路线。
4. **沉底是可选项，默认关闭**：窗口是**顶层窗口**（`parent = NULL`）。Win32 键盘焦点属于线程
   输入队列，`SetFocus` 只能设给同队列的窗口；把窗口 `SetParent` 到 `WorkerW` 会变成 explorer
   队列的子窗口，键盘**永远到不了**它 —— 所以「挂进桌面」这条路已删除。
   `window.sink_when_unfocused = true` 时才在失焦后沉到 `HWND_BOTTOM`：那时窗口会跑到所有
   最大化窗口下面（点不到，只能靠 `Ctrl+Alt+T` 叫回来）；默认 `false` 就是普通 z-order，
   点得到、打得进。设置面板最后一项就是这个开关，改完立即生效。
   启动时会先 `HWND_TOPMOST` 抬到最前并取焦点 —— 后台进程抢前台会被 Windows 的前台锁
   拒绝（实测 `focus_set=true` 而 `foreground=false`），这时保持 topmost 让窗口看得见、
   点得到，等用户点一下触发 `WM_ACTIVATE` 后再自动退回普通层。
5. **无托盘、无关闭按钮**：退出靠全局热键 `Ctrl+Alt+Q`，或 `tlshell.exe --quit`。
6. **只用一个显示器**：位置按启动时所在显示器的工作区钳制，不跟随跨屏拖动重新协商。
7. **全屏游戏会遮住它**：符合预期。
8. **Emoji 不保证显示**：字形回退链只覆盖拉丁与 CJK。
9. **壁纸程序可能盖住它**：与 Wallpaper Engine 这类程序同处最底层，谁最后沉底谁在下面。
   可见性看门狗每 2 秒检查一次（3 次冷却 3 秒的恢复上限），被持续抢占时只能靠点击唤回。
10. **桌面图标会画在它上面**：图标层由 explorer 绘制，位置固定在最上层。
11. **不支持拖放文件**：不注册 `WS_EX_ACCEPTFILES`，也不处理 `WM_DROPFILES`。
12. **输入法只做定位与上屏**：`WM_IME_STARTCOMPOSITION` 时把候选窗口挪到光标位置，
    `WM_IME_CHAR` 按 UTF-16 代理对拼成完整字符（含非 BMP）后写入 PTY；
    候选框样式与行内预编辑显示交给系统 IME。

## 安全边界

- 进程清单声明 `asInvoker`，不请求提权；从管理员 shell 启动时会继承该完整性级别。
- shell 可执行文件按白名单解析为**绝对路径**，不做当前目录回退。
- OSC 52 剪贴板写入默认关闭；粘贴会剥离 C0/C1 控制字符。
- 日志落 `%LOCALAPPDATA%\TLShell\logs\`，默认 `warn` 级别，**不记录** PTY 字节流、键盘输入与剪贴板内容。
- 子进程放进 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 作业对象：TLShell 退出时 shell 一并结束，不留孤儿进程。
- 子进程环境变量走**白名单**（`shell::ENV_ALLOWLIST`，24 项），`ComSpec` / `PATH` / `PATHEXT` /
  `PSModulePath` 四个由程序自算，云凭据、令牌、`*_proxy`、`NODE_OPTIONS`、`__COMPAT_LAYER` 等
  注入面一律不继承；启动目录取 `USERPROFILE` 并校验绝对路径、非 UNC、存在、非重解析点。
- 单实例：命名互斥体判重，第二个实例读 `%APPDATA%\TLShell\hwnd.txt` 的句柄前，先用
  `GetWindowThreadProcessId` + `QueryFullProcessImageNameW` 确认该窗口属于本程序，
  只发一条自定义唤醒消息，不越过进程边界去改别人的窗口状态。
- 提权状态自检：`TokenElevation` 为真时在底部提示条明示「以管理员权限运行」，
  因为无边框窗口没有任何其它的管理员线索。
- 依赖许可证与安全公告由 `deny.toml` + CI 的 `cargo deny check` 把关。

## 测试

```powershell
pwsh -NoProfile -File .\scriptsun-all.ps1 -Rounds 6   # 四层 36 项/轮
pwsh -NoProfile -File .\scripts\gate.ps1              # 提交门禁（含 cargo deny）
```

四层：静态门禁（fmt / clippy / test）→ 无界面自检 → 真机功能探针 → 行为负例。
退出码 `0` 全通过、`1` 有 FAIL、`2` 有 BLOCKED（环境缺失不算通过）。
真机探针只向自己的窗口投递消息，结束时把前台还给原窗口，不注入系统级输入。

另有一层**默认关闭**的真实输入层（会移动光标、点击自己的窗口、临时抢前台）：

```powershell
pwsh -NoProfile -File .\scriptsun-all.ps1 -Rounds 1 -SkipBuild -RealInput   # 加 5 项，共 38 项
```

它用真实 `SendInput` 走完整链路（点击 → 取焦点 → 拖选 → 按键 → 写 PTY → 归还前台），
点击前先确认该坐标上的窗口就是自己，否则一个事件都不发。

六轮不是同一套参数连跑：每轮用 `%TEMP%egis-round-N\config.toml` 换一组不透明度/内边距/
圆角/字号（`opacity=0.80+0.02N` 等），所以六轮同时验证「参数变化不影响链路」。
结果与逐项断言见 [`docs/aegis/evidence/test/M6/index.md`](docs/aegis/evidence/test/M6/index.md)，
视觉证据见 [`docs/aegis/evidence/vis/M7/index.md`](docs/aegis/evidence/vis/M7/index.md)。

## 文档

- 实现计划：`docs/aegis/plans/2026-10-22-tlshell-desktop-terminal.md`
- UI 规范：`docs/design/2026-10-22-tlshell-ui-spec.md`
- 安全架构：`docs/design/2026-10-22-tlshell-security-architecture.md`
- 视觉验收：`docs/aegis/plans/2026-10-22-tlshell-desktop-terminal-visual-acceptance.md`
- 构建与 CI：`docs/aegis/plans/2026-10-22-tlshell-build-ci-toolchain.md`

## 视觉验收

43 条视觉验收项（`V-01`…`V-43`）的逐条状态与截图证据见
[`docs/aegis/evidence/vis/M5/index.md`](docs/aegis/evidence/vis/M5/index.md)。

M6 起渲染路线换成 `CreateDIBSection` + `UpdateLayeredWindow(ULW_ALPHA)`，因此：

- `V-19` 圆角抗锯齿 **通过**：角部 4×4 超采样，`surface.rs` 有测试断言存在 `0 < coverage < 1`
  的过渡像素（旧的 `SetWindowRgn` 硬边路径已删除）。
- `V-02`（文字不被整窗 alpha 稀释）、`V-38`（白底选区）**通过**，证据见下方「颜色实测」。
- `V-30`（退出码状态行）、`V-32`（错误显示到界面）**通过**：状态栏右侧显示退出码，
  启动失败与 shell 解析失败会写到界面上，不只剩日志。
- 仍未验证：需要白壁纸实采样与录屏的对比度/动效类条目，文档里逐条标注为未验。

## 许可

MIT OR Apache-2.0。移植自 Alacritty 的键盘编码逻辑保留其 Apache-2.0 版权头（见 `NOTICE`）。
