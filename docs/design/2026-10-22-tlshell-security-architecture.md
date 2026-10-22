# TLShell 安全架构评审

- **日期**：2026-10-22
- **评审对象**：[实现计划](../aegis/plans/2026-10-22-tlshell-desktop-terminal.md)（L1–L395）与 [UI 规范](2026-10-22-tlshell-ui-spec.md)（L1–L297）
- **代码基线**：无。仓库只有两份文档，全部结论基于文档推演
- **状态**：架构评审产出，待并入 M0/M2/M3/M5 验收
- **评审视角**：威胁建模 + 信任边界 + 最小权限

---

## 0. 结论摘要

### 0.1 一句话判断

**TLShell 没有权限边界，它是"用户令牌的可视化别名"。** 它起一个同用户、同完整性级别的交互式 shell，
所以在它内部做任何"权限控制"都没有意义；真正要做的是三件事：

1. **不给它新能力**（网络、IPC 动词、插件、自启动）；
2. **不让它丢失身份线索**（提权态、焦点态、进程存在感）；
3. **不让终端内容泄漏到盘 / 剪贴板 / 屏幕 / 崩溃转储**。

### 0.2 必须开工前定死的 5 条

| # | 问题 | 严重度 | 依据 |
|---|---|---|---|
| S-01 | `asInvoker` 会**继承父进程令牌**：从管理员终端启动 TLShell → 整个常驻终端提权，且无任何视觉提示 | 高 | 计划书 L253 的"管理员程序无影响"判断不成立 |
| S-02 | WorkerW 三级兜底**只按窗口类名查找，不校验属主进程**，可挂到伪造的 `WorkerW` 上 | 高 | L305 |
| S-03 | shell 用**相对文件名**启动 + **继承全部环境变量** → 当前目录劫持 + 密钥随环境泄进 shell | 高 | L127、L203 |
| S-04 | **无进程生命周期绑定**：TLShell 崩溃/被杀后 ConPTY 子 shell 可能残留为无窗口 shell | 中 | L192、L256 |
| S-05 | **OSC 52 / 剪贴板 / 粘贴完全没有安全策略**，而 M5 就要做复制 | 高 | L338（只提复制）、L340-② |

---

## 1. 系统安全画像

| 维度 | 事实 | 安全含义 |
|---|---|---|
| 权限等价 | 与用户令牌等价 | 攻陷 TLShell == 攻陷用户；不存在"终端内的沙箱" |
| 网络面 | 自身**零网络**（计划书未引入任何 HTTP/TLS 依赖） | 唯一可验证的强不变量，必须写进 CI |
| 远程输入面 | 无 | 无"未认证远程 RCE"路径，最高严重度止于本地同用户 |
| 主要攻击面 | VTE 解析器、fontdue、Win32 消息、config/state 解析 | 全部是**本地不可信数据**解析 |
| 可见性 | 无任务栏、无托盘、无 Alt+Tab、无标题栏、半透明、位于所有窗口之下 | **用户无法判断"这是什么进程"** —— 这本身是安全问题 |
| 持久性 | 桌面层常驻 + 常驻交互式 shell | 无人值守的持久 shell；屏幕共享/截图会带走内容 |
| 提权 | `asInvoker` | 继承父令牌，可能静默提权 |

---

## 2. 信任边界划分

边界定义原则：**跨越边界的数据一律视为不可信**，包括看起来"是系统发的"消息。

