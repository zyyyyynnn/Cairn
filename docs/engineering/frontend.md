# 前端与 GPUI 规范


本文是 Cairn 在 `gpui-kit 0.10.0` / `gpui-component 0.10.0` 上验证的工程约束。图标、控件和窗口行为会随依赖版本变化；涉及框架内部实现的项目在升级依赖后必须重新复验。

## 布局与尺寸

1. **BorderBox**：taffy 默认按 border-box 计算；固定高度已经包含边框，不要再次把 1px 细线加进总高度。
2. **rem 单一来源**：`rems()` 依赖 `window.rem_size()`；壳层按 `BASE_FONT_PX = 14px` 和 `zoom` 计算，不要引入 16px rem 基准。所有虚拟化高度和布局常量应走同一换算。
3. **窗口最小尺寸只在创建时读取**：`window_min_size` 没有运行时 setter；需要动态限制时在应用层主动 resize，或按启动时的最坏情况预算。
4. **滚动条不是自动的**：只设置 `.overflow_x_scroll()` 不会画出滚动条；taffy 的 `scrollbar_width` 默认为 0，需要 `ScrollbarMode::Always` 并挂载 `Scrollbar`。所有显式滚动条统一通过 `overlay_scrollbar` 构造，轨道透明且不额外绘制灰底或分隔线。
5. **GPUI 没有内置键盘滚动**：应用层监听按键，调用 `ScrollHandle::set_offset`，用 `max_offset()` 限制边界。
6. **列宽共享模型**：表头与数据行必须读取同一套 flex 比例；否则拖拽列宽后表头与单元格会错位。
7. **CJK 排版**：GPUI 没有避头尾和 CJK 分词；不可换行的文本必须显式分配宽度预算，不能假设会自动处理标点。

## 状态、焦点与输入

8. **`read(cx)` 不要在 `&mut self` 中递归回读**：在实体更新上下文中读取同一实体可能触发不可展开的 panic；需要的字段应由调用方传入。
9. **`on_key_down` 挂在焦点节点**：GPUI 只把按键派发给持有焦点及祖先节点；事件监听器必须放在 `track_focus` 的节点上。
10. **输入模态决定焦点环**：维护 `keyboard_navigation`；鼠标点击时清除键盘焦点环，只保留中性底色。避免行选择导致整表呈现冗余外框。
11. **键盘行的稳定身份**：可键盘导航的行使用稳定的 `FocusHandle` + `use_keyed_state` + 稳定 `ElementId`；在包装层 `track_focus`，并拦截 Enter 的 `OpenSelected` 冒泡。
12. **文本光标绝对定位**：搜索框聚焦光标用 `div().absolute().left_0()` 脱离 flex 流；保证占位符不因光标占位发生位移。
13. **字体等宽元组**：`FontFeatures` 是 `Arc<Vec<(String, u32)>>` 的公开元组，没有 `tabular()` 辅助；用 `("tnum", 1)` 显式构造。

- **焦点恢复**：渲染区域消失导致焦点丢失时，`on_focus_lost` 先回到窗口给出的最近恢复目标；目标不在查询框内时回到查询框，键盘命令始终可达。
- **重命名草稿**：编辑器失焦不丢弃草稿（点击编辑器内部不应清空输入），只有 Escape 取消；写入进行中由草稿独占，第二次 Enter 不会重复提交。
- **结果表入口策略**：`ResultSetChange` 集中决定行与滚动去留——Replace 立即清行并回到顶部，Refresh 在索引追赶期间保留行，Background 与 Probe 保持行和滚动且不淡入，InPlace 保留行但新顺序从顶部开始。只有真正的内容到达才播放入口淡入，Reduce Motion 下直接落位。
- **快捷键播报**：控件注册表（`CONTROL_SHORTCUTS`）是广告快捷键的唯一来源，多个键合并为一个字符串（AccessKit 的 `keyboard_shortcut` 只承载单值）；组件菜单内的快捷键由 keymap 提示，不在调用点重复注册。

## 交互与渲染

