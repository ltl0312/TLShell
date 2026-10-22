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
