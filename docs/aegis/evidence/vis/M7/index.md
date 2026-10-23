# M7 视觉与可用性证据

本目录记录 M7 迭代（设置面板、沉底模式裁决、壁纸对比度、降权重启、IME 路径）的证据。
所有 PNG 都由 `tlshell.exe --dump-frame` 落盘 BMP 后转码而来：落盘的是**推给
`UpdateLayeredWindow` 的同一份像素**，不是屏幕截图（窗口可能被别的窗口盖住，截图不可信）。

| 文件 | 内容 | 生成方式 |
|---|---|---|
| `04-settings-panel.png` | 设置面板（8 行、选中行高亮、值右对齐、底部提示行） | `scripts/verify-settings.ps1` |
| `05-wallpaper-white.png` | 白底合成（最差条件）下的正文与选区 | `scripts/verify-wallpaper.ps1` |
| `05-wallpaper-black.png` | 黑底合成下的正文与选区 | 同上 |

## 设置面板（V-52）

- 打开：`F2` 或 `Ctrl+,`；关闭：`Esc`（不保存）/ `Enter`（保存）。
- 8 行：不透明度、字号、行高、内边距、圆角、粘贴保护、OSC 52 写入剪贴板、**失焦沉到桌面层**。
- 保存走 `config::save_atomic`（`.toml.tmp` + `rename`），保存成功后立刻 `reload_fonts` + `apply_size`，
  不需要重启。
- 实测（`%TEMP%\aegis-settings\config.toml`）：`opacity = 0.88` → 按两次 `→` → `Enter` → 文件里
  `opacity = 0.92`；日志 `打开设置面板（7 项）` / `设置已写入 …`。
  注：日志里写「7 项」是打开时的行数快照（沉底开关后加），以面板实际渲染为准，见 PNG。

## 壁纸对比度（V-44…V-51 的对比度部分）

| 壁纸 | 背景取样 | 正文取样 | 对比度 |
|---|---|---|---|
| 白（`#FFFFFF`） | (57,57,71) | (211,219,245) | **8.23 : 1** |
| 黑（`#000000`） | (26,26,40) | (180,188,214) | **9.09 : 1** |

取样规则：背景取右下空白像素；正文取第一行文字带里最亮的像素（固定坐标曾打到背景上，
把对比度算成 1.0，已修正）。两端口径都过 WCAG AA（4.5:1），最差 8.23:1。
`run-all.ps1` 的 D 层每轮都复算这两项，六轮结果一致。

**这是上下界，不是全部壁纸**：真实壁纸是任意颜色，白/黑是最坏两端，中间色的对比度介于两者之间
（因为合成是线性的）。真正需要单独验的是「与正文同亮度区间的中间灰」，目前未采样。

## 「无法点击/输入」的裁决（本轮最重要的一条）

上一轮把窗口改成顶层 + `HWND_BOTTOM` 常驻最底层。真机上这等于：窗口永远在**所有最大化窗口
下面**，鼠标点不到、键盘也进不来。真实 `SendInput` 探针证明了消息处理本身没问题：

```
抬到最前 foreground=true 命中=(490,250) -> 自己:TLShellWindow
真实点击@(490,250) moved=true down=true up=true
点击取焦点: foreground=true focus_set=true keyboard_ready=true
真实拖选 (152,180)->(490,180) steps=4 → 扩展选择 line=5 col=9..39
真实按键 3 次 → 自检写入 9 字节
沉底后命中=(490,250) -> 其他窗口:Chrome_RenderWidgetHostHWND
```

所以缺陷不在命中测试、不在焦点链、不在键盘编码，只在 z-order。修法是把「沉底」从**唯一行为**
降级为**可选模式**（`window.sink_when_unfocused`，默认 `false`），并给一个 `Ctrl+Alt+T`
把它从最底层叫回来。设置面板里可以现场切换，切完立即生效。

## 启动可见性（本轮新增的第二个成因）

把窗口改成顶层 + 沉底之后，还有第二个「看不见也点不到」的成因：`SW_SHOWNOACTIVATE`
只负责画出来，窗口会停在普通 z-order 的最下层。用户桌面上只要有最大化窗口（本机是
Chrome，rect -7,-7..1897,1070），终端就整个被盖住。

实测三步都试过：

| 做法 | 结果 |
|---|---|
| `SetForegroundWindow` + `SetActiveWindow` + `SetFocus` | `foreground=false focus_set=true`（前台锁拒绝后台进程抢前台） |
| 加 `SetWindowPos(HWND_TOP)` | 命中自检仍是 `其他窗口:Chrome_RenderWidgetHostHWND` —— 前台线程有额外特权，普通 z-order 抬不过它 |
| 加 `SetWindowPos(HWND_TOPMOST)` | 命中自检变成 `自己:TLShellWindow` —— 看得见、点得到 |

