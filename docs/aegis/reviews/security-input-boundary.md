# 输入边界与进程启动安全评审（S-02 / S-03）

| 项 | 值 |
|---|---|
| 评审对象 | `b699ea41e9a00676b42ebabc10dd952543944433`（2026-10-22 15:58），工作区有 1 处未提交改动 `src/app.rs` |
| 评审范围 | `src/win/workerw.rs`、`src/term/shell.rs`、`src/term/session.rs`、`src/term/paste.rs`、`src/term/input.rs`、`src/win/elevation.rs`、`src/app.rs`（消息分发 / 重挂 / SpawnOptions）、`docs/design/2026-10-22-tlshell-security-architecture.md`（§0.2、§1、§4 D 表、§5.4、§6 S 表、§7.2、§8） |
| 评审类型 | 只读设计评审（威胁建模 + 代码证据）。**未修改任何源码** |
| 关联 S 项 | S-02（WorkerW 属主校验）、S-03（绝对路径 / env / cwd）、S-14（TaskbarCreated 重挂） |
| 关联 D 项 | D5（`SetWindowDisplayAffinity` 对子窗口无效）、D13（env 全量继承） |
| 结论 | Q1、Q3 判定为**架构级取舍**（不安全但可解释）；Q2 判定为**必须落地的实现缺口**，且当前实现存在 2 个高危具体缺陷（F-05、F-08）不依赖方案选择 |

---

## 0. 结论摘要

| 问题 | 判定 | 严重度 | 主要 CWE | 建议 |
|---|---|---|---|---|
| Q1 WorkerW 跨进程父链 | 「输入不可信」并非由父链**引入**，而是由父链**放大**：父链把 HWND 从"可枚举猜测"变成"`EnumChildWindows` 确定性锚定"，并把窗口生命周期/位置/使能/坐标交给 explorer。`is_desktop_owner` 只比 basename（`workerw.rs:85`），同 IL 任意进程复制 `explorer.exe` 即可冒充宿主 | 中（现状）/ 高（可被利用冒充时） | CWE-290、CWE-345、CWE-940、CWE-668、CWE-653、CWE-1021、CWE-367、CWE-672、CWE-682、CWE-451 | 先补齐 S-02 三项（全路径 + 工作区覆盖 + 挂载后回读），再评估是否保留该方案 |
| Q1-b AttachThreadInput | **禁止引入**。共享输入队列会让 `GetAsyncKeyState`（`app.rs:29`）读到的修饰键态被对方污染/被对方观测，并让 `SendInput` 误投到 explorer 窗口 | 高（若引入） | CWE-653、CWE-441、CWE-362 | 焦点问题用 `SetFocus` + 自绘 focus ring 解决，不回退到 ATI |
| Q2 S-03 env 白名单 | 可定义为 **A 层 24 项 + B 层重建 3 项 + 一律拒绝的黑名单**；`PATH` / `PATHEXT` / `PSModulePath` / `ComSpec` 必须**自算**而非继承 | 高 | CWE-526、CWE-15、CWE-427、CWE-426 | 落地见 §3；`env_clear()` 是前提，当前 `session.rs:112-118` 只有 `TERM` |
| Q2-b 固定 cwd | 固定 `%USERPROFILE%`（canonicalize + 拒 UNC + 拒重解析点）；**`USERPROFILE` 缺失时必须拒绝启动**，不得回退继承。当前 `app.rs:325` 返回 `None` → 继承 TLShell 自身 cwd | 高 | CWE-426、CWE-73、CWE-59 | 见 §3.5 |
| Q3 顶层沉底 | 信任边界**严格变好**：去掉整条跨进程父链（CWE-668/653 消失）、`SetWindowDisplayAffinity` 重新可用（升级 S-07）、前景判定诚实、DPI 自洽。代价是**丢失 Win+D 免疫**（原功能回退）与需要自管 z-order | — | 消除 CWE-668/653/682；CWE-290 不变 | 推荐作为 S-02 的替代路径，但需先解决 Win+D |

---

## 1. 评审对象与证据基线

已读并交叉核对的文件与关键位置：