| ID | 边界 | 从 → 到 | 跨边界数据 | 主要威胁 | 必需控制 |
|---|---|---|---|---|---|
| **B1** | 输入 → UI | 用户 / IME → `win/window.rs` | 键盘、IME 组合串、鼠标、拖放 | 输入错投（焦点被抢后把口令打进 shell）、鼠标上报与拖动争用 | 焦点态可见（UI 规范 §4.2）、不抢焦点、拖放默认关闭 |
| **B2** | 窗口消息 → 核心 | Explorer / **任意同 IL 进程** → `app.rs` | `WM_*`、`WM_DROPFILES`、`WM_COPYDATA`、`TaskbarCreated` 广播 | 伪造广播触发重挂（重挂风暴）、跨进程消息夹带指令 | 任何窗口消息**不得携带命令**；广播类消息去抖 + 校验新父窗口 |
| **B3** | 核心 → PTY 写端 | `term/input.rs` → `portable-pty` | VT 字节、启动 argv、env、cwd | 命令注入（argv 拼接）、搜索序劫持、环境变量外泄、句柄泄漏 | argv 模板化、绝对路径、env 白名单、句柄白名单 |
| **B4** | PTY 读端 → 解析器 | 任意程序输出 → `vte::Parser` → `Term` | 不可信字节流（含 OSC/DCS/CSI） | 解析器 panic/DoS、OSC 52 剪贴板写入、OSC 8 链接、标题注入、视觉欺骗（RLO/零宽/`\r`） | 剪贴板回调默认拒绝、不实现链接、控制字符可视化、读线程 panic 隔离 |
| **B5** | 网格 → 屏幕 | `render/*` → `softbuffer` → DWM/桌面合成 | 终端内容像素 | 肩窥、截图/录屏/投屏外泄、无法排除捕获 | 不透明度下限、空闲消隐、文档化"桌面层内容可见" |
| **B6** | 核心 ↔ 存储 | `config/*` ↔ `%APPDATA%\TLShell\` | `config.toml`（**执行策略**）、`state.json`（几何） | config 被改写 = 任意程序执行；state 被改写 = 崩溃/越界/几何 DoS | config 视为代码级资产（ACL + 本地盘）、state 原子写 + 严格校验 |
| **B7** | 核心 ↔ 剪贴板 | `render/selection.rs` ↔ 会话剪贴板 | 终端文本 | 复制带出密钥（含剪贴板历史/云同步/RDP 重定向）、粘贴注入命令 | 复制时排除剪贴板历史与云同步；粘贴强制 bracketed paste |
| **B8** | 核心 ↔ 字体子系统 | `render/metrics.rs` ↔ 系统字体 | TTF/OTF 字节 | 字体解析器输入、缓存无界 | 只按字体族名解析、不接受路径、缓存上限 |
| **B9** | 核心 ↔ 单实例 IPC | 同用户任意进程 ↔ TLShell | 显示/聚焦/退出 三个无参动词 | 互斥体抢占（拒绝服务）、伪造目标窗口、抢焦点诱导误输入 | `Local\` + SID 命名 + 受限 DACL；目标窗口校验镜像路径；动词白名单 |
| **B10** | 核心 ↔ 网络 | — | **不存在** | 未来误加遥测/更新检查 | 依赖树断言：无 `reqwest`/`hyper`/`native-tls`/`rustls` |
| **B11** | 核心 → 子进程生命周期 | TLShell → ConPTY 子进程 | 存在性 | 崩溃后残留无窗口 shell | Job Object `KILL_ON_JOB_CLOSE` |

### 2.1 边界内的"内部"边界（容易被漏掉）

- **Explorer 是 TLShell 的父窗口宿主**。窗口挂进 WorkerW 后，Explorer 成为父窗口。Explorer 是 medium IL，
  因此它**不能**向高 IL 窗口发消息（UIPI 挡住），但**同 IL 时可以对我们的窗口做 `SetWindowPos`/`ShowWindow`**。
  结论：不要把"父窗口是 Explorer"当作信任依据，只当作布局依据。
- **屏幕是共享输出介质**。桌面层窗口对全屏捕获、投屏、录屏一律可见，且 `SetWindowDisplayAffinity`
  对子窗口无效。**这条与 R2 的"截屏验收"直接冲突，不能靠技术手段消除，只能靠产品约束**。

---

## 3. 进程与权限模型

### 3.1 目标模型

| 项 | 目标 | 说明 |
|---|---|---|
| 进程数 | 1 个 TLShell 进程 | 不拆服务、不拆 helper。拆进程只会增加 IPC 攻击面，不会带来隔离收益 |
| 线程 | UI 线程 + 1 个 PTY 读线程 + 1 个渲染节拍定时器 | 与计划书 L192 一致 |
| 完整性级别 | **medium，强制** | 见 S-01：启动时检测 `TokenElevation`，已提权则拒绝运行或显式降权提示 |
| 令牌特权 | 无 | 命名对象用 `Local\`，避免 `SeCreateGlobalPrivilege` |
| 子进程 | 同用户同 IL，独立 ConPTY | 不给 shell 加任何额外能力，也不试图沙箱化（终端沙箱化即失去意义） |
| 句柄继承 | 显式白名单 | 只允许 ConPTY 管道；其余句柄一律不可继承 |
| 环境变量 | 白名单构造 | 不继承父进程 env（见 S-03） |
| 工作目录 | 固定 `%USERPROFILE%` | 不用启动时的 cwd |
| 生命周期 | Job Object 绑定 | 见 S-04 |
| 网络 | 零 | 依赖树断言 + CI |
| 自启动 | v1 **不做** | 桌面层无 chrome 进程 + 自启动 = 现成的用户级持久化原语 |
| 服务/驱动/COM/计划任务 | 不做 | — |

### 3.2 为什么不做沙箱（明确的"不做"理由）

终端的产品价值就是"给用户一个能跑任意程序的 shell"。任何 AppContainer / 低 IL / Job 内存限制
都会直接破坏 `vim`、`wsl`、包管理器、`git` 的行为。**用沙箱换来的安全边际，远小于它破坏的可用性**。
因此本项目的安全投入必须全部落在"减少自身攻击面"和"减少内容外泄"上，而不是"限制 shell"。

### 3.3 提权策略（这是计划书需要改写的部分）

计划书 L253 写：

> 管理员程序：目标程序提权时 PTY 仍可工作，挂载不受影响 | 无影响（挂载的是自己的窗口）

这条在技术上不成立，需要拆成三条事实：

1. **ConPTY 子进程不能自行提权**。用户在终端里跑需要管理员权限的程序，会触发 UAC，
   该程序在新控制台里运行（或直接失败），**不会**在这个 PTY 里变成管理员。所谓"仍可工作"是误导。
2. **TLShell 自己会被动提权**。`asInvoker` 的语义是"用父进程的令牌"，所以从管理员
   PowerShell / 管理员启动器里拉起 TLShell，TLShell 就是 high IL，随后**它起的每一个 shell 都是管理员**。
3. **提权态没有任何视觉提示**。零 chrome + 无任务栏 + 无标题栏 ⇒ 用户失去"这个窗口是管理员的"这一
   唯一线索。这是本次评审里最危险的一处设计组合。

**要求**：启动时 `GetTokenInformation(TokenElevation)`；已提权则（a）拒绝启动并提示，或
（b）启动但显式渲染"管理员"角标 + 在日志与 `--status` 中标注。二选一，不能沉默。

---

## 4. 敏感数据流

| # | 数据 | 路径 | 风险 | 控制 |
|---|---|---|---|---|
| D1 | 键盘输入 / IME 组合串 | `WM_KEYDOWN`/`WM_CHAR`/`WM_IME_*` → `input.rs` → PTY 写端 | 焦点被抢后口令打进 shell；重复投递；代理对拆散 | 焦点态可见；`WM_KEYDOWN` 只管控制键、`WM_CHAR` 只管文本；代理对重组 |
| D2 | 启动 argv + 注入的 `-Command` | `config.toml` → argv 构造 → ConPTY | argv 拼接注入、`-Command` 吞掉后续参数 | 每种 shell 一个**封闭模板**，不允许自由拼接 |
| D3 | 环境变量 | 父进程 env → 子 shell | 父进程的 `AWS_*`/`GITHUB_TOKEN`/`*_KEY` 静默进入交互式 shell | env 白名单（见 5.3） |
| D4 | 终端输出字节 | PTY → `vte` → `Term` → 网格 | 解析器 panic、OSC 52、OSC 8、视觉欺骗 | 见 S-05、S-11 |
| D5 | 网格像素 | `painter` → `softbuffer` → DWM | 肩窥、截屏、投屏 | 不透明度下限 + 空闲消隐 + 文档化 |
| D6 | 选区文本 | `selection.rs` → 剪贴板 | 剪贴板历史/云同步/RDP 重定向外带密钥 | 复制时写 `ExcludeClipboardContentFromMonitorProcessing` + `CanIncludeInClipboardHistory=0` + `CanUploadToCloudClipboard=0` |
| D7 | 剪贴板内容 | 剪贴板 → PTY | 粘贴注入（多行自动执行、`\x1b` 控制序列、UNC 路径触发 SMB 认证） | bracketed paste 强制；多行/含控制字符时二次确认；UNC 路径只当文本 |
| D8 | `config.toml` | 磁盘 → 解析 → 执行 | 等价于代码：`program` + `args` + `[keys]` | 目录 ACL、拒绝漫游、变更检测与告警 |
| D9 | `state.json` | 磁盘 → 几何 | 越界/NaN/巨值 → 分配 DoS、坐标错乱、崩溃 | 原子写 + 严格校验 + 版本前向拒绝 |
| D10 | 日志 | `app.rs` → 磁盘 | 记录 shell 输出/命令行/路径 → 落盘泄漏；换行注入伪造日志 | 只记事件与错误码；不记 PTY 内容；字段转义 |
| D11 | 崩溃转储 | 进程内存 → WER | 转储含 scrollback 明文 | 关闭本地转储 / 文档化 WER 风险 |
| D12 | 进程内存 | — | 同 IL 进程可 `ReadProcessMemory`、可经 UIA 读窗口文本 | 明确"终端内容对同用户进程不保密"，不做假的"安全模式" |

---

## 5. 最小权限建议（可直接落成验收项）

### 5.1 启动与提权

1. manifest 显式声明 `requestedExecutionLevel level="asInvoker" uiAccess="false"`；
   **永不启用 `uiAccess="true"`**（那是绕过 UIPI + 抢前台的合法后门）。
2. 启动自检 `TokenElevation`；提权则拒绝或标注（§3.3）。
3. 拒绝从网络路径 / 可移动介质启动（`GetModuleFileName` + `GetDriveType`），避免"从共享目录运行的可执行文件"这一经典落地方式。

### 5.2 子进程

4. shell 一律解析为**绝对路径**（`%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`、
   `%ProgramFiles%\PowerShell\7\pwsh.exe`），不接受裸文件名。原因：`CreateProcessW` 在
   `lpApplicationName == NULL` 时**第二顺位搜索当前目录**，`program = "pwsh.exe"` + 攻击者可控 cwd = 直接执行攻击者二进制。
5. 若用户显式配置了自定义 `program` 路径，按"执行策略"对待：绝对路径 + 存在性校验 + 首次使用提示。
6. `cwd` 固定 `%USERPROFILE%`，与 TLShell 自身的启动目录解耦。
7. 子进程放入 Job Object，`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`，并禁止 breakaway。
8. 子进程创建使用句柄白名单（`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`），断言无额外可继承句柄。
9. 退出路径必须"先优雅关 PTY，再退进程"；崩溃路径靠 Job 兜底。

### 5.3 环境

10. env 白名单构造，只放：`SystemRoot`、`SystemDrive`、`windir`、`ComSpec`、`PATH`、`PATHEXT`、
    `TEMP`/`TMP`、`USERPROFILE`、`HOMEDRIVE`/`HOMEPATH`、`APPDATA`、`LOCALAPPDATA`、`USERNAME`、
    `USERDOMAIN`、`NUMBER_OF_PROCESSORS`、`PROCESSOR_ARCHITECTURE`、`LANG`、`PSModulePath`。
11. 额外透传走显式配置 `[shell] env_passthrough = ["FOO"]`，默认空。

### 5.4 存储

12. `config.toml` 放 `%LOCALAPPDATA%\TLShell\`（**不要** `%APPDATA%` 漫游）；`state.json` 同理。
    理由：`config.toml` 是执行策略文件，让它跟着漫游配置文件同步到文件服务器没有收益，只有暴露。
13. 目录创建后校验 DACL：仅当前用户 + SYSTEM + Administrators，无 `Users`/`Everyone` 写权限；
    发现被放宽则告警。
14. `state.json` 写入用"同目录临时文件 + `MoveFileEx(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`"，
    不就地覆写；打开时用 `FILE_FLAG_OPEN_REPARSE_POINT` 拒绝跟随符号链接/重解析点。
15. 读 `state.json` 失败/损坏/`version` 未知 → **回落到 config 默认值并保留坏文件副本**，不得崩溃。
16. 校验矩阵（缺一不可）：`dpi > 0`（否则缩放比 `cur/stored` 变 Inf/NaN）、`bounds` 非空且宽高 > 0、
    `width/height` 在 [最小尺寸, WorkerW 客户区]、`padding * 2 + min_cell < width/height`（**无符号减法下溢**）、
    `font.size > 0`、`line_height > 0`、`opacity ∈ [0.75, 1.0]`、`border_radius ≤ min(w,h)/2`。

### 5.5 剪贴板与输出

17. 实现 `alacritty_terminal` 的剪贴板回调时，**默认全部拒绝**（`clipboard_store` 空实现）；
    要支持 OSC 52 必须是显式配置 + 默认 `Disabled`。
18. 复制时附加三个剪贴板格式，阻断历史记录与云同步。
19. 不实现 OSC 8 链接、不做 URL/路径自动识别、不做"点击打开"。理由：`file://` 与 UNC 路径
    点击会触发 SMB 认证（NTLM 中继/哈希外带），而收益只是"少复制一次"。
20. 粘贴：目标程序启用 bracketed paste（mode 2004）时一律包裹；未启用且内容含 `\r`/`\n` 或 `\x1b` 时二次确认。

### 5.6 可观测性与供应链

21. 日志只记事件：启动（PID、镜像路径、是否提权）、WorkerW 挂载结果（候选 HWND + 属主镜像）、
    热键注册结果、配置校验失败项、IPC 动词。**不记 PTY 字节、不记命令行**。
22. 关闭 WER 本地转储，或至少在 README 声明"崩溃转储含终端明文"。
23. `Cargo.lock` 入库；CI 加 `cargo-deny`（advisories + bans + sources）+ `cargo-audit`；
    `build.rs` 保持零依赖、可人工通读（它决定 manifest 内容）。
24. CI 断言依赖树中不存在网络栈 crate（B10）。

---

## 6. 需要加固的设计点（按严重度）

### S-01 · 高 · 提权继承导致静默管理员终端

- **证据**：L253 把"管理员程序"判定为无影响；manifest 用 `asInvoker`（L140）。
- **攻击/误用路径**：用户在管理员 PowerShell 里 `.\TLShell.exe` → TLShell 以 high IL 常驻桌面层 →
  之后所有输入都在管理员上下文执行，且界面与普通终端**完全无法区分**。
- **修复**：§3.3 的二选一；并在 `--status` 与首次运行提示里显示完整性级别。

### S-02 · 高 · WorkerW 候选未校验属主

- **证据**：L305「三级兜底查找 WorkerW（DefView 兄弟 → Progman 子 WorkerW → 顶层 WorkerW）」。
- **问题**：三级兜底全部只按**窗口类名**筛选。类名是任何进程都能注册的。
  同 IL 进程可创建一个名为 `WorkerW` 的窗口，被我们 `SetParent` 成父窗口，从而获得对我们窗口的
  布局控制（移动/隐藏/尺寸），并可把它摆成钓鱼位置。
- **修复**：候选筛选加两条硬条件——`GetWindowThreadProcessId` + `QueryFullProcessImageNameW`
  必须以 `\explorer.exe` 结尾；且该窗口必须覆盖目标显示器工作区。任一不满足 → **降级为普通窗口**，
  不要"再试下一个候选直到成功"（三级兜底 + 强制成功 = 更容易被引到伪造窗口上）。

### S-03 · 高 · 启动路径与环境的最小权限缺失

- **证据**：L127 按**可执行文件名**判断并注入；L203 `program = "pwsh.exe"`；L192 未提 env；L305 未提 cwd。
- **问题**：① 相对文件名 + 当前目录搜索序 = 二进制种植；② 全量继承 env = 父进程密钥进 shell；
  ③ cwd 继承 = shell 起始目录可能是任意位置。
- **修复**：§5.2 第 4–6 条、§5.3 全部。

### S-04 · 中 · 崩溃后残留无窗口 shell

- **证据**：L192（读线程 + channel）、L256（退出方式）。
- **问题**：TLShell 被强杀或崩溃时，ConPTY 子进程可能存活；此时存在一个**没有窗口、没有任务栏、
  没有托盘**的交互式 shell 进程。这在 EDR 视角与"无窗口 shell 后门"同形，且用户无法回收。
- **修复**：Job Object + `KILL_ON_JOB_CLOSE`（§5.2 第 7 条）。注意 `portable-pty` 只暴露 PID，
  需 `OpenProcess` + `AssignProcessToJobObject`，存在极小竞态；若要消除竞态，需自建 ConPTY 并在
  `PROC_THREAD_ATTRIBUTE_JOB_LIST` 里指定 Job。

### S-05 · 高 · 剪贴板与粘贴没有安全策略

- **证据**：L338 只有"鼠标选中 + `Ctrl+C` 复制"，全文无 OSC 52、无粘贴、无剪贴板历史。
- **问题**：
  - ① `vte` 会把 OSC 52 解析出来，`alacritty_terminal` 通过 `EventListener` 回调交给宿主。
    若宿主实现了 `clipboard_store`，`cat` 一个恶意文件就能**静默替换用户剪贴板**（典型场景：
    把剪贴板里的钱包地址换成攻击者地址）。这是终端类软件最常见的一类静默攻击。
  - ② 复制出去的终端文本会进入 Win10 剪贴板历史 + 云同步 + RDP 剪贴板重定向。
  - ③ 粘贴缺失但用户一定会 `Ctrl+V`；未做 bracketed paste 的多行粘贴 = 直接执行。
- **修复**：§5.5 第 17–20 条。

### S-06 · 中 · 第二次启动抢焦点，可诱导把口令打进 shell

- **证据**：L193「第二次启动只发送『显示/聚焦』信号」。
- **问题**：`SetForegroundWindow` 把焦点从用户正在输入的窗口（可能是密码框）切到终端，
  后续按键进入 shell 命令行并被回显。这是**输入错投导致的凭据泄漏**，且零 chrome 让用户
  更难立刻意识到焦点已变。
- **修复**：禁止无条件 `SetForegroundWindow`；只做 `FlashWindowEx` + 显示，聚焦必须由用户点击触发。

### S-07 · 中 · 桌面层内容的机密性无法用技术手段保证

- **证据**：L251（只挂主屏）、UI 规范 §2.4（不透明度）、R2 用截屏验收。
- **问题**：桌面层窗口对全屏捕获、投屏、录屏一律可见；`SetWindowDisplayAffinity` 对子窗口无效，
  且与 R2 的截屏验收冲突。
- **修复**：不做技术对抗，做产品约束：① 不透明度下限 0.75（UI 规范已有，但计划书 L211 写的是
  `0.0–1.0`，**两处不一致必须统一**）；② 空闲 N 分钟消隐（可配，默认关）；③ 一键隐藏热键；
  ④ README 明确"TLShell 不保护你的会话，离开请 Win+L"。

### S-08 · 中 · `state.json` / `config.toml` 的健壮性与坐标空间

- **证据**：L229–L241、L316（"`GetWindowRect` → 转 WorkerW 相对坐标 → 写 state"）、L318（钳制）。
- **问题**：
  - ① **坐标空间未记录**。M2 把窗口从屏幕坐标改成 WorkerW 客户区坐标（L306 自己指出了这点），
    但 `state.json` schema 没有 `space` 字段。挂载失败降级为普通窗口时写入的是屏幕坐标，
    下次挂载成功读到的却是 WorkerW 坐标 → 位置错乱。
  - ② `dpi` 为 0 时缩放比变 Inf/NaN（L316"按 DPI 比例还原"）。
  - ③ 无原子写；断电/崩溃留下半截 JSON。
  - ④ `%APPDATA%` 漫游会把执行策略文件同步出去（L241）。
- **修复**：schema 加 `"space": "workerw" | "screen"`；校验矩阵见 §5.4 第 16 条；原子写；
  改 `%LOCALAPPDATA%`。

### S-09 · 中 · 热重载作用域未定义（含执行键）

- **证据**：L329「改 `background` 颜色立即反映（热重载）」；L338 热重载在 M5 打磨项里。
- **问题**：热重载若覆盖 `[shell]` 段，则"改配置 → 静默重启 shell"——既是会话丢失风险，
  也让"写一个 config 就执行代码"多了一个不需要重启的触发点。
- **修复**：定义**热重载矩阵**：可热更 = 颜色 / 字体 / 不透明度 / padding / 圆角；
  不可热更（需重启）= `[shell]` 全段、`[keys]`。热更必须原子（校验失败保留上一份 good config）。

### S-10 · 中 · 日志与崩溃转储的内容边界

- **证据**：L189（"降级为普通窗口并在日志告警"）、L256。
- **问题**：日志路径与内容都没定义。若日志顺手记了 shell 输出或 argv，等于把终端内容落盘；
  若日志拼进 config 里的 `program` 路径（攻击者可控），换行可伪造日志条目。
- **修复**：§5.6 第 21–22 条。

### S-11 · 中 · PTY 输出侧的三类风险未处理

- **证据**：L178（`vte::Parser` → `Term`）、L338（大输出节流）。
- **问题**：
  - ① **解析器 panic 未隔离**：读线程 panic → channel 断 → 终端静默死亡，无提示。
  - ② **channel 无界**：UI 慢时大输出把内存打爆（计划书风险 #5 只提 CPU，没提内存）。
  - ③ **视觉欺骗**：输出里的 `\r` 覆写、`U+202E`（RLO）、零宽字符可以让屏幕显示与实际内容不一致
    （"终端内容能骗人"是终端固有属性，但至少要**不主动放大**它）。
- **修复**：读线程 `catch_unwind` + 会话级错误态；channel 有界（或"停止读 = ConPTY 背压"策略，明确写死）；
  v1 不实现 bidi，控制/零宽字符按可见占位符渲染或丢弃。

### S-12 · 中 · 输入路径分工未定义（重复投递 / 代理对）

- **证据**：L294「`WM_KEYDOWN`/`WM_CHAR` → 字节写回 PTY」。
- **问题**：两个消息源同时投递可打印字符会**双写**；`WM_CHAR` 按 UTF-16 码元投递，
  非 BMP 字符（emoji、部分 CJK 扩展）会拆成两个消息，需重组；Alacritty 的 `input.rs` 输入是
  统一 `KeyEvent`（含 `text: Option<&str>`），移植时必须补这一层映射。
- **修复**：写死分工——文本只走 `WM_CHAR`（`TranslateMessage` 之后），控制序列只走 `WM_KEYDOWN`
  的非文本键；代理对重组 + 死键（`WM_DEADCHAR`）处理；补单测。

### S-13 · 中 · `Ctrl+C` 语义冲突

- **证据**：L338「鼠标选中 + `Ctrl+C` 复制」。
- **问题**：`Ctrl+C` 同时是 `0x03`（SIGINT）。若"有选区就复制"作为默认，用户忘记清选区时
  **中断信号静默失效**，正在跑的命令停不下来——这是可用性转安全的问题（比如中断一个误执行的破坏性命令）。
- **修复**：明确规则并写入 README：有选区 → 复制 + 首次触发时在 overlay 提示"再次 Ctrl+C 发送中断"；
  或默认"`Ctrl+Shift+C` 复制，`Ctrl+C` 永远中断"。二选一，必须写死。

### S-14 · 中 · `TaskbarCreated` 重挂可被伪造触发

- **证据**：L338「Explorer 重启后 `TaskbarCreated` 重挂」。
- **问题**：`TaskbarCreated` 是 `RegisterWindowMessage` 注册的广播消息，任何同 IL 进程都能
  `PostMessage(HWND_BROADCAST, msg, ...)` 触发我们重挂 → 反复触发即重挂风暴（CPU + 窗口闪烁）。
- **修复**：去抖（≥2s）、重挂前校验新 WorkerW 属主（同 S-02）、失败即停止重试并记日志。

### S-15 · 中 · 全局热键是可配置的全局输入面

- **证据**：L226 `[keys] quit = "Ctrl+Alt+Q"`；L283 `RegisterHotKey`。
- **问题**：① 配置驱动 → 若 config 被改写，可注册一个抢占用户常用组合的全局热键；
  ② 注册失败则**无退出路径**（UI 规范 G5 已识别，但计划书 L256 仍只写热键 + `--quit`）；
  ③ 全局热键在任意前台程序下生效，误触即杀会话（丢 vim 未保存内容）。
- **修复**：热键限定为 `Ctrl+Alt+<字母>` 白名单；注册失败 → hover `×` 必须存在（UI 规范 §4.4 已有）；
  quit 需二次确认 overlay；`--quit` 走优雅关闭。

### S-16 · 低/中 · 供应链与 vendored 代码

- **证据**：L76–L81（`alacritty_terminal` 0.26、`portable-pty` 0.9、`windows` 0.62 等）、
  L77（移植 `input.rs`）、L140（`build.rs` 嵌 manifest）。
- **问题**：0.x 依赖 + 大体积 `windows` crate + vendored 源码（与上游漂移后无法接收上游修复）
  + `build.rs` 在构建期执行。
- **修复**：`Cargo.lock` 入库；`cargo-deny` + `cargo-audit`；vendored 文件加"上游 commit + 同步日期"头注释；
  `build.rs` 零依赖可审阅。

### S-17 · 低 · 单实例机制的健壮性

- **证据**：L193、L256（`--quit`）。
- **问题**：互斥体可被抢占（→ 启动即"已有实例"，然后给一个不存在的实例发信号并退出 = 拒绝服务）；
  `--quit` 让任意同用户进程可终止会话（丢工作）。
- **修复**：`Local\TLShell.{SID}` 命名 + 受限 DACL；目标窗口校验镜像路径 == 自身；
  IPC 动词白名单（show / flash / quit），**永不接受带参动词**；`--quit` 文档化为"非安全边界"。

---

## 7. 计划书中存在歧义的设计点

以下不是"漏洞"，而是**必须在动工前写死的决策**，否则实现者会各自发挥。

| # | 歧义 | 现状 | 要求 |
|---|---|---|---|
| A-1 | 单实例 IPC 用什么机制 | L193 只说"发送显示/聚焦信号" | 指定消息窗口 + 自定义消息 or 命名管道；写清鉴权与动词表 |
| A-2 | `state.json` 坐标空间 | L306 承认坐标系变了，schema 无字段 | 加 `space` 字段（S-08） |
| A-3 | 热重载覆盖哪些键 | L329/L338 | 热重载矩阵（S-09） |
| A-4 | `[shell] args` 与注入 `-Command` 的顺序/引用 | L123/L127/L204 | 每 shell 一个封闭 argv 模板；禁止预拼命令行字符串 |
| A-5 | `WM_KEYDOWN` vs `WM_CHAR` 分工 | L294 | 写死分工 + 代理对/死键（S-12） |
| A-6 | 粘贴完全不在范围内 | 全文无粘贴 | 要么明确"v1 不做粘贴"，要么按 §5.5 做 |
| A-7 | `Ctrl+C` 语义 | L338 | 二选一写死（S-13） |
| A-8 | 鼠标上报与"顶部 8px 拖动"冲突 | L305 + UI 规范 §4.3 | 程序启用鼠标上报时禁用拖动/缩放命中区，或要求修饰键 |
| A-9 | 崩溃/异常退出时子 shell 去向 | L192/L256 | Job Object 策略（S-04） |
| A-10 | 日志路径与内容边界 | L189 | §5.6 第 21 条 |
| A-11 | `opacity` 区间 | L211 `0.0–1.0` vs UI 规范 §2.4 `[0.75,1.0]` | 统一为 `[0.75,1.0]`，并解释原因 |
| A-12 | "未决项：无" | L52 声称无未决项，但 UI 规范 §8 有 4 项待确认 | 计划书 §2 应改为"未决项见 UI 规范 §8" |
| A-13 | "管理员程序无影响" | L253 | 按 §3.3 改写 |
| A-14 | `--quit` / `--reset-position` 的调用者假设 | L256 + UI 规范 §4.4 | 声明"同用户任意进程可调用，非安全边界" |
| A-15 | 是否接受拖放 | 全文未提 | 显式列入"不做"；若做，需路径引用 + UNC 处理 |
| A-16 | OSC 8 / URL 点击 / 路径识别 | 全文未提 | 显式列入"不做" |
| A-17 | 多屏时"钳制回可见区"以哪块屏为基准 | L318 + L251 | 以"最近一次所在显示器"为基准，找不到则主屏居中 |
| A-18 | `[keys]` 可配置范围 | L226 | 白名单（S-15） |

---

## 8. 建议并入各里程碑的验收动作

| 里程碑 | 追加验收 |
|---|---|
| M0 | 从管理员 shell 启动 → 观察是否提权并给出明确提示；manifest 断言 `asInvoker` + `uiAccess="false"` |
| M1 | 单测：argv 模板（三 shell × 有/无自定义 args）；单测：env 白名单；单测：`WM_CHAR` 代理对重组；读线程 panic 注入后会话进入错误态而非进程退出 |
| M2 | 伪造 `WorkerW` 类名窗口存在时，**不得**挂载（降级为普通窗口）；`TaskbarCreated` 连续广播 10 次 → 不出现重挂风暴 |
| M3 | `state.json` 写 `dpi:0` / `bounds:[]` / `version:99` / 半截 JSON → 全部回落默认值且不崩溃；断电模拟（写入中途 kill）后文件仍可解析 |
| M4 | `opacity=0.5` 被钳制到 0.75；空闲消隐（若实现）生效 |
| M5 | `printf '\e]52;c;%s\a' "$(echo aGVsbG8= | base64)"` → **剪贴板内容不变**；粘贴含 `\r` 的多行文本 → 出现确认；`Ctrl+C` 语义符合 §7 A-7 的决策 |

---

## 9. 明确不做（安全相关的"不做"清单）

与计划书 §1.2 并列，这些是**安全决策**而非范围裁剪：

1. 不做遥测、崩溃上报、更新检查（B10）。
2. 不做自启动 / 服务 / 计划任务 / COM 注册（持久化原语）。
3. 不做 OSC 8 链接、不做 URL 自动识别与点击打开（SMB 认证外带）。
4. 不做全局键盘捕获（`WH_KEYBOARD_LL` / `GetAsyncKeyState` 轮询）——桌面层终端 + 全局键盘捕获
   在行为上与键盘记录器不可区分，也会被 AV/EDR 直接标红。
5. 不做插件 / 脚本扩展 / 配置里的任意"钩子"。
6. 不做"安全模式"话术——终端内容对同用户进程不保密，不要给用户假的保证。

---

## 10. 待确认

1. **提权策略二选一**：拒绝启动，还是启动 + 管理员角标？（§3.3）
2. **`Ctrl+C` 语义二选一**：有选区复制 + 二次提示，还是 `Ctrl+Shift+C` 复制、`Ctrl+C` 恒为中断？（S-13）
3. **`config.toml` 是否接受迁到 `%LOCALAPPDATA%`**（放弃漫游，换取执行策略文件不外泄）？（S-08）
4. **v1 是否做粘贴**：做则必须带 bracketed paste 与确认；不做则需在 README 说明用户会撞上 `Ctrl+V` 无反应。（A-6）