最终行为：启动时 `HWND_TOPMOST` 抬到最前 → 取焦点 → 如果真拿到前台（用户从资源管理器
启动的正常路径）立刻 `HWND_NOTOPMOST` 退回普通层；被前台锁挡住（后台/脚本启动）就保持
topmost，等第一次 `WM_ACTIVATE(0)` 再退下来。`Ctrl+Alt+T` 走同一条路径。

回归断言在 `run-all.ps1` C 层：`启动即可见可点`（`foreground=true` 或命中自检落在自己窗口上）
与 `唤起热键生效`（投 `WM_HOTKEY` 后必须出现「收到唤起热键」）。

## 未验证项（不掩饰）

1. **提权重启的提权分支**：`--restart-unelevated` 只在非提权下验过（退 0 + 日志
   「当前不是管理员，无需降权重启」）。提权分支要真弹 UAC，本机无法自动化，代码路径已写但未跑通。
2. **IME 真机上屏**：`WM_IME_COMPOSITION` + `ImmGetCompositionStringW(GCS_RESULTSTR)` 已实现，
   自检里 `ime_result_absent` 只证明「无组字时返回 None 且不崩」。真机上屏需要真人用输入法敲一次。
3. **中间灰壁纸**：见上，只验了白/黑两端。
4. **录屏类条目**：本机无录屏工具链。用「逐帧落盘 + 像素分析」替代（同一份合成像素），
   能验静态对比度与形状，验不了动态过渡（淡入淡出、光标闪烁时序）。

## 打字乱码的定位与修复（本轮新增）

User 报障：敲 `ipconfig` 屏幕上出现 `ip.o1!i"ipconfig`。拆开看正好是两半：前半是**错字符**，
后半是**同一串字符的正确副本**，对应两个独立缺陷。

### 缺陷一：扫描码被当成 UTF-16 码元

`VK_PACKET`（`0xe5`）有两个来源，处理顺序写反了：代码先判断「扫描码 ≥ 0x20 就是 UTF-16 码元」，
可真实键盘/虚拟化注入送来的是**真实扫描码**。字母 C 的扫描码是 `0x2E`、N 是 `0x31`、F 是 `0x21`、
G 是 `0x22`——它们全都 ≥ 0x20，于是被当成码元直接写进 PTY：

| 按键 | 扫描码 | 错误输出 | 正确输出 |
|---|---|---|---|
| c | `0x2E` | `.` (46) | `c` (99) |
| n | `0x31` | `1` (49) | `n` (110) |
| f | `0x21` | `!` (33) | `f` (102) |
| g | `0x22` | `"` (34) | `g` (103) |
| i / p / o | `0x17/0x19/0x18` | 正确 | — |

i、p、o 的扫描码都 < 0x20，所以「看起来只错了一部分」——这正是 `ip.o1!i"` 的形状。

修法：**先按扫描码解释**（`MapVirtualKeyW(scan, MAPVK_VSC_TO_VK_EX)` → `ToUnicodeEx`），只有
扫描码 ≥ 0x80（不可能是扫描码）时才退回 UTF-16 码元，那条路留给 CJK 与 emoji。

### 缺陷二：同一个键写了两遍

修好翻译后，同一个键还会来一条 `WM_CHAR`（真键盘路径实测如此）。`VK_PACKET` 分支当时不设
抑制窗，于是「错字符 + 正确字符」各写一份——这就是用户看到的 16 个字符。现在写完后立刻
arm `suppress_char_until`（50ms），紧随其后的 `WM_CHAR` 被丢弃。

### 证据

`--probe-real` 现在敲完整单词 `ipconfig`（而不是三个孤立键），日志逐字节可核：

```
修复前：105 112 46 111 49 33 105 34      → ip.o1!i"
修复后：105 112 99 111 110 102 105 103   → ipconfig
```

`run-all.ps1` E 层断言这八个字节按顺序出现（`(?s)自检写入 1 字节: \[105\].*?\[103\]`），
六轮每轮都跑。单测 `packet_scan_codes_are_never_read_as_raw_code_points` 守不变量：
任何扫描码都不得原样变成「数值相同的字符」。

**诚实边界**：探针是从进程内 `SendInput` 发的，它复现了用户报告的**同一个字节模式**，
但「真人用物理键盘」这一步仍无法在自动化里替代；`VK_PACKET` 的具体来源（虚拟化/输入法）
在不同机器上可能不同，所以判据放在「字节是否正确」，而不是「走了哪条分支」。