| 文件 | 行 | 事实 |
|---|---|---|
| `src/win/workerw.rs` | 47 | `owner_process_name`：`GetWindowThreadProcessId` + `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` + `QueryFullProcessImageNameW`，取 basename 小写；失败 `None` |
| | 85 | `is_desktop_owner`：`name == "explorer.exe"` 即信任；**不比较完整路径、不验签名、不比较会话/IL** |
| | 106 | `find_workerw`：三级兜底（`progman-child` / `progman-child-first` / `top-level-workerw`），只按类名筛选，**未校验工作区覆盖** |
| | 169 | `SetParent(hwnd, Some(workerw))` 只判 `Ok/Err`，**不判结果是否真的挂上** |
| | 179-188 | 父为 `None` 时改 `GWL_STYLE` 为 `WS_CHILD`，再 `SetParent`，**返回值被 `let _` 丢弃** |
| | 196-206 | `SetWindowPos(hwnd, HWND_TOP, …, SWP_NOMOVE\|SWP_NOSIZE\|SWP_NOACTIVATE)`，故意带 z-order |
| | 219 | **无条件**返回 `Attach::Attached{…}` |
| | 26 | `Attach` 只有 `Attached` / `Fallback`，缺「样式已改但未挂上」第三态 |
| `src/app.rs` | 29 | `GetAsyncKeyState` / `SetCapture` / `ReleaseCapture` 用于修饰键与拖拽 |
| | 246 | `RegisterWindowMessageW(w!("TaskbarCreated"))`，失败仅 warn |
| | 636 | `reattach` 直接复用 `workerw::attach`，无去抖、无新父校验 |
| | 1313-1357 | `wndproc` → `catch_unwind` → `dispatch`；仅特判 `WM_NCCREATE`，**全链路无消息来源校验** |
| | 325 | `cwd: std::env::var_os("USERPROFILE").map(PathBuf::from)` → 缺失即 `None` |
| `src/term/session.rs` | 112-118 | `CommandBuilder::new(program)` + `env("TERM", …)`，**无 `env_clear()`** → 全量继承 |
| `src/term/input.rs` | 4 | 头注释自述「只从 `WM_KEYDOWN` / `WM_CHAR` 取值，没有 key-up」 |
| `src/win/elevation.rs` | 20 | `TokenElevation` 查询，失败返回 `Err`（注释 :19 明确「绝不把查不到当成已提权」） |
| `src/term/paste.rs` | 18-55 | `sanitize` 归一 CRLF、丢 C0/C1；`needs_confirm` >1 行；`bytes_for_pty` 加 `ESC[200~…ESC[201~` |
| `src/term/shell.rs` | 38-101 | 白名单目录 + `canonicalize` + `is_allowed_path` 复核；裸名只在白名单内搜索 |
| | 146 | `inject_args`：PS5 补 `-NoLogo -NoExit -Command <UTF8 setup>`；cmd 补 `/K "chcp 65001 >nul"`；**未补 `/D`** |

`elevation.rs` 与 `paste.rs` 未发现缺陷，本报告的发现项集中在 Q1/Q2 路径上（`shell.rs` 仅 1 项 F-08）。

---

## 2. Q1：把窗口 `SetParent` 进 explorer 的 WorkerW，输入与消息可信度问题

### 2.1 信任关系被改写的地方

`SetParent` 成功后，TLShell 的窗口从「`WS_POPUP` 顶层窗口」变成「explorer 进程 Explorer.exe 的 `WorkerW` 子窗口」。这一变化同时改写四件事：

| 维度 | 顶层窗口（现状降级路径） | WorkerW 子窗口 |
|---|---|---|
| 窗口归属 | 本进程线程，独立输入队列 | 跨进程父子链（父子不同线程、不同进程） |
| 输入队列 | 独立 `Thread Input Queue` | 仍独立，但**父链参与消息路由**（`WM_PARENTNOTIFY` 发给父窗口） |
| 生命周期 | 由本进程 `WM_CLOSE`/`WM_DESTROY` 决定 | 父窗口可 `DestroyWindow` 直接销毁子窗口，不经过我们的关闭协商 |
| 前台判定 | 参与 Alt+Tab / 任务栏 / `GetForegroundWindow` | 不参与（子窗口无 `WS_POPUP`、无激活） |

### 2.2 消息来源不可信（可信度分级）

按设计文档 §1 的 B2「窗口消息不可信（同 IL 任意进程可 `Post`）」，这条约束在方案 A/B 下都成立，**但在父链下可被确定性利用**：

| 攻击路径 | 顶层窗口 | WorkerW 子窗口 | 我们的代码位置 |
|---|---|---|---|
| 定位目标 HWND | 需 `EnumWindows` 全桌面扫描（可枚举，噪音大） | **`EnumChildWindows(WorkerW)` 精确枚举**，无需猜类名/标题 | — |
| 伪造键入 | `PostMessage(WM_KEYDOWN/WM_CHAR)` | 同上，且父窗口也可 `SendMessage(hwnd, …)` | `app.rs:1188/1194` → `write_to_pty`（B3 唯一写入口**被语义绕过**：消息不是"输入"，但效果等价） |
| 伪造点击/滚轮 | `PostMessage(WM_LBUTTONDOWN/WM_MOUSEWHEEL)` | 同上 + `WM_PARENTNOTIFY` 让父观察我们的点击时序 | `app.rs:1118/1168` |
| 伪造激活/焦点 | `PostMessage(WM_ACTIVATE/WM_SETFOCUS)` | 父窗口**本来就控制**我们的激活时序 | `app.rs:1103` |
| 伪造 IME 上屏 | `PostMessage(WM_IME_CHAR)` | 同上；跨队列下 `WM_IME_STARTCOMPOSITION` 与上屏消息可能重排 | `app.rs:1184` 附近 |
| 伪造 z-order/位置 | `PostMessage(WM_WINDOWPOSCHANGING)` 只能骗我们自己的逻辑 | 父窗口可**真实**移动/改 z-order/`EnableWindow(FALSE)` | — |

**结论（可信度分级）**：在 Medium IL 内，「系统输入队列投递」与「其它进程 `Post`/`Send`」在消息层面**不可区分**——`InSendMessage()` 只能挡 `SendMessage`，`SendInput` 走的仍是队列投递（`InSendMessage()==FALSE`）。因此：

