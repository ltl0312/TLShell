# M9：标签栏几何、悬停反馈、shell 切换、回滚与字号缩放

本轮针对用户报告（m04567）的六条问题各做一次迭代，每条都留下可核对的证据。
凡是「只能真人验」的，都在最后一节写明，不假装自动化验过。

## 迭代 1：选区丢首字符

**症状**：从右往左拖选 `PS C:\Users\ZhuanZ>`，复制出来是 `S C:\Users\ZhuanZ>`——第一个 `P` 选不中。

**根因**：上游 `alacritty_terminal` 的 `range_simple` 在「起点锚在格子右半边」时会把
`start.column` 加一，而我们的拖选起点正是用 `Side::Right` 更新的，于是反向拖选必然吃掉首格。

**修法**：不再用上游 `Selection`，`src/term/grid.rs` 自己存 `sel_start`/`sel_end` 两个网格点，
`selection_range()` 自己做 normalize（交换 + 相等返回 `None`），`selection_text()` 自己逐行取格
（跳过 `WIDE_CHAR_SPACER`，`'\0'` 当空格，行尾 trim，行间 `\n`）。

**证据**：`cargo test --offline grid` → 17 passed，其中
`selection_keeps_the_first_cell_when_dragging_backwards` 断言
`Column(10) → Column(0)` 得到 `"PS C:\\Users"`。
（写这条测试时先错写成 `Column(9)`，得到 `"PS C:\\User"`——`PS C:\Users` 是 11 个字符。）

## 迭代 2：清屏保留提示符

**症状**：点「清屏」后连 `PS C:\Users\ZhuanZ>` 一起消失，屏幕全黑像卡死。

**根因**：`clear_screen` 无条件发 `CSI 3J CSI 2J CSI H` 清网格，shell 并不知道要重画提示符。

**修法**：`Tab` 记 `shell_kind`；PowerShell（PSReadLine）/bash 走 `Ctrl+L`（`0x0C`）让 shell 自己
清屏并重画提示符；只有 `cmd.exe` 不认 `Ctrl+L`，才退回本地清网格。

**证据**：日志 `清屏: 已向 shell 发送 Ctrl+L（由 shell 重画提示符）` /
`清屏: cmd 不认 Ctrl+L，改为本地清网格`；`verify-tabs.ps1` 的「点清屏按钮」判据断言前者。

## 迭代 3：标签栏排布错乱

**症状**：标签标题与 `×` 压在一起（截图里是 `1shel/ 1|`），按钮跟着最后一个标签往后跑，
标签一多就被挤出窗口。

**根因**：`tab_width = 标题字符数 × cell_w + 16`，而标题从 `start + 8` 起画、叉号中心在
`start + width - 8`——留给叉号的只有 8px，标题末端必然压上去；按钮位置由标签数量决定，
没有考虑窗口宽度。

**修法**：`src/render/layout.rs` 重写几何。

| 量 | 值 |
|---|---|
| `TAB_PAD_LEFT` | 8 |
| `TAB_CLOSE_W` | 18 |
| `TAB_GAP` | 4 |
| 标签宽 | `TAB_PAD_LEFT + 标题宽 + TAB_CLOSE_W + 6` |
| 叉号中心 | `start + 宽 - 6 - TAB_CLOSE_W/2` |

按钮改为**右对齐**、与标签流互不相干：`清屏` 右边缘留 10px，`shell` 按钮在它左侧，
`+` 再左侧，`clip_end` 是标签可画到的右界——放不下的标签直接不画，绝不压到按钮上。

**证据**（900×420、单元格 11×24、DPI 130，逐像素投影，亮度阈值 430）：

```
标签栏: 单元格=11 栏高=30 新建中心=798 shell中心=834 清屏中心=879 标签数=1 可绘右界=786
标签[0]: shell 1 起=4 止=113 叉=98
投影: 标题 (14,21)(25,65)(80,87) 叉号 (94,102) + (794,802) ps5 (820..849) 清屏 (869..899)
```

