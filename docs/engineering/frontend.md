# 前端与 GPUI 规范

> GPUI 与搜索框/窗口行为；框架行为按锁定版本复验。当前应用未接入无头 UI 测试，窗口截图验收必须在真实窗口完成。

本文是 Cairn 在 `gpui-kit 0.8.0` / `gpui-component 0.8.0` 上验证的工程约束。图标、控件和窗口行为会随依赖版本变化；涉及框架内部实现的项目在升级依赖后必须重新复验。

## 布局与尺寸

1. **BorderBox**：taffy 默认按 border-box 计算；固定高度已经包含边框，不要再次把 1px 细线加进总高度。
2. **rem 单一来源**：`rems()` 依赖 `window.rem_size()`；壳层按 `BASE_FONT_PX = 14px` 和 `zoom` 计算，不要引入 16px rem 基准。所有虚拟化高度和布局常量应走同一换算。
3. **窗口最小尺寸只在创建时读取**：`window_min_size` 没有运行时 setter；需要动态限制时在应用层主动 resize，或按启动时的最坏情况预算。
4. **滚动条不是自动的**：只设置 `.overflow_x_scroll()` 不会画出滚动条；taffy 的 `scrollbar_width` 默认为 0，需要 `ScrollbarMode::Always` 并挂载 `Scrollbar`。
5. **GPUI 没有内置键盘滚动**：应用层监听按键，调用 `ScrollHandle::set_offset`，用 `max_offset()` 限制边界。
6. **列宽共享模型**：表头、数据行、骨架屏必须读取同一套 flex 比例；否则拖拽列宽后表头与单元格会错位。
7. **CJK 排版**：GPUI 没有避头尾和 CJK 分词；不可换行的文本必须显式分配宽度预算，不能假设会自动处理标点。

## 状态、焦点与输入

8. **`read(cx)` 不要在 `&mut self` 中递归回读**：在实体更新上下文中读取同一实体可能触发不可展开的 panic；需要的字段应由调用方传入。
9. **`on_key_down` 挂在焦点节点**：GPUI 只把按键派发给持有焦点及祖先节点；事件监听器必须放在 `track_focus` 的节点上。
10. **输入模态决定焦点环**：维护 `keyboard_navigation`；鼠标点击时清除键盘焦点环，只保留中性底色。否则选一行就给整张表加外框。
11. **键盘行的稳定身份**：可键盘导航的行使用稳定的 `FocusHandle` + `use_keyed_state` + 稳定 `ElementId`；在包装层 `track_focus`，并拦截 Enter 的 `OpenSelected` 冒泡。
12. **文本光标绝对定位**：搜索框聚焦光标用 `div().absolute().left_0()` 脱离 flex 流；否则 placeholder 会因光标占位发生横向位移。
13. **字体等宽元组**：`FontFeatures` 是 `Arc<Vec<(String, u32)>>` 的公开元组，没有 `tabular()` 辅助；用 `("tnum", 1)` 显式构造。

## 交互与渲染

14. **Hover 挂在命中区域上**：在交互包装层的普通子节点上增加 `.hover()`，在现有版本中可能不再触发；把 hover 样式放在拥有 hitbox 的交互节点上。
15. **鼠标监听器在绘制时注册**：`Window::on_mouse_event` 只接收当前绘制帧注册的监听器；拖拽监听宜在 `canvas` 的绘制回调中注册。
16. **Dialog 不能在窗口创建闭包中打开**：`window.open_dialog(...)` 必须由后续用户动作触发，否则会撞上组件窗口状态未初始化。
17. **Button 的重复 Hover 会 panic**：不要在 `Button` 上重复声明 `.hover()`；Ghost 的 hover 通过主题/全局交互样式配置。
18. **SidebarMenuItem 高度**：组件在 `refine_style` 后会把高度固定为 `h_7`；外部 `.h()` 可能被吞掉，需要改用 `min-height` 约束。

## 窗口与平台