1. 消息层不可能建立可信来源；B2 的正确定位是**能力约束而非来源校验**（只接受参数化消息、不解析指针载荷、不让消息直接变成命令）。
2. 我们唯一能收紧的是**降低被命中的概率与放大系数**：不让父链替攻击者完成"发现窗口"这一步。
3. `dispatch`（`app.rs:1334`）目前连 `WM_NCCREATE` 之外的**任何**来源信息都不看。作为加固可对输入类消息校验 `InSendMessage()==FALSE` 且 `GetQueueStatus(QS_KEY|QS_MOUSE)` 有未读位；但这只挡 `SendMessage` 与"没有真实输入的注入"，**不要把它写成安全边界**，只能记为纵深（见 F-09，按"接受并记录"处理）。

### 2.3 窗口冒充（三个方向）

| 方向 | 可行性 | 说明 | CWE |
|---|---|---|---|
| ① 宿主冒充 WorkerW | **高（现状即成立）** | `is_desktop_owner`（`workerw.rs:85`）只比较 basename。把任意 exe 复制为 `%TEMP%\explorer.exe` 并以 Medium 运行，注册 `WorkerW` 类 + 一个无 `SHELLDLL_DefView` 子窗口的顶层窗口，`top-level-workerw` 策略即会挑中它 → 我们的终端成为攻击者窗口的子窗口 | CWE-290、CWE-345 |
| ② 我方被冒充为输入源 | 中 | 同 IL 进程可 `SendMessage` 到我们的窗口；父链下还可从 `WorkerW` 侧枚举到我们 | CWE-940 |
| ③ 第三方覆盖层 | **高（不可消除）** | 我们是 `WorkerW` 的子窗口，同 IL 进程可创建 `WS_EX_TOPMOST` 窗口或同级子窗口覆盖在我们之上；叠加 D5「子窗口 `SetWindowDisplayAffinity` 无效」，攻击者可**伪造一个看起来是我们的终端**并截屏 | CWE-1021、CWE-451 |

②③ 的共性：把终端渲染进别人的窗口树，等于把"视觉与输入的最终仲裁权"交给该窗口树的属主与同 IL 的任何进程。这是方案 A 的结构性代价，不是实现缺陷。

### 2.4 `AttachThreadInput` 的副作用（结论：禁止引入）

当前代码**没有**调用 `AttachThreadInput`（`grep AttachThreadInput` 在 `src/` 无命中）。评审需明确写下"不要为了修焦点问题引入它"：

| 副作用 | 机理 | 后果 |
|---|---|---|
| 键态共享 | 附加后两线程共享键盘状态表，`GetKeyState`/`GetKeyboardState` 互相可见、互相影响 | 我们读修饰键（`GetAsyncKeyState`，`app.rs:29`）与对方逻辑交叉污染（CWE-653） |
| 输入误投 | `SendInput` 由共享队列的焦点窗口接收；附加期间焦点在对方窗口时，我们"发出的键"落到 explorer | 命令被送进错误目标；反向则 explorer 的输入落进终端（CWE-441 混淆代理） |
| 队列阻塞连坐 | 任一方在 `SendMessage` 到对方时阻塞，会拖住整条共享队列 | explorer 卡住 → 我们 UI 冻结（CWE-362 类） |
| 焦点语义被伪造 | 附加后 `SetFocus`/`GetFocus` 跨线程生效 | 我们可能把焦点"塞"进 explorer 窗口，或反之，前台判定更不诚实（CWE-451） |

替代方案（可靠且无共享）：`SetFocus(hwnd)`（同线程内）+ 自绘 focus ring（工程已定 1px 环 + 530ms 块光标闪烁）+ 不依赖线程键盘态、改用 `WM_KEYDOWN` 的 `lParam` 位域（bit 29 表示 Alt 上下文）判断修饰语义。

### 2.5 代码层已确认的具体缺陷（与方案选择无关，现在就该修）

| 编号 | 位置 | 缺陷 | 可利用性 | CWE |
|---|---|---|---|---|
| F-01 | `workerw.rs:85` | 只比 basename，任意目录的 `explorer.exe` 通过 | 同 IL 无提权即可挂载成功 → 我们的窗口挂到攻击者窗口下 | CWE-290、CWE-345 |
| F-02 | `workerw.rs:106` → `:169` | 找到候选与 `SetParent` 之间无 `IsWindow` 复查、无 HWND 复用防护、**未校验候选覆盖显示器工作区** | TOCTOU：候选销毁后 HWND 值可被复用；窗口不覆盖工作区时可制造视觉伪装 | CWE-367、CWE-1021 |
| F-03 | `workerw.rs:188`、`:219` | 二次 `SetParent` 返回值被丢弃；`attach` **无条件**返回 `Attached` | 样式已被改成 `WS_CHILD` 但实际未挂上 → 孤儿/不可见窗口，且状态不可观测（无第三态） | CWE-672 |
| F-04 | `app.rs:636` | `reattach` 无 ≥2s 去抖、无"新父窗口校验"、复用同一 `attach` 路径 | `TaskbarCreated` 广播风暴 + 每次重挂都可能挂到冒充宿主 | CWE-367、CWE-672 |

**加固骨架（建议，未落地）**：