标题最后一个字结束于 x=87，叉号从 94 开始，间隔 7px；按钮两两相距 ≥30px。
截图：`docs/aegis/evidence/vis/M8/M8-01-tab-bar.png`（同几何的单标签栏）。
两个标签时的排布只由上面这行日志判定（`标签[1]` 的 `起` 必须 ≥ `标签[0]` 的 `止`），
没有单独的截图——本机无法稳定地在首帧落盘前注入第二次点击。

## 迭代 4：悬停反馈与 shell 类型按钮

**悬停**：`WM_MOUSEMOVE` 算命中格，变化时重绘；首次登记 `TrackMouseEvent(TME_LEAVE)`，
`WM_MOUSELEAVE` 时清空悬停。悬停格用前景色 30/40 alpha 叠一层，按钮同样。
日志 `悬停: Clear` 走 `info` 级——`debug` 在验收脚本的 `TLSHELL_LOG=info` 下不落盘。

**shell 按钮**：点一下按 `pwsh → powershell → cmd` 循环，用 `shell::resolve` 逐个试探，
成功才写回 `config.toml` 并重启当前标签的会话（旧 `Session` 丢后台线程 drop，
不在 UI 线程 join），提示条显示 `shell 已切换为 <program>`。

**证据**：`verify-tabs.ps1` 的「悬停清屏有反馈」「点 shell 按钮切换类型」两项 PASS。

## 迭代 5：历史回滚与指示器

- 滚轮：`delta/120*3` 行进历史。
- `Shift+PageUp` / `Shift+PageDown` / `Shift+Home` / `Shift+End`：整屏/首尾（裸 `PageUp`
  仍交给 shell，不抢 readline 的键）。
- 右侧 3px 滚动条：滑块高 `max(18, 轨道高 × 0.15)`，位置按 `offset/history` 映射，
  只在 `offset > 0` 时出现。
- 日志 `回滚: 偏移=N 历史=M`。

**证据**：`verify-tabs.ps1` 第 9 项先敲 `1..80|%{$_}` 造出 67 行历史，再投 `WM_MOUSEWHEEL`
一格，断言 `回滚: 偏移=3 历史=67`（偏移 > 0 且历史 > 0）。

## 迭代 6：字号缩放

`Ctrl+=` 加一档、`Ctrl+-` 减一档、`Ctrl+0` 复位；步长 1px，夹在 `[6, 72]`
（与 `sanitize()` 共用 `FONT_SIZE_MIN`/`FONT_SIZE_MAX`），改完立刻重新加载字体、重排网格、
原子写回配置；写回失败时提示条明说「已生效，但没写回配置」。

**证据**：`cargo test --offline font_zoom` → 3 passed
（`one_step_moves_exactly_one_pixel`、`zoom_stops_at_the_configured_bounds`、
`bounds_match_the_sanitizer`）。

## 自动化验收

`scripts/verify-tabs.ps1` 现在 **9 项全 PASS、exit=0**（几何全部从程序日志解析，
脚本不再自己算宽度——上一版正是自己算，几何一改就点错）：

```
[PASS] 点 + 新建标签          [PASS] 点标签切换
[PASS] 点清屏按钮             [PASS] 点 × 关闭标签
[PASS] 关闭后新建复用序号     [PASS] 关到空自动补一个
[PASS] 悬停清屏有反馈         [PASS] 点 shell 按钮切换类型
[PASS] 滚轮回滚进历史
```

`scripts/run-all.ps1` 的 D 层用例随之改名为「标签栏与回滚可用」，要求 PASS ≥ 9 且 FAIL = 0。

## 只能真人验的部分（不掩饰）

1. **带修饰键的快捷键**：`Ctrl+=` / `Ctrl+-` / `Ctrl+0` / `Shift+PageUp` 等都靠
   `GetAsyncKeyState` 读物理修饰键状态，`PostMessage` 造不出真实修饰键状态，因此
   自动化只覆盖到「编码函数与边界」这一层，真实按键需要人工按一遍。
2. **标签栏不会折行也不会横向滚动**：标签多到放不下时后面的标签直接不画
   （几何上不压按钮），要看到它们得先关掉前面的。
3. **标签不能拖拽重排**，也**不能重命名**。
4. **关闭标签后顶上来的 `shell 1` 不带历史**：它是新起的会话。
