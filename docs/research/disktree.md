# disktree 参考研究：Treemap 配色、布局与视觉交互

> 研究对象：GitHub [`tobi/disktree`](https://github.com/tobi/disktree)，固定版本 `v0.11.0`，commit [`6c8d4ce6211bf135ff4e42890faebff1040e1846`](https://github.com/tobi/disktree/commit/6c8d4ce6211bf135ff4e42890faebff1040e1846)（MIT License）。Cairn 当前 Treemap 规范以 [product/treemap.md](../product/treemap.md) 与 [SPEC.md](../SPEC.md) 为准。

---

## 0. 结论摘要

disktree 的 Treemap 配色是一套**先约束视觉噪声、再表达语义**的系统：

1. **一个类别一个色相，所有彩色类别共用同一饱和度与明度**；深度只改变明度，不改变色相。这样“面积大”不会自动变成“颜色响”，类别之间可比较。
2. **颜色只回答“这是什么”，斜线纹理回答“能不能删”**；两者是独立通道，互不覆盖。
3. **只保留一个强色（主题 warning 琥珀）**给选中、主操作和可回收空间；类别色永远不进入这个强调位。
4. **标题带、标签裁剪、轮廓绘制顺序、缩放进入**都有明确的层级规则，避免文字压住子块、选中环被子块盖住。

---

## 1. 项目定位、技术栈与数据模型

### 1.1 定位

README 的首句是：“Find what is filling a disk, mark what should go, and remove it — with the volume's free space in view the whole time.” 它面向 **Omarchy** 桌面，扫描主目录或整块磁盘，用嵌套 Treemap 展示真实磁盘占用，支持标记、复核、移入回收站或永久删除。来源：[`README.md#L5-L16`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/README.md#L5-L16)。

它不是通用文件管理器，而是“**清理决策工具**”：空间、可回收量、删除风险是产品中心；Treemap 是决策界面，不是装饰图表。

### 1.2 技术栈与运行平台

| 项 | 事实 | 来源 |
| --- | --- | --- |
| 语言/版本 | Rust 1.97，edition 2024 | [`Cargo.toml#L1-L20`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/Cargo.toml#L1-L20) |
| UI | GPUI，经 `gpui-kit = 0.6.6` 与 `gpui-omarchy = 0.1.3` | [`crates/disktree-app/Cargo.toml#L16-L18`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/Cargo.toml#L16-L18) |
| 平台 | Linux（Omarchy、Wayland/X11、Vulkan）、macOS 11+、Windows 10/11 | [`README.md#L18-L125`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/README.md#L18-L125) |
| 版本 | workspace `0.11.0`，MIT | [`Cargo.toml#L5-L10`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/Cargo.toml#L5-L10) |
| 规模示例 | README 截图显示 881 GiB、3.9M files、499.4K dirs | [`assets/screenshot.png`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/assets/screenshot.png) |

关键差异：disktree 与 Cairn 同为 Rust + GPUI，但 disktree 用 **gpui-kit 0.6.6 + gpui-omarchy**，Cairn 用 **gpui-kit 0.8.0 + gpui-component Theme**；两者控件 API、主题模型、渲染路径都不同，不能直接移植代码。

### 1.3 目录结构

| 路径 | 职责 | 来源 |
| --- | --- | --- |
| `crates/disktree-core` | 扫描、树、squarified 布局、空间与删除规则，无 UI | [`README.md#L401-L409`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/README.md#L401-L409) |
| `crates/disktree-core/src/tree.rs` | `Node`、`Metric`、聚合、硬链接去重 | [`tree.rs#L29-L90`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-core/src/tree.rs#L29-L90) |
| `crates/disktree-core/src/classify.rs` | 目录类别、可回收原因 | [`classify.rs#L21-L70`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-core/src/classify.rs#L21-L70) |
| `crates/disktree-core/src/treemap.rs` | squarified 布局、嵌套 header、`Others` 合并 | [`treemap.rs#L105-L317`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-core/src/treemap.rs#L105-L317) |
| `crates/disktree-app/src/palette.rs` | 类别色、年龄色、强调色、hatch | [`palette.rs#L1-L174`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/palette.rs#L1-L174) |
| `crates/disktree-app/src/treemap_view.rs` | 绘制 tile、轮廓、标签、tooltip 锚点 | [`treemap_view.rs#L1-L539`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/treemap_view.rs#L1-L539) |
| `crates/disktree-app/src/state.rs` | 全部动作、键位、视图变换、过渡动画 | [`state.rs#L119-L283`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/state.rs#L119-L283) |

### 1.4 数据模型

`Node` 的字段（[`tree.rs#L63-L90`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-core/src/tree.rs#L63-L90)）：

- `name`、`kind`（Directory/File/Symlink/Other）
- `bytes`、`files`、`dirs`：子树聚合值；叶子只存自身
- `inode`：硬链接去重
- `read_error`：不可读标记
- `modified`：子树最新写入时间，用于 Age 模式
- `category: Category`、`reclaim: Option<Reclaim>`
- `children: Vec<Node>`，按当前 `Metric` 降序

`Metric` 只有 `Bytes` 与 `Files` 两种；Age 不改变面积，只改变颜色（[`README.md#L144-L151`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/README.md#L144-L151)）。这使“面积=大小、颜色=年龄”不会互相打架。

### 1.5 类别从哪来：目录名/形状，而不是扩展名

`Category` 有 9 个值：`Code`、`AgentScratch`、`Toolchain`、`Synced`、`Git`、`Media`、`Documents`、`Cache`、`Other`（[`classify.rs#L21-L70`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-core/src/classify.rs#L21-L70)）。分类依据是：

- 目录名的短表查找：`src`/`code`/`projects` → Code，`.cache`/`node_modules` → Cache，`Pictures`/`Music`/`SteamLibrary` → Media，等等；
- 形状判断：含 `objects`、`refs`、`HEAD` 的目录视为 Git 仓库；
- 顶层未知目录取“最大可识别子目录”的类别（`dominant_child_category`）；
- 文件**继承父目录**的类别，单独文件不按扩展名分类。

来源：[`classify.rs#L130-L181`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-core/src/classify.rs#L130-L181)、[`classify.rs#L229-L314`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-core/src/classify.rs#L229-L314)。

---

## 2. Treemap 布局、标签、缩放、交互与动画

### 2.1 布局：squarified + 嵌套 header + 尾部合并

核心算法是 Bruls、Huizing、van Wijk 的 squarified treemap：按面积降序，逐行扩展，直到最差宽高比不再改善。实现位于 [`treemap.rs#L322-L394`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-core/src/treemap.rs#L322-L394)。

布局参数（[`treemap.rs#L105-L139`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-core/src/treemap.rs#L105-L139)）：

| 参数 | 默认值 | 作用 |
| --- | --- | --- |
| `max_depth` | 3（界面 1–6，`[` `]` 调整） | 一次画几层 |
| `padding` | 1.0 px | 兄弟块之间、目录内缘 |
| `padding_outer` | 3.0 px | 顶层目录之间更宽的分隔 |
| `min_tile` | 5.0 px | 小于此宽度/高度不画，不能读也不能点 |
| `max_children` | 96 | 每个目录最多保留 96 个直接子项，尾部合并为 `Others` |
| `header` | 20.0 px | 顶层目录名称带 |
| `header_inner` | 15.0 px | 更深目录的窄名称带 |

嵌套规则（[`treemap.rs#L187-L317`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-core/src/treemap.rs#L187-L317)）：

1. 目录先按面积分到矩形；
2. 若目录还会细分，先在顶部预留一条 header band，子项在 band 下方布局；
3. header 是父 tile 的一部分，命中测试时点 band 也选中父目录，父目录不需要精确点边框；
4. 如果宽度 < 44 px 或 band 下方剩余高度 < 3×`min_tile`，**放弃细分**，整个目录作为一个块。代码注释的理由是：“a name drawn over its own children is worse than one level less of detail”。
5. 超过 `max_children` 的尾部合并为一个 `Others` tile，面积仍计入，不会静默丢失。

### 2.2 标签：裁剪到自己的区域，按屏幕空间取舍

标签不是独立悬浮层，而是画在 tile 自己的 header band 或左上角，并用 `ContentMask` 裁到该区域，防止文字渗到邻居（[`treemap_view.rs#L389-L539`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/treemap_view.rs#L389-L539)）。

具体规则：

- 名称字号 `0.75 rem`，尺寸字号 `0.6875 rem`；内边距、阈值、间距都按字号的比例算，所以界面缩放时关系不变；
- 顶层目录的名称在 header band 内**加粗**，因为它命名一个区域；更深层用常规字重；
- 标签最小宽度 `3.375 rem`、最小高度 `0.9375 rem`，不够就不画；
- 每帧最多 shape 150 条标签（`MAX_LABELS`），按 tile 面积从大到小取，先保证大块可读；
- 尺寸文本在顶层 band 里靠右，形成一列可比较的数字；更深层跟在名称后面，或在足够高的闭合 tile 里另起一行；
- 标签颜色：深色主题用 `theme.bright`，浅色用 `theme.foreground`；深度 > 0 时透明度 `0.88`。

来源：[`state.rs#L1847-L1893`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/state.rs#L1847-L1893)、[`state.rs#L3013-L3029`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/state.rs#L3013-L3029)、[`state.rs#L3161-L3162`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/state.rs#L3161-L3162)。

**风险点**：`0.88` 透明度会降低文字与填充的对比度。按源码公式复算，Tokyo Night 深色主题下 Toolchain 深度 3 的标签对比度约 **4.28:1**，Cache 约 **4.38:1**，低于 WCAG AA 的 4.5:1。Cairn 不照搬该透明度，保持数据面标签不透明。

### 2.3 缩放与下钻：先放大到“目录刚好填满”，下一格才进入

视图变换是唯一的缩放状态：

```
screen = (base - origin) * scale
```

布局在未缩放的 base 像素空间里算一次并缓存；缩放/平移只改 `scale/origin`，不重新布局。来源：[`state.rs#L119-L212`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/state.rs#L119-L212)、[`AGENTS.md#L101-L102`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/AGENTS.md#L101-L102)。

### 2.4 悬停、选中、标记：三种轮廓，按重要性排序

绘制顺序：

1. 先画所有 tile 填充；
2. 再画 hatch（可回收斜线）；
3. 再画顶层 2px 色条、不可读角标；
4. **最后**画所有轮廓，并按优先级排序：选中（rank 3，2px 琥珀）> 悬停（rank 2，1px 亮色）> 标记（rank 1，2px danger）。

来源：[`treemap_view.rs#L249-L366`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/treemap_view.rs#L249-L366)、[`treemap_view.rs#L369-L382`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/treemap_view.rs#L369-L382)、commit [`75d657d`](https://github.com/tobi/disktree/commit/75d657d57a227f978d6a8936bb219db963a33717)。

### 2.5 动画：区域连续变形

进入/退出一层时，disktree 记录被进入目录在旧帧与新帧中的屏幕矩形，按 cubic ease-out 插值，时长 `130 + 45 × log2(ratio)` ms（约 **130–310 ms**）。来源：[`state.rs#L215-L283`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/state.rs#L215-L283)、[`state.rs#L2930-L2948`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/state.rs#L2930-L2948)。

### 2.6 颜色系统：HSL 约束

`palette.rs` 为每个类别定义 `(hue, chroma)`，hue 在 GPUI 的 `Hsla` 中是 **0..1**：

| 类别 | hue | 度 | chroma | 语义 |
| --- | --- | --- | --- | --- |
| Code | 0.605 | 218° | 1.0 | 源码与 checkout |
| AgentScratch | 0.065 | 23° | 1.0 | agent 工作区、沙箱、实验 |
| Toolchain | 0.415 | 149° | 1.0 | 编译器、包管理器 |
| Synced | 0.535 | 193° | 1.0 | 同步客户端目录 |
| Git | 0.955 | 344° | 1.0 | 版本库对象 |
| Media | 0.745 | 268° | 1.0 | 图片、音乐、视频、游戏、模型 |
| Cache | 0.125 | 45° | 0.95 | 可再生成的缓存 |
| Documents | 0.600 | 216° | 0.18 | 文档、下载、桌面（几乎中性） |
| Other | 0.600 | 216° | 0.08 | 无法识别（几乎中性） |

来源：[`palette.rs#L23-L64`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/palette.rs#L23-L64)、[`palette.rs#L128-L174`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/palette.rs#L128-L174)。

---

## 3. 核心设计约束借鉴与边界

### 3.1 借鉴的约束
1. **固定 S/L 基座，深度只动明度**：把颜色作为分类标签而非面积权重。
2. **类别色与交互/状态色分层**：类别色不进入 `ShellTokens` 的 `ring`/`primary`/`warning`/`danger`。
3. **轮廓置于顶层覆盖**：选中环绘制在 tile 与子块之上，避免被嵌套子块遮挡。
4. **表头预留与标签裁剪**：父目录名称独占表头带，宁可放弃细分也不将文字叠在子块上。
5. **多重编码**：结合颜色、图标、名称与图例，不将颜色作为唯一信息通道。

### 3.2 不照搬的部分
1. **许可证与资产**：仅借鉴设计约束，不复制源码、截图或图标资产。
2. **框架与绘制架构**：disktree 使用单 canvas 绘制 quad，Cairn 使用 GPUI 元素树以保留焦点、无障碍（AccessKit）、右键菜单与 hover 能力。
3. **清理标记语义**：不引入 disktree 的 `Marked` 待删标记或可回收斜线 hatch。

---

## 附录：一手来源索引

所有链接均指向 `tobi/disktree` commit `6c8d4ce6211bf135ff4e42890faebff1040e1846`。

| 主题 | 文件/链接 |
| --- | --- |
| 产品定位、使用、平台 | [`README.md`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/README.md) |
| 技术栈、版本、license | [`Cargo.toml`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/Cargo.toml)、[`crates/disktree-app/Cargo.toml`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/Cargo.toml) |
| MIT 许可证 | [`LICENSE`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/LICENSE) |
| 类别模型与名称表 | [`classify.rs`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-core/src/classify.rs) |
| 树与指标 | [`tree.rs`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-core/src/tree.rs) |
| squarified 布局、header、Others | [`treemap.rs`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-core/src/treemap.rs) |
| 颜色公式 | [`palette.rs`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/palette.rs) |
| 绘制、轮廓、标签 | [`treemap_view.rs`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/treemap_view.rs) |
| 视图变换、缩放、过渡、输入 | [`state.rs`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/state.rs) |
| 图例、侧栏、tooltip | [`views.rs`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/views.rs) |
| 主题 fallback | [`appearance.rs`](https://github.com/tobi/disktree/blob/6c8d4ce6211bf135ff4e42890faebff1040e1846/crates/disktree-app/src/appearance.rs) |
| 设计意图提交 | commit [`75d657d`](https://github.com/tobi/disktree/commit/75d657d57a227f978d6a8936bb219db963a33717) |
| 发布 v0.11.0 | [release](https://github.com/tobi/disktree/releases/tag/v0.11.0) |
| gpui-omarchy 主题源码 | [`gpui-omarchy 0.1.3 theme.rs`](https://docs.rs/crate/gpui-omarchy/0.1.3/source/src/theme.rs) |