```rust
// 1) 全路径 + 会话 + 完整性级别
fn is_trusted_desktop_owner(hwnd: HWND) -> bool {
    let Some(full) = owner_process_full_path(hwnd) else { return false };
    let sysroot = std::env::var_os("SystemRoot")?;              // 规范化后比较
    let expected = Path::new(&sysroot).join("explorer.exe");
    if !path_eq_ignore_case(&full, &expected) { return false }   // 不再只比 basename
    session_id_of_process(pid) == session_id_of_current_process()
}

// 2) 挂载前复查 + 覆盖校验 + 挂载后回读
if !IsWindow(workerw).as_bool() { return Attach::Fallback { .. } }
if !covers_primary_work_area(workerw) { return Attach::Fallback { .. } }  // S-02 要求
if !is_trusted_desktop_owner(workerw) { return Attach::Fallback { .. } }   // 不试下一个候选

// 3) SetParent 后必须回读，不等即回滚 style
let prev_style = GetWindowLongPtrW(hwnd, GWL_STYLE);
SetParent(hwnd, Some(workerw))?;
if parent_of(hwnd) != Some(workerw) {
    SetWindowLongPtrW(hwnd, GWL_STYLE, prev_style);           // 回滚 WS_CHILD
    return Attach::Fallback { reason: "SetParent 未生效".into() };
}
```

### 2.6 Q1 的其它边界效应

- **DPI/坐标（CWE-682）**：跨进程父子链下，子窗口继承父窗口的 DPI awareness context。若 explorer 的 `WorkerW` 与 TLShell 的 PMv2 不一致，`WM_DPICHANGED` 时序与客户区坐标会被虚拟化；工程已确立的网格不变量（字号 14/行高 1.25/padding 10 → 字宽 8px、行高 18px、110 列）在跨 DPI 虚拟化下可能失效——这既是显示缺陷也是命中测试缺陷。
- **生命周期（CWE-672）**：explorer 重启会销毁 `WorkerW`，我们是子窗口通常一起没了（`app.rs:632` 注释自述"尽力而为"），但若样式已改而未挂上（F-03），会留下无父的 `WS_CHILD` 窗口，用户看不到却占着进程。
- **前景诚实性（CWE-451）**：子窗口不参与 Alt+Tab/任务栏，用户无法用系统手段确认"这个终端是不是真的在前台、有没有被遮挡"。与 D5（子窗口无法防截屏）叠加，构成本方案最大的可用性-安全耦合代价。

---

## 3. Q2：S-03 最小 env 白名单与固定 cwd

### 3.1 现状

`session.rs:112-118` 只有 `command.env("TERM", "xterm-256color")`，**没有 `env_clear()`** → 除 `TERM` 外全部继承（D13 现状）。设计文档 §5.4 要求「绝对路径 + 显式 env 白名单 + 固定 cwd」，M1 验收要求 `env` 输出中不出现意外继承。绝对路径部分**已落地**（`shell.rs:57-101`，裸名只在白名单目录搜索 + `canonicalize` 复核），env 与 cwd 部分**未落地**。

### 3.2 A 层：必须保留（逐项 + 理由 + 来源）

「来源=自算」表示由 TLShell 通过 API/固定规则生成，**不从父进程 env 继承**（继承版可被伪造）。