19. **单实例唤醒匹配产品标题，不匹配共享类名**：GPUI 的窗口类名可能与其他应用共用。当前 Cairn 通过 `EnumWindows` 匹配标题 `Cairn`（或校验进程）后调用 `ShowWindow(SW_RESTORE)`、`BringWindowToTop`、`SetForegroundWindow`；没有活动窗口时允许新实例自愈启动。托盘唤醒走同一路径。
20. **主题切换分两步**：在同一个 `Theme::update` 中同时修改 mode 与修复颜色可能被丢弃；先切 mode，再在下一步修复颜色。
21. **UI 验收没有 headless 捷径**：当前应用未接入无头窗口测试；涉及窗口截图、焦点与输入法的验证必须在真实窗口中完成，并使用最新屏幕捕获。
22. **Sidebar 的辅助功能边界**：`Sidebar<E>` 不提供独立 a11y 树；不要把它的根节点当成可靠的导航 landmark。需要可访问交互时，由应用层建立显式 role/name/focus 结构。

## 相关代码

- `crates/cairn-app/src/app.rs`：布局阶梯、窗口尺寸。
- `crates/cairn-app/src/theme.rs`：字号、主题、等宽数字。
- `crates/cairn-app/src/search_bar_view.rs`：搜索框与光标。
- `crates/cairn-app/src/main.rs`：单实例唤醒与托盘。
- `crates/cairn-app/src/accessibility.rs`：可观察控件注册。

---

## 搜索输入与窗口唤醒

本文记录搜索框编辑、焦点模态、IME 光标和单实例窗口唤醒的当前行为。

## 1. 编辑快捷键

| 操作 | 快捷键 |
| --- | --- |
| 撤销 | `Ctrl+Z` |
| 重做 | `Ctrl+Y` / `Ctrl+Shift+Z` |
| 剪切 | `Ctrl+X` / `Shift+Delete` |
| 复制选中文本 | `Ctrl+C` / `Ctrl+Insert` |
| 粘贴 | `Ctrl+V` / `Shift+Insert` |
| 全选搜索文本 | `Ctrl+A` |
| 清空搜索 | `Escape` |
| 按词删除 | `Ctrl+Backspace` / `Ctrl+Delete` |
| 按词移动光标 | `Ctrl+Left` / `Ctrl+Right` |

“编辑”菜单同步暴露撤销、重做、剪切、复制、粘贴、全选与清空，中英文文案保持一致。

## 2. Undo/Redo 语义

- `SearchInputState` 保存最多 **64** 个 `InputSnapshot`，每个快照包含 `text` 与 `selection`。
- 连续单字符输入在 **500ms** 内合并为一个快照。
- 粘贴、剪切、清空、整段删除和词级删除立即压栈。
- 新编辑会清空 redo 分支；撤销/重做同时恢复选区。
- 粘贴文本会先把 CRLF/LF 规整为空格，避免多行内容打断单行查询框。

## 3. 焦点、光标与 IME

- 焦点环仅在 `keyboard_navigation` 为真（Tab/方向键）时显示；鼠标点击后侧边栏和表格只保留中性底色，不画 1px 外框。
- 搜索框获得焦点时，caret 用绝对定位悬浮，不参与 flex 布局；placeholder 在聚焦/失焦时不发生横向位移。
- 中文输入法组合文本通过 `marked_range` 跟踪；组合期间的替换、候选窗定位与 UTF-16/UTF-8 选区换算都要保持不丢字、不重复上屏。
- 搜索框的 `on_key_down` 必须挂在持有 `track_focus` 的节点上。

## 4. 单实例与托盘唤醒

当前实现通过 `EnumWindows` 查找标题为 `Cairn` 的顶层窗口；不要只按 GPUI 的共享窗口类名匹配，否则可能激活其他 GPUI 应用。

- 找到窗口：`ShowWindow(SW_RESTORE)` → `BringWindowToTop` → `SetForegroundWindow`。
- 找不到活动窗口（互斥体存在但窗口已消失）：允许新实例启动，避免用户被困在无窗口状态。
- 托盘“显示 Cairn”和托盘双击复用同一唤醒函数。
- 第二次启动 `cairn.exe` 不应并发创建第二个主窗口；这也会让截图/自动化误连到旧进程。

## 5. 验证位置

- `crates/cairn-app/src/input.rs`：快照栈、词边界、IME 范围替换及单测。
- `crates/cairn-app/src/menu_actions.rs`：撤销/重做/剪切/粘贴动作与焦点恢复。
- `crates/cairn-app/src/commands.rs`：全局键位与必选绑定。
- `crates/cairn-app/src/main.rs`：单实例唤醒、托盘。
- `crates/cairn-app/src/sidebar_view.rs`：焦点模态与键盘导航。