## 输入法「打一遍、回车又一遍」的修复

现象：微软拼音候选条组字时终端里已经出现拼音字母，按空格/回车选词后又出现一遍，
最终得到两遍。两处独立原因：

1. **组字期间终端自己也在翻译按键**。`WM_KEYDOWN` 里我们用 `ToUnicodeEx` 自行翻译，
   组字状态下它返回的是拼音字母（`a`、`d`、`s`…），于是候选窗里一组字、终端里一组字。
   修法：`src/win/ime.rs::is_composing` 用 `ImmGetCompositionStringW(GCS_COMPSTR)` 判断
   「正在组字」，此时把键盘整个让给输入法（仅非 Ctrl/Alt 时）。
2. **上屏结果走了两条消息**。`WM_IME_COMPOSITION`(`GCS_RESULTSTR`) 与 `WM_IME_CHAR`
   携带同一段文本，两条都写就是「回车又输一遍」。修法：两条路都汇到
   `App::commit_ime_text`，按「内容相同 + 来源不同 + 400 ms 内」判重，命中即丢弃；
   判据是纯函数 `ime_is_duplicate`，单测覆盖同源重复（连打两个「中」不能被吞）。

证据：

- 单测 5 条（`same_text_from_the_other_path_is_a_duplicate`、
  `the_same_path_may_repeat_the_same_text`、`different_text_is_never_a_duplicate`、
  `duplicates_expire_with_the_window`、`the_first_commit_is_never_a_duplicate`）。
- 真机自检 `IME 去重自检: 第一次写入=3 字节 第二次写入=0 字节 去重=true`，
  `run-all.ps1 -RealInput` 每轮断言这一行。

**手动验证步骤**（真实输入法必须人工确认，自动化只能覆盖到上面这条）：

1. `cargo build --release` 后运行 `target\release\tlshell.exe`。
2. 切到微软拼音，输入 `nihao`，**组字期间终端里不应出现 `nihao`**（只有候选条有）。
3. 按空格/回车选词，终端里应恰好出现一次「你好」。
4. 查看 `%LOCALAPPDATA%\TLShell\logs\tlshell.log`，应看到一行
   `输入法上屏重复（同内容、另一条消息），已丢弃` —— 这说明第二条消息确实被吞掉了；
   若没有这行，说明本机输入法只发其中一条，也没有重复可去。

## 桌面层默认值

用户反馈「感觉不是绑定桌面的，可以在任意窗口层级」，因此
`window.sink_when_unfocused` 默认改为 `true`：失焦即沉到 `HWND_BOTTOM`，与壁纸同层，
普通窗口永远在它上面。代价写在 README「已知限制 4」：最大化窗口会盖住它，需要
`Ctrl+Alt+T` 或设置面板把这一项关掉。自检期间（`--probe-real`）临时不沉底，
否则探针拿不到前台、合成事件会落到别人窗口上（本轮修的就是这个）。

## 背景透明度与文字透明度分开（本轮）

用户要求「字体的透明度和背景透明度分开，并且背景透明度下限设置为 0」。做法是把绘制面从
`0x00RRGGBB` 改成**预乘 ARGB**，此后背景与文字各自带 alpha，互不影响：

- `Painter::bg_alpha` 只作用于铺底（`Frame::clear_with`）与单元格底色；
- `Painter::text_alpha` 只作用于字形覆盖度（`scale_coverage`）与下划线/删除线；
- `config.window.opacity` 下限从 `0.10` 放开到 `0.00`，新增 `config.window.text_opacity`（下限 `0.10`）。

### 像素级证据

`scripts/verify-alpha-split.ps1` 生成两版配置（`opacity = 0.00`，`text_opacity` 分别 1.00 / 0.40），
各落一帧（合成底色 `#1E1E2E`），再采样背景点与正文像素：

| `text_opacity` | 背景取样 | 非背景像素 | 平均亮度 |
|---|---|---|---|
| 1.00 | (30,30,46) | 1677 | 152 |
| 0.40 | (30,30,46) | 1662 | 82 |

背景取样等于合成底色 `(30,30,46)`，说明**背景真的没画**（不是「画得很淡」）；
正文平均亮度从 152 掉到 82，与 `35 + 0.40 × (152 − 35) = 81.8` 吻合，说明只有字变淡。
这条脚本已接进 `run-all.ps1` 的 D 层（用例「背景/文字透明度互不影响」）。

### 附带修掉的一个「点不动」