14. **Hover 挂在命中区域上**：在交互包装层的普通子节点上增加 `.hover()`，可能不触发；把 hover 样式放在拥有 hitbox 的交互节点上。
15. **鼠标监听器在绘制时注册**：`Window::on_mouse_event` 只接收当前绘制帧注册的监听器；拖拽监听宜在 `canvas` 的绘制回调中注册。
16. **Dialog 不能在窗口创建闭包中打开**：`window.open_dialog(...)` 必须由后续用户动作触发，否则会撞上组件窗口状态未初始化。
17. **Button 的重复 Hover 会 panic**：在主题与全局交互样式中统一配置，不在 `Button` 上重复声明 `.hover()`。
18. **加载态按钮需固定图标**：无图标 `Button::loading(true)` 会动态插入默认 `IconName::Loader` spinner 导致按钮宽度跳动；加载态按钮须同时配置固定 `.icon(...)` 与 `.loading_icon(IconName::LoaderCircle)`。

## 窗口与平台

19. **单实例唤醒匹配产品标题，不匹配共享类名**：GPUI 的窗口类名可能与其他应用共用。当前 Cairn 通过 `EnumWindows` 匹配标题 `Cairn`（或校验进程）后调用 `ShowWindow(SW_RESTORE)`、`BringWindowToTop`、`SetForegroundWindow`；没有活动窗口时允许新实例自愈启动。托盘唤醒走同一路径。
20. **主题切换分两步**：在同一个 `Theme::update` 中同时修改 mode 与修复颜色可能被丢弃；先切 mode，再在下一步修复颜色。
21. **UI 验收依赖真实窗口**：当前应用涉及窗口截图、焦点与输入法的验证在真实窗口中完成；`shell/accessibility.rs` 的控件注册表只保证键、角色与文案自洽，控件是否真正按注册表渲染及窗口是否报告对应角色，依赖真实窗口验收。
22. **Sidebar 的辅助功能边界**：`Sidebar<E>` 不提供独立 a11y 树；不要把它的根节点当成可靠的导航 landmark。需要可访问交互时，由应用层建立显式 role/name/focus 结构。
23. **初始化后固定 LTR 布局方向**：`gpui_kit::init(cx)` 会按系统语言自动初始化 `LayoutDirection`；仅提供 `zh-CN`/`en-US` 界面的应用须在 `init` 后显式调用 `cairn_ui::locale::init_layout_direction(cx)` 固定 `LayoutDirection::Ltr`。
24. **Windows 滚动条外观映射**：在 Windows 上 `Theme::sync_scrollbar_appearance` 将系统自动隐藏滚动条设置映射为 `ScrollbarMode::Hover`（开启自动隐藏）或 `ScrollbarMode::Always`（关闭自动隐藏）。
25. **显式状态机**：搜索匹配模式使用 `QueryMatchMode::{Plain(TextModifiers), Regex}` 表达互斥转换；导航历史使用 `NavHistory` 维护非空历史与游标；工作区位置与结果表面使用 `ActiveLocation::{Index, RecycleBin}`、`LocationProfile` 与 `SurfaceLocation::{Index, LocalListing(LocalListingStage)}` 约束命令、排序回退与列显隐。
26. **异步代次守卫**：`ServiceDomain::probe_generation`、`SubsystemState::generation`（配合 `begin_load`/`finish_load_if_current`/`supersede_load`/`begin_action`）与回收站删除后的 `DeleteRefreshContext` 代次/查询校验隔离并发异步回调，防止过期任务覆盖当前状态；`schedule_delayed_refresh` 仅在服务已连接时调度。
27. **运行时端口与系统边界**：`AppRuntimePorts` 注入设置持久化、全局热键注册与更新状态读取；应用内更新仅接受 `https://` 且命中 GitHub 主机白名单的重定向/下载链路、限制下载体量并通过唯一临时文件 `sync_all()` + `MoveFileExW` 原子落盘；配置持久化采用 `create_new(true)` + `sync_all()` + `MoveFileExW`；定位文件经 Shell COM `SHParseDisplayName` + `SHOpenFolderAndSelectItems` 执行；托盘退出通过 `tokio::sync::watch` 通知主循环调用 `cx.quit()`，窗口查找限定为当前 PID 或校验进程映像路径。

## 壳层布局与动效