| 变量 | 来源 | 保留理由 | 不保留的后果 |
|---|---|---|---|
| `SystemRoot` | 继承（值须等于 `GetWindowsDirectoryW`，否则自算覆盖） | 加载系统 DLL/COM、`%SystemRoot%` 展开；`cmd`/`pwsh` 的多数内置行为依赖它 | 大量程序启动失败或回落到错误目录 |
| `SystemDrive` | 自算（`SystemRoot` 盘符） | `%SystemDrive%` 展开、部分安装/日志脚本 | 脚本拼路径失败 |
| `windir` | 自算 = `SystemRoot` | 与 `SystemRoot` 同义但**两个名字都被广泛读取**（大小写不敏感，值需一致） | 读 `%windir%` 的脚本失败 |
| `ComSpec` | **自算** = `%SystemRoot%\System32\cmd.exe` | `system()`、`CommandBuilder` 内部、批处理 `%COMSPEC%` | 子进程无法启动 `cmd`；**若继承则可被指向同 IL 攻击者路径**（F-06 一部分） |
| `PATH` | **重建**（见 §3.3） | shell 内命令解析 | 继承版 PATH 可含 `%USERPROFILE%\bin` 等可写目录 → 命令劫持（CWE-427） |
| `PATHEXT` | **自算**（见 §3.3） | 决定无扩展名命令的解析顺序 | 继承版可加入 `.VBS/.JS/.WSF` → 双击等价行为被悄悄启用（CWE-427） |
| `TEMP`、`TMP` | **自算** → 会话私有目录 | 临时文件隔离 | 继承共享 `%TEMP%` → 同 IL 进程可预置/读取临时文件（CWE-377） |
| `USERPROFILE` | 继承（若与 `SHGetKnownFolderPath(FOLDERID_Profile)` 不一致则以 API 为准） | home 展开、`cwd` 基准、大量工具默认路径 | `cwd` 无基准（见 §3.5） |
| `HOMEDRIVE`、`HOMEPATH` | 自算（拆分 `USERPROFILE`） | 老程序读 `HOMEDRIVE`+`HOMEPATH` 而非 `USERPROFILE` | 部分工具把 home 解析成 `C:\` |
| `USERNAME`、`USERDOMAIN` | 自算（`GetUserNameW` + token 查询） | 提示符、日志、少数按用户名分支的脚本 | 提示符/日志出现伪造身份；**继承版可被伪造** |
| `APPDATA`、`LOCALAPPDATA` | 自算（`SHGetKnownFolderPath`） | 应用配置/缓存根；`TEMP` 私有目录的父目录 | 配置写到错误位置；继承版可被指向攻击者目录（CWE-15） |
| `ProgramData` | 自算（Known Folder） | 机器级配置 | 少 |
| `ProgramFiles`、`ProgramFiles(x86)`、`ProgramW6432` | 自算（Known Folder / `%SystemDrive%\Program Files`） | 安装路径解析；`shell.rs:38` 也用它定位 pwsh | 白名单目录与子进程认知不一致 |
| `CommonProgramFiles`、`CommonProgramFiles(x86)`、`CommonProgramW6432` | 自算 | 共享组件定位 | 少 |
| `OS` | 自算 = `Windows_NT` | 极多脚本的首要平台判据 | 脚本走错分支 |
| `PROCESSOR_ARCHITECTURE` | 自算（`GetNativeSystemInfo`） | 架构判据 | 脚本选错二进制 |
| `PROCESSOR_ARCHITEW6432` | 自算（仅 WOW64 进程设置） | 32 位宿主下的正确架构判据 | WOW64 下 `%PROCESSOR_ARCHITECTURE%` 误导 |
| `NUMBER_OF_PROCESSORS` | 自算（`GetActiveProcessorCount`） | `-j` 类默认值 | 并行构建退化为 1，功能受损 |
| `TERM` | 自算 = `xterm-256color` | 已有（`session.rs:118`） | 全屏程序不启用颜色/光标控制 |
| `COLORTERM` | 自算 = `truecolor` | 告诉程序支持 24 位色 | 退化为 256 色 |
| `TLSHELL_SESSION` | 自算（会话 id） | 设计 §7.2 A-11 指定的替代标识（**不注入 `WT_SESSION`**） | 无法区分多会话 |
| `LANG`/`LC_ALL` | 不设 | Windows 下无意义，设了反而误导 | — |

### 3.3 三个必须"重建"的变量

| 变量 | 规则 | 理由 |
|---|---|---|
| `PATH` | `%SystemRoot%\System32`；`%SystemRoot%`；`%SystemRoot%\System32\Wbem`；`%SystemRoot%\System32\WindowsPowerShell\v1.0`；`%SystemRoot%\System32\OpenSSH`（存在才加）；`%ProgramFiles%\PowerShell\7`（存在才加）。**逐项 `canonicalize` 后校验非重解析点、非用户可写目录**（`%ProgramFiles%` 需检查 ACL 不含当前用户写权限，退化为"存在性 + 非 `%TEMP%`/`%USERPROFILE%` 前缀"的近似判据） | 继承的 `PATH` 是 CWE-427 的入口：`%USERPROFILE%\bin`、`%LOCALAPPDATA%\Microsoft\WindowsApps`（含 `python.exe` 之类的 App Execution Alias 桩）都可被同 IL 用户进程写入 |
| `PATHEXT` | 固定 `.COM;.EXE;.BAT;.CMD` + 仅当用户显式配置时追加 `.PS1`。**不含 `.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC`** | 这些扩展会以 Windows Script Host 执行，等价于"命令即代码"；白名单化能显著降低误触与社工面 |
| `PSModulePath` | **重建**：PS5 → `%USERPROFILE%\Documents\WindowsPowerShell\Modules;%ProgramFiles%\WindowsPowerShell\Modules;%SystemRoot%\System32\WindowsPowerShell\v1.0\Modules`；PS7 → `%USERPROFILE%\Documents\PowerShell\Modules;%ProgramFiles%\PowerShell\7\Modules;%ProgramFiles%\PowerShell\Modules` | 继承版会把**任意可写目录**塞进模块搜索序；`Import-Module` 按名字加载即被劫持（CWE-427），且会触发 `PSModulePath` 上的 `PSModuleAutoLoading`（用户脚本自动执行） |

### 3.4 B 层：一律拒绝继承（黑名单，示例非穷举）

按"能改子进程行为"或"泄露本进程凭据"分类：

| 类别 | 变量 | 风险 | CWE |
|---|---|---|---|
| 云凭据 | `AWS_*`（`AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/`AWS_SESSION_TOKEN`/`AWS_PROFILE`/`AWS_CONFIG_FILE`）、`AZURE_*`、`GOOGLE_APPLICATION_CREDENTIALS`、`ARM_*` | 凭据被任意终端命令读到并外传 | CWE-526 |
| 代码托管/包管理令牌 | `GITHUB_TOKEN`、`GH_TOKEN`、`GITLAB_TOKEN`、`NPM_TOKEN`、`CARGO_REGISTRY_TOKEN`、`DOCKER_AUTH_CONFIG` | 同上 | CWE-526 |
| SSH/凭据代理 | `SSH_AUTH_SOCK`、`SSH_AGENT_PID`、`KUBECONFIG` | 无需私钥即可签名/访问集群 | CWE-526 |
| 代码执行注入 | `NODE_OPTIONS`、`NODE_PATH`、`PYTHONPATH`、`PYTHONSTARTUP`、`PYTHONHOME`、`PERL5LIB`、`PERL5OPT`、`RUBYOPT`、`JAVA_TOOL_OPTIONS`、`_JAVA_OPTIONS`、`BASH_ENV`、`ENV`、`PROMPT_COMMAND`、`PS4` | 环境变量直接变成代码执行（`NODE_OPTIONS=--require x.js`） | CWE-15、CWE-94 |
| 构建工具注入 | `RUSTC_WRAPPER`、`RUSTFLAGS`、`RUSTDOCFLAGS`、`CL`、`_CL_`、`LINK`、`INCLUDE`、`LIB`、`CC`、`CXX` | 构建时执行任意程序/注入编译参数 | CWE-15 |
| Git 注入 | `GIT_SSH`、`GIT_SSH_COMMAND`、`GIT_EXEC_PATH`、`GIT_CONFIG_GLOBAL`、`GIT_CONFIG_SYSTEM`、`GIT_PROXY_COMMAND` | 每个 git 命令都执行外部程序 | CWE-15 |
| 兼容层/代理 | `__COMPAT_LAYER`（可强制 `RunAsInvoker`/DPI 伪装）、`HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY`、`*_proxy` | 违背零网络承诺或改变提权语义 | CWE-15、CWE-807 |
| 搜索路径 | 继承的 `PATH`、`PATHEXT`、`PSModulePath`、`ComSpec` | 见 §3.3 | CWE-427、CWE-426 |
| 进程标识伪造 | 继承的 `USERNAME`、`USERDOMAIN`、`USERPROFILE`、`APPDATA`、`LOCALAPPDATA`、`TEMP` | 见 §3.2 自算项 | CWE-15、CWE-377 |

