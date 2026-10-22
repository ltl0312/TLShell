# M6 视觉证据（逐像素 alpha 渲染路线）

M6 把绘制通路从 `softbuffer` + `SetLayeredWindowAttributes(LWA_ALPHA)` + `SetWindowRgn`
换成 `CreateDIBSection` + `UpdateLayeredWindow(ULW_ALPHA)`，逐像素 alpha 由程序自己算。

## 证据获取方式（可复现）

窗口常驻最底层，屏幕区域通常被其它窗口盖住，截屏既不稳定也不可信。
所以证据一律取**推给 `UpdateLayeredWindow` 的同一份像素**，再以白底合成落成 BMP：

```powershell
$env:TLSHELL_LOG='debug'
.\tlshell.exe --probe-input --dump-frame %TEMP%\frame.bmp
# 1500ms 后落盘，日志出现「首帧已落盘」
```

白底合成是**最差条件**：整窗 alpha 0.88 下浅色背景会把深色文字的对比度压到最低。

## 帧清单

| 文件 | 内容 | 对应验收项 |
|---|---|---|
| `01-text-selection-color-130dpi-white.png` | 130 DPI、白底、默认主题，含提示条与选区下沿 | V-02 V-19 V-38 |
| `02-selection-drag-130dpi-white.png` | 拖选状态（`WM_MOUSEMOVE` 后不抬键），选区跨 20 列 | V-38 |
| `03-elevated-ring-130dpi-white.png` | 提权态：边框内侧两像素警示圈常驻 | S-01 |

## 像素实测（`System.Drawing` 逐点取值，与解析解一致）

| 位置 | 实测 RGB | 解析期望 | 结论 |
|---|---|---|---|
| 空白背景 `#1e1e2e` @ α0.88 叠白 | 57, 57, 71 | 57.2, 57.2, 71.1 | 一致 |
| 提示条底色 `#585b70` @ α0.88 叠白 | 108, 111, 129 | 108.1, 111.0, 129.0 | 一致 |
| 选区下沿 `#89b4fa` @ α0.88 叠白 | 151, 189, 251 | 151.5, 189.3, 250.6 | 一致 |
| 提权警示圈 `#f9e2af` @ α0.88 叠白（四边各取一点） | 250, 230, 185 | 249.7, 229.5, 184.6 | 一致 |
| 非提权态同位置（回归对照） | 57, 57, 71 | 57.2, 57.2, 71.1 | 一致，说明圈不会被误画 |

## 本轮发现并修复的整窗缺陷：通道序

修复前同一位置的实测是（129, 111, 108）——正好是期望值的镜像。
原因：`win::window::parse_color` 返回的是 GDI 的 **COLORREF（0x00BBGGRR）**，
而 `Theme::from_config` 直接把它当 DIB 像素（**0x00RRGGBB**）用，
于是整窗所有颜色 R/B 颠倒：蓝灰 Catppuccin 主题被渲染成棕灰。

- 修法：新增 `win::window::parse_rgb` 专供绘制路径，`parse_color` 委托它再转 COLORREF。
- 防回归：`win::window::tests::parse_rgb_keeps_rrggbb_order`、
  `render::theme::tests::theme_colors_are_in_rrggbb_order`、
  `win::surface::tests::dib_channel_order_matches_windows_expectation`
  （最后这条用 `GetPixel` 回读 DIB，证明写入路径本身是对的，缺陷只在解析）。

## 逐项状态

| 项 | 状态 | 依据 |
|---|---|---|
| V-02 文字不被整窗 alpha 稀释 | 通过 | 文字与背景各自合成到 alpha 平面，实测三处颜色与解析解一致 |
| V-19 圆角抗锯齿 | 通过 | 角部 4×4 超采样；`surface::tests::corner_is_antialiased_not_hard` 断言存在 `0 < coverage < 1` 的过渡像素，且每格落在 (0.02, 0.98)。`SetWindowRgn` 硬边路径已删除 |
| V-38 白底选区可辨识 | 通过 | 选区底色 + 强调色下沿双线索；`theme::tests::selection_edge_stays_visible_on_any_wallpaper` 在 α∈{0.88,0.70} × 壁纸∈{白,黑} 上断言下沿对比度 ≥ 3:1，反向测试 `default_selection_fill_alone_splits_the_extremes_badly` 证明只看底色做不到 |
| V-30 退出码状态行 | 通过 | `src/app.rs` 状态行常驻显示退出码，子进程退出不关窗，`Ctrl+Shift+R` 重开 |
| V-32 错误显示到界面 | 通过 | `show_terminal_error` 把启动失败/shell 解析失败写进终端区，不只剩日志 |
| S-01 提权态常驻线索 | 通过 | 边框内侧两像素警示圈（`painter::draw_elevation_ring`），有像素级测试与真机采样；非提权态不画（同一位置实测为背景色） |

## 未验证（诚实标注）

- 深色壁纸实采样与录屏类对比度条目：本机无录屏工具链，未切深色壁纸实采。
- `opacity` < 0.70 的可读性：按规范低于下限，`OPACITY_WARN_BELOW` 会告警但不阻止；
  V-38 的下沿保证在 0.10 下不成立（实测 1.14:1），已在测试注释里写明。

## 提权态的验证办法（没有 UAC 也能测）

本机没有自动提权手段（`asInvoker`，且自动提权等于替用户提权）。为了不让这条视觉线索
"写完就算"，debug 构建留了一个只读开关：

```powershell
$env:TLSHELL_FORCE_ELEVATED='1'
.	lshell.exe --dump-frame %TEMP%\elevated.bmp
```

它只把「按提权态渲染」这一个外观判定打开，不改变任何权限（进程仍是 `asInvoker` 的中等完整性）。
release 构建里这个分支不存在（`cfg!(debug_assertions)`），日志会留一行
`TLSHELL_FORCE_ELEVATED 生效：按提权态渲染（仅调试构建）`。