分层窗口的命中测试按**像素 alpha** 做：alpha 为 0 的像素不接收鼠标，消息直接穿透到下面的
窗口。背景透明度调到 0 之后，整块客户区变成「只有字形能点中」，点空白处等于点在后面的窗口上。
修法：铺底 alpha 至少有 1/255（`MIN_PAINTED_ALPHA`，人眼不可见），客户区保持可点击。
单测 `zero_background_alpha_still_keeps_the_surface_clickable` 锁这条不变量。

## 搜狗输入法：组字期的拼音不该进 PTY

用户反馈：微软拼音正常，搜狗拼音**组字期间终端就出现拼音字母**，回车选词后又上屏一遍。
根因是判据选错了：`is_composing` 依赖 `ImmGetCompositionStringW(GCS_COMPSTR)`，而搜狗组字时
这条不返回内容，于是 `is_composing` 恒为 false，字母被当成普通字符写进 PTY。

现在的判据是**转换模式**而不是组字串：`ime::wants_native_input(hwnd)` 读
`ImmGetConversionStatus`，只要处于中文/假名模式（`IME_CMODE_NATIVE | IME_CMODE_KATAKANA`）就
不把字母写进 PTY，交给输入法自己在候选窗里处理；上屏结果仍走 `WM_IME_COMPOSITION`
（`GCS_RESULTSTR`），与 `WM_IME_CHAR` 之间靠已有的 400 ms 去重挡重复。

**诚实边界**：本机没有装搜狗输入法，自动化无法复现它的消息序列；这条修复的判据是
Windows 官方转换模式语义（与输入法品牌无关），但「搜狗真机上确实只上屏一次」需要人工确认：

1. `cargo build --release` → `target\release\tlshell.exe`；
2. 切搜狗拼音，敲 `nihao` —— 终端里**不应**出现 `nihao`；
3. 回车/空格选词 —— 终端里应恰好出现一次「你好」；
4. 日志里若出现 `输入法上屏重复（同内容、另一条消息），已丢弃`，说明第二条消息被吞掉了。

## 沉底到最底的可断言证据

「沉底」以前只看 `SetWindowPos` 的返回值，它成功并不等于真的到了最底。新增
`dock::windows_below(hwnd)`：沿 `GetWindow(GW_HWNDNEXT)` 数自己下面还剩几个窗口，并跳过
`Progman` / `WorkerW` / `Shell_TrayWnd` 这类本来就在最底的壳窗口。启动日志因此变成：

```
[info] 启动沉底: ok=true 下方窗口=0
```

`run-all.ps1` C 层断言这行（用例「桌面层沉底到最底」）。点击时同样会重新沉底，日志
`沉底: ok=true 下方窗口=N`（N 为点击瞬间的状态）——前台与键盘保持不变，只有叠放次序回最底。

### 只数「下方窗口」不够：`HWND_BOTTOM` 会把窗口压到桌面下面

`SetWindowPos(hwnd, HWND_BOTTOM)` 满足「下方窗口=0」，但它把窗口放到了
`Progman`（桌面窗口）**之下**：`IsWindowVisible` 仍为 true、`windows_below` 仍是 0，
而屏幕上什么都没有。这类「断言通过、用户看不到」的假证据比不做断言更坏。

修法（`src/win/dock.rs::desktop_anchor`）：插入点改取「桌面窗口正上方的那个窗口」——
`GetShellWindow()` 拿到 `Progman`，`GetWindow(Progman, GW_HWNDPREV)` 拿到它上面的一格，
`SetWindowPos` 的语义是「排在这个窗口之后」，于是我们落在 `Progman` 之上、
所有普通窗口之下：看得见，也不挡人。取不到桌面（非 explorer 外壳）时退回
`HWND_BOTTOM`，并保留「不挡任何人」的语义。

新增日志与断言：

```
[info] 启动沉底: ok=true 下方窗口=0 紧邻下方=Progman
```

- 用例「桌面层停在桌面上方」：`紧邻下方` 必须是 `Progman` 或 `WorkerW`。
- 这两条合起来才是完整判据：**上面没人**（下方窗口=0）且 **下面就是桌面**
  （紧邻下方=Progman/WorkerW），即「贴在桌面上、所有普通窗口之下」。

## 设置面板的 9 行（含新增的两项透明度）

`06-settings-panel-9rows.png` 由 `--open-settings --dump-frame` 落盘。面板行序：

1. 背景不透明度（0.00–1.00，步长 0.02）
2. 文字不透明度（0.10–1.00，步长 0.02）
3. 字号 / 4. 行高 / 5. 内边距 / 6. 圆角
7. 粘贴保护 / 8. OSC 52 写入剪贴板 / 9. 失焦沉到桌面层

`↑↓` 选行、`←→` 调值、`Enter` 保存（`config::save_atomic`：`.toml.tmp` + `rename`）、`Esc` 取消。