### 3.5 固定 cwd 的选择

**选择**：`shell.cwd` 默认 `None` → 解析为 `%USERPROFILE%`（Known Folder API），并按下述规则校验。

| 规则 | 理由 | CWE |
|---|---|---|
| 必须是**绝对路径**（`Path::is_absolute`） | 相对 cwd 等价于把"当前目录"交给父进程 | CWE-73、CWE-426 |
| 拒绝 UNC（`\\server\share`、`\\?\UNC\…`） | `cd`/`.\x.exe` 会走 SMB，违背 §2「零网络」（S-16） | CWE-73 |
| `canonicalize` 后仍须存在且是目录 | 不存在则由 `CreateProcessW` 静默回落到父 cwd（失败不报错的经典坑） | CWE-426 |
| 拒绝重解析点（`FILE_ATTRIBUTE_REPARSE_POINT`） | junction/symlink 可把 cwd 指向别处，使"固定"失效 | CWE-59 |
| **`USERPROFILE` 缺失/无效 → 拒绝启动并报错** | 现在的 `app.rs:325` 返回 `None` → `session.rs:114` 不设 cwd → **继承 TLShell 自身 cwd**（可能是 `System32` 或任意启动目录） | CWE-426 |
| 不使用 `std::env::current_dir()` 作为兜底 | 兜底就是继承，等于不做 | CWE-426、CWE-15 |

配置形态建议（与 §7.2 A-11/A-12 一致）：

```toml
[shell]
cwd = ""          # 空 = %USERPROFILE%；非空须为绝对路径且通过上述 5 项校验

[env]             # 显式注入（D13 / 6.2-2）；键名不得落在 §3.4 黑名单
# AWS_PROFILE = "dev"    # 需要就显式写，绝不继承
```

**实现骨架（建议，未落地）**：

```rust
fn build_child_env() -> HashMap<OsString, OsString> { /* env_clear 后逐项 set */ }

// session.rs
let mut command = CommandBuilder::new(&resolved.program);   // 已是绝对路径（shell.rs 保证）
command.env_clear();                                        // 关键前提
for (k, v) in build_child_env() { command.env(k, v); }      // A 层 + 重建项
for (k, v) in &config.env {                                 // [env] 显式注入
    if is_denied(&k) { return Err(...) }
    command.env(k, v);
}
command.env("TERM", "xterm-256color").env("COLORTERM", "truecolor");
command.cwd(resolve_cwd(&config.shell.cwd)?);               // 绝对 + 无 UNC + 无 reparse
```

### 3.6 Q2 的验收断言（对齐设计 §8 M1）

1. 启动后 `set` / `env` 输出中，除 §3.2/§3.3 列出的键外**无任何键**；`AWS_*`、`GH_TOKEN`、`SSH_AUTH_SOCK`、`NODE_OPTIONS`、`RUSTC_WRAPPER` 必须缺席。
2. `cd`（无参）输出等于 `%USERPROFILE%`。
3. 把 `USERPROFILE` 从自身环境删除后启动 → **TLShell 报错退出**，而不是落到 `C:\Windows\System32`。
4. `shell.program = "cmd"` 且 `PATH` 内含 `%USERPROFILE%\bin\cmd.exe` 时，仍启动 `System32\cmd.exe`（`shell.rs` 已满足）。
5. `PATH` 中不出现任何 `%TEMP%`、`%USERPROFILE%`、`%LOCALAPPDATA%` 前缀项。

---

## 4. Q3：改为「顶层窗口沉底」后信任边界如何变化

