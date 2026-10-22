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
.\tlshell.exe                 # 启动
.\tlshell.exe --quit          # 让已有实例退出
.\tlshell.exe --selftest      # 无界面自检，写 JSON 报告
```

## 交互

| 操作 | 行为 |
|---|---|
| 拖动内边距 | 移动窗口 |
| 拖四边/四角 | 缩放（最小 480×240 逻辑像素） |
| 滚轮 | 回看历史 3 行/格；打字自动回到底部 |
| 拖选 | 选中文本，`Ctrl+C` 复制（无选区时 `Ctrl+C` 仍是中断） |
| `Ctrl+V` | 粘贴。剥离 C0/C1 控制字符；含多行时底部提示条要求 3 秒内再按一次确认；终端支持时用 bracketed paste 包裹 |
| `Ctrl+Alt+Q` | 退出 |
| `Ctrl+Alt+H` | 隐藏 |

底部提示条（一行，3 秒后消失，启动告警留 30 秒）用于：粘贴确认、剪贴板失败、
热键注册失败、以管理员权限运行。

## 已知限制

这些是**设计取舍**，不是缺陷：

1. **Win+D 后焦点丢失**：需要点击终端才能继续输入。系统行为，无法绕过（Rainmeter 的输入框同样如此）。
2. **点击终端会激活它**：这是能打字的必要条件。TLShell 不加 `WS_EX_NOACTIVATE`。
3. **只有 alpha，没有模糊**：嵌入 WorkerW 后 DWM 的背景特效失效，亚克力/Mica 不可用。
   整窗 alpha 是「文字和背景一起变透」，`opacity` 调到 0.5 时正文会明显发灰。
   要做「背景透、文字不透」必须逐像素 alpha（`UpdateLayeredWindow`），
   那会推翻 `softbuffer` 这条选型，不在计划书范围内。
4. **圆角是硬边**：圆角由 `SetWindowRgn` 做区域裁剪实现，边缘没有抗锯齿。
5. **无托盘、无关闭按钮**：退出靠全局热键 `Ctrl+Alt+Q`，或 `tlshell.exe --quit`。
6. **只挂主屏**：多显示器场景下只在主屏的 WorkerW 上出现。
7. **全屏游戏会遮住它**：符合预期。
8. **Emoji 不保证显示**：字形回退链只覆盖拉丁与 CJK。
9. **壁纸程序可能盖住它**：TLShell 挂在 `WorkerW` 层里，与 Wallpaper Engine 这类
   动态壁纸是同一层的兄弟窗口。TLShell 会把自己提到最前，但壁纸程序若持续抢占
   层级，仍可能被覆盖。
10. **桌面图标会画在它上面**：`SHELLDLL_DefView` 与 `WorkerW` 是 `Progman` 的兄弟，
   图标层始终在上。这是“成为壁纸”的固有代价。
11. **不支持拖放文件**：不注册 `WS_EX_ACCEPTFILES`，也不处理 `WM_DROPFILES`。
12. **输入法只做定位与上屏**：`WM_IME_STARTCOMPOSITION` 时把候选窗口挪到光标位置，
    `WM_IME_CHAR` 直接写入 PTY；候选框样式与行内预编辑显示交给系统 IME。

## 安全边界

- 进程清单声明 `asInvoker`，不请求提权；从管理员 shell 启动时会继承该完整性级别。
- shell 可执行文件按白名单解析为**绝对路径**，不做当前目录回退。
- OSC 52 剪贴板写入默认关闭；粘贴会剥离 C0/C1 控制字符。
- 日志落 `%LOCALAPPDATA%\TLShell\logs\`，默认 `warn` 级别，**不记录** PTY 字节流、键盘输入与剪贴板内容。
- 子进程放进 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 作业对象：TLShell 退出时 shell 一并结束，不留孤儿进程。
- 单实例：命名互斥体判重，第二个实例用 `%APPDATA%\TLShell\hwnd.txt` 记的句柄唤醒已有窗口后退出。
- WorkerW 候选在按类名找到之后，还要用 `GetWindowThreadProcessId` + `QueryFullProcessImageNameW`
  确认宿主进程是 `explorer.exe`；同完整性级别的进程注册同名类窗口会被拒绝。
- 提权状态自检：`TokenElevation` 为真时在底部提示条明示「以管理员权限运行」，
  因为无边框窗口没有任何其它的管理员线索。
- 依赖许可证与安全公告由 `deny.toml` + CI 的 `cargo deny check` 把关。

## 文档

- 实现计划：`docs/aegis/plans/2026-10-22-tlshell-desktop-terminal.md`
- UI 规范：`docs/design/2026-10-22-tlshell-ui-spec.md`
- 安全架构：`docs/design/2026-10-22-tlshell-security-architecture.md`
- 视觉验收：`docs/aegis/plans/2026-10-22-tlshell-desktop-terminal-visual-acceptance.md`
- 构建与 CI：`docs/aegis/plans/2026-10-22-tlshell-build-ci-toolchain.md`

## 视觉验收

43 条视觉验收项（`V-01`…`V-43`）的逐条状态与截图证据见
[`docs/aegis/evidence/vis/M5/index.md`](docs/aegis/evidence/vis/M5/index.md)。
其中 `V-19`（圆角抗锯齿）、`V-30`（退出码状态行）、`V-32`（错误显示到界面）
以及全部对比度/录屏类条目**未通过或未验证**，已在文档中逐条标注。

## 许可

MIT OR Apache-2.0。移植自 Alacritty 的键盘编码逻辑保留其 Apache-2.0 版权头（见 `NOTICE`）。