28. **布局阶梯**：`app::layout::STEP` 是 4px 的 rem 换算；只有 `CONTROL_HEIGHT`（32px）与 `ROW_HEIGHT`（28px）是独立数值，其余带高与间距都表达为二者的倍数。100% 缩放时得到整数像素，缩放时整体等比换算；新增带高应引入命名关系而不是新字面量。
29. **标题栏固定几何**：标题栏左侧保持 `SIDEBAR_WIDTH`，折叠侧栏只隐藏其下方的树，不移动菜单栏与导航控件。
30. **对齐脊线**：`CONTENT_INSET`（12px）是查询带、表头、行图标和状态摘要的共享左缘；`TIGHT_LEAD`（4px）是外层收紧量，由内部控件的 8px 内边距补足到 12px。搜索带使用与标题栏相同的 chrome 底色，两者读作同一块连续框架。
31. **光学修正**：侧栏标签按字体墨迹中心校正（行框中心不等于表意文字墨迹中心）；图标进入 1rem 盒时通过 1.75 单位描边的侧栏变体保持 0.875rem 时的笔画重量；状态句的 24px 图标动作按测得墨迹中心落座（`STATE_ACTION_OPTICAL_OFFSET`）。
32. **面板圆角遮罩**：GPUI 按矩形裁剪子节点，面板内的表头与行表面会覆盖圆角；`rounded_panel_corners` 只在四角弧线与矩形之间补绘背景色，不参与布局与命中测试。缩放遮罩绘制在内容之上、四角月牙之下。
33. **列宽拖拽**：表头列宽是 flex-grow 比例，指针位移须换算回配置空间（`delta_px * widths.total() / table_width`），否则边界会偏离光标。
34. **顶栏菜单**：触发器在 mouse-down 切换并阻止冒泡，否则 `PopupMenu` 的“外部点击关闭”会与 click 阶段的重开叠加；菜单用 `deferred` + `anchored` 锚定在触发器左下，不用 `Popover`——其模态表面会截走菜单自身的 Up/Down/Enter。
35. **结果表焦点环**：用 `contains_focused` 判断表格是否持有焦点（Tab 会进入区域内部的重试按钮），且仅在键盘模态下绘制，避免指针点击时在行选中之外再加一圈外框。
36. **窗口缩放遮罩**：拖动窗口时内容区覆盖一层实心遮罩，几何静止 `RESIZE_QUIET` 后按 `MASK_DISSOLVE` 淡出。GPUI 在内部模态循环中消费 `WM_EXITSIZEMOVE` 且不重新派发，因此只能在窗口 bounds 观察回调里用计时器判定结束（`WindowResizeTracker`：代数守卫加轮询上限）；观察挂在 `render` 之外，`render` 不修改状态也不启动任务。
37. **启动顺序**：`gpui_kit::init` → `init_layout_direction` → 字体与文本渲染 → `init_keymap` → 应用主题 → `gpui_kit::open_window`。keymap 必须先于任何窗口或菜单读取，`open_window` 让 `Root` 拥有窗口第一层；持久化外观在首帧前安装，窗口直接以用户离开时的缩放与明暗打开。
38. **窗口选项**：以 `TitleBar::window_options()` 为基线；标题仍交给平台（任务栏与 Alt+Tab 读取），`hide_title` 只影响自绘标题栏。默认与最小尺寸同为 1280x800 逻辑像素并居中摆放（左上原点会让状态栏落到任务栏下方）。
39. **对话框页脚**：左取消、右确认；浅色中性按钮按填充亮度判断后向内收缩 1 个设备像素，色度按钮保留整块填充，两种尺寸在视觉上等大。

## 相关代码

- `crates/cairn-app/src/app.rs`、`crates/cairn-app/src/startup.rs`：布局阶梯、窗口尺寸与 `AppRuntimePorts`。
- `crates/cairn-app/src/shell/theme.rs`：字号、主题、等宽数字。
- `crates/cairn-app/src/shell/location.rs`、`crates/cairn-app/src/shell/nav_history.rs`：`ActiveLocation` / `LocationProfile` 与 `NavHistory`。
- `crates/cairn-app/src/search/modifiers.rs`、`crates/cairn-app/src/search/bar_view.rs`：`QueryMatchMode`、搜索框与光标。
- `crates/cairn-app/src/table/columns.rs`、`crates/cairn-app/src/table/surface.rs`：列定义、排序回退与 `SurfaceLocation`。
- `crates/cairn-app/src/shell/menu_popup.rs`、`crates/cairn-app/src/shell/selection_actions.rs`：顶栏弹层与选区动作。
- `crates/cairn-app/src/service/mod.rs`、`crates/cairn-app/src/shell/update.rs`、`crates/cairn-app/src/shell/tray.rs`：`SubsystemState` 代次守卫、原子更新与单实例/托盘退出。
- `crates/cairn-app/src/shell/accessibility.rs`：可观察控件注册。