### 4.1 边界变化对照

| 边界/属性 | 方案 A：`SetParent` → WorkerW（现状） | 方案 B：顶层窗口沉底（`HWND_BOTTOM` 或 `GWLP_HWNDPARENT=Progman` 仅设 owner） |
|---|---|---|
| 窗口父子关系 | **跨进程**父链（explorer 为父） | 本进程内；`WS_POPUP` 保持，`GWLP_HWNDPARENT` 对 popup 窗口设的是 **owner** 而非 parent |
| 输入队列 | 跨队列父子链，`WM_PARENTNOTIFY` 外发 | 完全本进程，无外发 |
| CWE-668（资源暴露给错误域） | 存在 | **消除** |
| CWE-653（隔离/分区失败） | 存在（父可改使能/z-order/销毁） | **消除** |
| CWE-441（混淆代理，经 ATI） | 若引入 ATI 则存在 | **消除**（无需 ATI） |
| CWE-290 / CWE-940（消息伪造） | 存在，且 `EnumChildWindows` 精确锚定 | **存在，但只能靠 `EnumWindows` 扫描** → 无父链放大 |
| CWE-682（DPI 坐标） | 存在（继承父 awareness context） | **消除**（本进程自洽，网格不变量稳定） |
| CWE-451（UI 误表示） | 存在（子窗口不参与 Alt+Tab/任务栏，前台判定不诚实） | **消除**（可选显示在任务栏，Alt+Tab 可见） |
| CWE-1021（覆盖/视觉伪装） | 高（同 IL 覆盖层 + D5 无法防截屏） | 中（仍可被 `TOPMOST` 覆盖，但 **`SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` 可用**） |
| S-07 截屏防护 | **不可达**（D5） | **可达** → 可升级为"子窗口方案无法提供的"缓解 |
| Win+D 免疫 | 有（子窗口不参与 Show Desktop） | **丢失**（顶层窗口被 Show Desktop 一起最小化）→ 功能回退 |
| z-order 维护 | 由 WorkerW 层级决定，代码只需 `HWND_TOP` | 需在 `WM_WINDOWPOSCHANGING` 重申沉底；有闪烁/抢焦点风险（S-06） |
| `TaskbarCreated` 重挂 | 必须（S-14：≥2s 去抖 + 新父校验） | 仅在 `owner=Progman` 时需要重新设 owner；否则**整个 S-14 失效为"不需要"** |
| 提权 / IL | `asInvoker` / Medium（不变） | 不变 |

### 4.2 判定

- **安全维度**：方案 B 严格优于方案 A。它把 CWE-668/653/682/451 整条链从设计里去掉，并把 S-07（截屏防护）从"不可达"变成"可落地"。这三项在方案 A 下**无论怎么修 workerw.rs 都拿不到**——它们由 `SetParent` 的语义决定，不是实现缺陷。
- **功能维度**：方案 B 失去 Win+D 免疫（原本是选择 WorkerW 的主要动机）。这不是安全回退，而是产品取舍。
- **共同残余**：两个方案都无法在 Medium IL 内阻止同 IL 进程 `PostMessage` 伪造输入（B2 的固有约束）。要真正收紧只有三条路：提升 IL/UIPI（与 `asInvoker` 冲突）、`UIAccess`（设计明确不请求）、或**把高价值动作改为不可由消息伪造的形态**（粘贴二次确认的确认键用 `WM_KEYDOWN` + 真实键盘态双检、拒绝 `InSendMessage` 消息、关键动作前要求物理键组合）。第三条是唯一与现有设计兼容的方向。
- **建议**：把 S-02 拆成两条路径，先按 §2.5 把 F-01～F-04 修掉（无论最终选哪个方案都需要），同时把「Win+D 免疫的替代实现」立为独立任务；替代实现验证通过后切到方案 B，届时 S-14 可整体关闭。

---

## 5. 发现清单