---

## 搜索输入与 IME

本节记录搜索框编辑、焦点模态与 IME 光标的当前行为。

### 1. 编辑快捷键

| 操作 | 快捷键 |
| --- | --- |
| 撤销 | `Ctrl+Z` |
| 重做 | `Ctrl+Y` / `Ctrl+Shift+Z` |
| 剪切 | `Ctrl+X` / `Shift+Delete` |
| 复制（有查询选区时复制查询文本；否则复制选中项完整路径） | `Ctrl+C` / `Ctrl+Insert` |
| 粘贴 | `Ctrl+V` / `Shift+Insert` |
| 全选（结果区已加载行 / 搜索框文本） | `Ctrl+A` |
| 清空搜索 | `Escape` |
| 按词删除 | `Ctrl+Backspace` / `Ctrl+Delete` |
| 按词移动光标 | `Ctrl+Left` / `Ctrl+Right` |

“编辑”菜单同步暴露撤销、重做、剪切、复制、粘贴、全选与清空，中英文文案保持一致。结果区全选只覆盖已加载行，上限 1,000 项；一次“打开所在文件夹”最多打开 8 个目录，超出时拒绝并提示缩小选择范围。

### 2. Undo/Redo 语义

- `SearchInputState` 保存最多 **64** 个 `InputSnapshot`，每个快照包含 `text` 与 `selection`。
- 连续单字符输入在 **500ms** 内合并为一个快照。
- 粘贴、剪切、清空、整段删除和词级删除立即压栈。
- 新编辑会清空 redo 分支；撤销/重做同时恢复选区。
- 粘贴文本会先把 CRLF/LF 规整为空格，避免多行内容打断单行查询框。

### 3. 焦点、光标与 IME

- **焦点环表现**：焦点环仅在 `keyboard_navigation` 为真（Tab/方向键）时显示；鼠标点击激活时侧边栏与表格以中性底色表达当前选中态。
- **四层绝对定位分层渲染**：
  - 底层（选区高亮）：有选区时按文本布局 `layout.x_for_index` 绘制背景矩形；
  - 中下层（IME 组合线）：跟踪 `marked_range` 标记拼音组合区间，下划线贴底呈现；
  - 中上层（文本主体流）：单行不折行文本流式排版；
  - 顶层（悬浮 Caret）：仅在聚焦且无选区时绝对定位悬浮绘制，脱离 flex 常规流，保证文本排版宽度不受光标影响。
- **鼠标划选与分级选择模型**：
  - 单击按指针 X 坐标对齐字符边界折叠光标；Shift+单击基于锚点扩展选区；
  - 双击按 `select_word_at` 精确切分词元（CJK 字符、英数单词、连续标点、连续空白各为独立单元）；三击全选；
  - 划选拖拽期间在窗口顶层挂载全屏透明 canvas 遮罩，捕获全局鼠标移动与释放事件，确保滑出输入框依然平滑选择并在松开时光标与锚点安全复位。
- **光标导航性能不变量**：方向键（Left/Right/Home/End）及 Shift/Ctrl 组合移动光标仅重绘光标与选区（`cx.notify()`），后台查询仅在文本内容变更时触发，纯光标位移保持查询结果稳定。
- **输入法映射**：中文输入法组合文本通过 `marked_range` 跟踪，组合期间精准维护替换区间、候选窗屏幕定位与 UTF-16/UTF-8 选区映射。
- **按键派发**：搜索框的 `on_key_down` 挂在持有 `track_focus` 的节点上。
- **自绘查询框**：查询框不使用 `Input` 组件——平台 IME 通过 `impl EntityInputHandler for CairnApp` 驱动它（SPEC SM-3），两个输入所有者会拆分 UTF-16/UTF-8 映射或丢候选文本；主题表面、rem 几何、焦点样式与搜索图标仍取自组件 tokens。

### 4. 验证位置

- `crates/cairn-app/src/search/input.rs`：快照栈、词边界、IME 范围替换及单测。
- `crates/cairn-app/src/shell/query_edit.rs`：撤销/重做/剪切/粘贴动作与焦点恢复。
- `crates/cairn-app/src/shell/commands.rs`：全局键位与必选绑定。
- `crates/cairn-app/src/main.rs`：单实例唤醒、托盘。
- `crates/cairn-app/src/shell/sidebar_view.rs`：焦点模态与键盘导航。