| 编号 | 严重度 | 标题 | 证据 | 加固 | CWE |
|---|---|---|---|---|---|
| F-01 | **高** | WorkerW 属主只比 basename，任意 `explorer.exe` 可冒充宿主 | `workerw.rs:85` | 比较完整路径 = `%SystemRoot%\explorer.exe` + 同会话 + 完整性级别校验 | CWE-290、CWE-345 |
| F-02 | **高** | 候选查找与 `SetParent` 之间无复查；未校验工作区覆盖 | `workerw.rs:106` → `:169` | `IsWindow` 复查 + `rcWork` 覆盖校验 + 不满足即 Fallback（不试下一候选） | CWE-367、CWE-1021 |
| F-03 | **高** | 二次 `SetParent` 返回值丢弃 + `attach` 无条件返回 `Attached` | `workerw.rs:188`、`:219` | 回读 `parent_of` 必须等于候选，否则回滚 `GWL_STYLE` 并返回 Fallback | CWE-672 |
| F-04 | 中 | `reattach` 无去抖、无新父校验 | `app.rs:636` | ≥2s 去抖 + 单次失败不重试 + 复用加固后的 `attach`（S-14） | CWE-367、CWE-672 |
| F-05 | **高** | `USERPROFILE` 缺失时 cwd 为 `None` → 继承 TLShell 自身 cwd | `app.rs:325`、`session.rs:114` | 缺失即拒绝启动；cwd 走 §3.5 五项校验 | CWE-426、CWE-73 |
| F-06 | **高** | `CreateProcessW` 无 `env_clear()`，全量继承（含云凭据/令牌/注入类变量） | `session.rs:112-118` | `env_clear()` + §3.2 白名单 + §3.4 黑名单 + `[env]` 显式注入（D13） | CWE-526、CWE-15、CWE-427 |
| F-07 | 中 | `PATH` / `PATHEXT` / `PSModulePath` 继承可写目录 → 命令/模块劫持 | 同上 | 三项按 §3.3 重建；`PATHEXT` 去掉脚本宿主扩展 | CWE-427 |
| F-08 | **高** | `cmd` 注入 `/K` 未配 `/D` → `HKCU\Software\Microsoft\Command Processor\AutoRun` 被自动执行 | `shell.rs:146` | 注入参数改为 `/D /K "chcp 65001 >nul"`；PS5 路径同步复核 `-NoProfile` 是否需显式加 | CWE-15、CWE-426 |
| F-09 | 中（接受） | `dispatch` 无消息来源校验，同 IL 可 `PostMessage` 伪造键入 → 直达 B3 写入口 | `app.rs:1334`、`:1188`、`:1194` | 记为 B2 固有约束；纵深可加 `InSendMessage()==FALSE` 过滤，**不得**当作安全边界 | CWE-940、CWE-345 |
| F-10 | 低 | 修饰键靠 `GetAsyncKeyState` 全局读取，`SendInput` 可翻转 Alt/Ctrl 语义 | `app.rs:29`、`input.rs:4` | 改用 `WM_KEYDOWN` 的 `lParam` 位域（bit 29 Alt 上下文）+ 自绘 focus ring，不引入 ATI | CWE-290、CWE-693 |
| F-11 | 低 | `TEMP`/`TMP` 继承共享临时目录 | `session.rs`（无相关语句） | 会话私有 `%LOCALAPPDATA%\TLShell\tmp\<pid>`，退出清理 | CWE-377 |
| F-12 | 信息 | `shell.args` 可传 `-Command`/`/c` 覆盖注入逻辑 | `shell.rs:146` | 保持现状（属于用户显式意图），但在 README 标注"args 即代码" | CWE-15 |

---

## 6. 遗留未知

| 编号 | 未知 | 影响 | 建议探测 |
|---|---|---|---|
| U-01 | 不同 Windows 版本的 `WorkerW` 层级与类名差异（设计 §9 明确不评估） | 方案 A 的兜底策略在各版本行为不一致 | 在 10 21H2 / 11 22H2+ / Server 上实测 `FindWindowExW` 三级结果 |
| U-02 | explorer 的 DPI awareness context 具体值（PMv1 / System） | 跨进程 `SetParent` 后的 DPI 虚拟化程度不确定 | `GetAwarenessFromDpiAwarenessContext(GetWindowDpiAwarenessContext(workerw))` |
| U-03 | 「Win+D 免疫」的替代实现（`IApplicationDesignModeSettings`/`IVirtualDesktopManager`/desktop-level owner）可行性未验证 | 决定能否切方案 B | 原型验证：顶层窗口 + owner=Progman 在 Show Desktop 下的行为 |
| U-04 | 第三方壁纸程序（Wallpaper Engine 类）在两种方案下的覆盖行为 | 影响视觉伪装面 | 实测 |
| U-05 | Medium IL 下 UIPI 对 `PostMessage`/`SendMessage` 到本进程的实际拦截面 | 决定 F-09 的残余风险 | 用同 IL 测试进程实测 |
| U-06 | `%ProgramFiles%` 的 ACL 是否含当前用户写权限（取决于安装位置） | 影响 §3.3 `PATH` 项的校验策略 | 部署后自检并记日志 |

---

## 7. 文档同步台账（本次评审要求同步，未修改源码/设计文档）

| 文档 | 位置 | 建议变更 |
|---|---|---|
| `docs/design/2026-10-22-tlshell-security-architecture.md` | §6 S 表 S-02 | 状态从「待落」改为「部分落地（basename 校验已存在，`workerw.rs:85`）」；拆出 **S-02b**（工作区覆盖校验）与 **S-02c**（挂载后回读 + TOCTOU 复查） |
| 同上 | §6 S 表 S-03 | 拆为 **S-03a**（`env_clear` + 白名单 + `[env]`）与 **S-03b**（固定 cwd + 缺失拒绝启动）；绝对路径部分标记「已落地（`shell.rs:57-101`）」 |
| 同上 | §4 D 表 D13 | 补 `[env]` 配置 schema 与 §3.4 黑名单的落地形态，与 §7.2 A-11（不注入 `WT_SESSION`）保持一致 |
| 同上 | §6 S 表 | 新增 **S-02d**（`cmd` 必须 `/D`，即 F-08） |
| 同上 | §2 / §5.4 | 明确"禁止 `AttachThreadInput`"，并把理由（§2.4）写入 |
| 同上 | §7.2 | 新增 A-16：`PATHEXT` 不含脚本宿主扩展 |
| `README` | 安全说明 | 保留 D5「子窗口无法防截屏」表述；若切方案 B 需改写为"启用 `WDA_EXCLUDEFROMCAPTURE` 后有效" |
