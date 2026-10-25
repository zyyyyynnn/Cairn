# 搜索 + 启动器三单品研究：Listary、Flow Launcher、PowerToys Run

> 本文是外部产品研究，不是 Cairn 当前实现状态；Cairn 的规范以 [SPEC.md](../SPEC.md) 为准。研究索引见 [README.md](README.md)，交叉形态空白见 [cross-form.md](cross-form.md)。所有版本号、日期与机制描述均附一手来源；查不到的写"未找到公开数据"。

---

## 0. 结论摘要

1. **三者代表"检索入口"的三种形态**：Listary 是"自建索引 + 全局唤起 + 对话框跳转"的垂直整合单品；Flow Launcher 与 PowerToys Run 是"壳层不建索引、文件检索委托专用索引器"的插件面板。三者都没有 Treemap 或空间维度，查询语法强度也全部弱于 Everything 与 Cairn 的 QRY-04。
2. **只有 Listary 自建文件索引**（默认索引所有本地 NTFS 分区 + 实时监控 + 非可靠路径定时扫描降级）；Flow Launcher 默认走 Windows Search Index、可选 Everything SDK IPC，PowerToys Run 只走 Windows Search Index（OLE DB 查 SystemIndex catalog）。
3. **"快慢两段渐进呈现"与"引擎不可用显式化"是最值得直接借鉴的两个机制**：PowerToys Run 先 `CONTAINS` 后 `LIKE`、快查询 ≤100ms 与延迟执行插件分级；Flow Launcher 对引擎不可用抛显式错误并引导安装（可自动下载 Everything）——分别映射到 Cairn 的 QRY-01/QRY-06 与 SVC-04。
4. **微软官方正把 PowerToys Run 演进为 v2 "Command Palette"**（独立新应用，release notes 提及 Windows App SDK 运行时与 Native AOT 构建），旧 Run 只收修复；Listary 稳定线更新慢、靠 V7 beta（Rust 重写）续命。

---

## 1. Listary（Little Carved）

### 1.1 定位

一句话：介于启动器与文件管理器之间的即时文件搜索 + 保存/打开对话框跳转工具。

官方自述（[官网](https://www.listary.com/)）："The revolutionary file search tool for Windows"、"Listary makes finding your files and launching apps blazing fast"；并引 Scott Hanselman 评语称其 "somewhere between a launcher and a file manager, both but neither"。

### 1.2 当前版本与发布节奏

| 线 | 版本 | 日期 | 来源 |
| --- | --- | --- | --- |
| 稳定 | 6.3.5.94 | 2025-08-19 | [下载页](https://www.listary.com/download)（"v6.3.5.94 ｜ Last updated Aug 19, 2025"）、[changelog](https://help.listary.com/changelog) |
| V6 beta | 6.3.6.99 | 页面未标注 | [Changelog (beta)](https://help.listary.com/changelog-beta)（页面 last updated 2026-10-06） |
| V7 beta | 7.1.0.45 | 页面 last updated 2026-10-19 | [V7 changelog (beta)](https://help.listary.com/v7/changelog-beta)、[下载页](https://www.listary.com/download)（"Beta ｜ Last updated Oct 19, 2026"） |

稳定版约每 3–9 个月一版（6.3.0.78 于 2024-05-06、6.3.1.84 于 2024-11-27、6.3.2.88 于 2025-03-16，[changelog](https://help.listary.com/changelog)），beta 线持续滚动；6.3.1.84 起放弃 Win 7，当前要求 Win 10 & 11（[下载页](https://www.listary.com/download)）。

关键版本节点（[changelog](https://help.listary.com/changelog)、[V7 changelog](https://help.listary.com/v7/changelog-beta)）：

- 6.3.0.78（2024-05-06）："New file search engine based on Rust, improving performance by 20%-100%, while using less memory in most scenarios"；Win11 风格 UI；网络驱动器索引增强（多数 NAS 自动监控变化、启动后不重建）。
- 6.3.1.84（2024-11-27）：自定义索引重构——边索引边搜、增量更新、状态显示、开放给非 Pro 用户。
- 6.3.5.94（2025-08-19）：Listary 引擎（listary-core.exe）并入主进程，换取稳定性、搜索性能与内存收益。
- V7 7.0.0.5：引入轻量高级语法 `ext:pdf`、`*.pdf`、`date:today`、`date:2026-07`、`size:>1gb`、引号短语与 `-` 排除。
- V7 7.1.0.37："Rewritten in Rust: up to 4× faster, with 21% less memory"；已知限制：第三方集成插件暂不加载、无 ARM 版、同一时刻仅一个文件搜索窗口。

### 1.3 数据获取层

- **自建索引**："By default, Listary automatically indexes all NTFS partitions on your local disk"；本地全盘初次索引 "about 1-2 minutes"（[Index Options](https://help.listary.com/options-index)）。是否使用 USN Journal：官方文档未提及（未找到公开数据）。
- **实时性口径**："Listary monitors file changes in real time"，但 "Whether real-time updates are truly available depends on the path's file system / protocol (local NTFS drives and most SMB shares typically support real-time indexing)"；不可靠路径可切定时扫描，或 "Never"（初次建索引后不再自动更新）（同页）。
- **覆盖范围**：非 NTFS 卷与网络驱动器需手动添加并设重扫周期（Pro）；自定义索引可只扫描指定文件夹；OneDrive/Dropbox 同步文件自动覆盖，Google Drive 流式文件需手动加索引（同页）。
- **可验证性**：官方建议以 "Last updated" 时间戳与索引文件数验证实时监控是否生效（同页）。

### 1.4 交互与集成形态

- **入口**：双击 `Ctrl` 唤起启动器（"Press Ctrl twice to launch Listary"，[Hotkeys](https://help.listary.com/hotkeys)）；文件搜索窗口由托盘或自定义热键打开，另有 URL scheme `listary://show-file-search-window`；6.3.2.88 起双击 Ctrl 第二次可隐藏或切换到文件搜索窗口（[changelog](https://help.listary.com/changelog)）。
- **对话框内跳转（Quick Switch）**：在 Windows 保存/打开对话框下方自动出现 Quick Switch 窗口，"press Ctrl+G to instantly jump to the folder you're looking for"，并自动建议最近打开的文件夹（[Quick Switch](https://help.listary.com/quick-switch)）。
- **查询形态**：键入即出结果（官网宣称 "0.01 s"，[官网](https://www.listary.com/)）；尾随 `\` 收窄到路径（`win\note`、`d:\photo`）；类型过滤 `folder:` `file:` `doc:` `pic:` `video:` `audio:`；模糊匹配与全拼/首字母（6.0.11.35 引入），但 "fuzzy matching only supports apps and recently opened files"（[Search Files](https://help.listary.com/search-file)）。
- **结果动作**：Enter 打开、Ctrl+Enter 打开所在文件夹、Ctrl+P/N 上下浏览、Alt+P 预览、Ctrl+O 或 → 或右键呼出动作菜单（打开所在文件夹、复制/剪切/删除、复制路径、用 VS Code 编辑、系统右键菜单）（[Hotkeys](https://help.listary.com/hotkeys)、[Search Files](https://help.listary.com/search-file)）。
- **资源管理器与第三方集成**：对 File Explorer、桌面、文件/文件夹对话框原生增强；并适配 Files、Total Commander、Directory Opus、FreeCommander、XYplorer、xplorer2、Bandizip、AutoCAD、WinRAR 提取对话框、OneCommander、WPS 文件对话框、Explzh（[Integration](https://help.listary.com/options-integration)）。

### 1.5 扩展与生态

- 无公开插件市场。应用集成走独立开源仓库 `Listary.FileAppPlugin`（6.1.0.38 引入 "Open application integration plugin system"，含 WinRAR 集成，[changelog](https://help.listary.com/changelog)）；V7 7.1.0.37 注明第三方集成插件暂不加载（[V7 changelog](https://help.listary.com/v7/changelog-beta)）。
- 自定义面：Actions、Commands（经命令行控制其他应用）、Web Search 关键词、Filters、常用文件夹菜单（[文档目录](https://help.listary.com/)）。
- 集成管理：按应用开关增强；冲突应用可加入列表停用（如 `chrome.exe`）（[Integration](https://help.listary.com/options-integration)）。

### 1.6 性能公开口径

- 营销口径："0.01 s — Search in milliseconds"（[官网](https://www.listary.com/)）。
- changelog 定量口径：Rust 引擎 "improving performance by 20%-100%"（6.3.0.78）；索引构建峰值内存降约 70%（6.3.0.65）；V7 "up to 4× faster, with 21% less memory"（7.1.0.37）、内存较 7.0 再降 34%（约 50 MB，7.1.0.43）（[changelog](https://help.listary.com/changelog)、[V7 changelog](https://help.listary.com/v7/changelog-beta)）。
- 常驻内存绝对值、索引规模、与竞品同机对比：未找到公开数据。

### 1.7 许可与定价

- 免费版仅限个人使用（"The Listary free plan is for personal use only"），限时附赠部分 Pro 功能 "until Jul 30, 2026"（[Pro 页](https://www.listary.com/pro)）。
- Pro：一次性 $19.95、终身免费更新、商用许可、3 台设备、30 天退款；Pro 独占：Dark theme、Custom Filters、索引共享网络驱动器、Advanced search syntaxes、Custom Actions（同页）。

### 1.8 对 Cairn 的可借鉴点

1. **Quick Switch 的"第三入口"形态**（对照 UI-08）：Listary 的入口链是 全局热键唤起 → 搜索窗口 → **在其他应用的保存/打开对话框内把路径直接送进去**（Ctrl+G）。Cairn 的 UI-08 托盘 + 全局热键只覆盖前两环；对话框跳转是被长期验证的独特入口，可列入 P2 评估（实现面涉及对 Win32 公共对话框的窗口自动化，风险需单独评估）。
2. **索引新鲜度可验证 + 非实时路径显式降级**（对照 IDX-07、SM-5）：Listary 用 "Last updated" 时间戳让用户亲眼验证实时监控是否生效，并对不可靠路径显式提供定时扫描/Never 档位——与 SPEC 零静默过期原则同向，可作为 Cairn 卷状态（Synced/SnapshotMode）旁展示"最后追赶到 USN 锚点时间"的交互参照。
3. **拼音与轻量语法的用户基础**（对照 QRY-04、P2 `py:`）：Listary 6.0.11.35 起支持全拼/模糊全拼，V7 引入 `ext:`/`size:`/`date:today` 轻量语法；Cairn 的 QRY-04（运算符、区间、相对时长、`mtd/ytd/qtd`）强于三者，拼音首字母搜索有直接用户证据，且 Cairn 免费全量提供（Listary 把 Advanced search syntaxes 列为 Pro 独占）。

---

## 2. Flow Launcher

### 2.1 定位

一句话：社区插件生态的 MIT 开源键盘启动器，文件检索委托给 Windows Index 或 Everything。

官方自述（[GitHub README](https://github.com/Flow-Launcher/Flow.Launcher)）："A quick file search and app launcher for Windows with community-made plugins"；官网 "Quick File Search & App Launcher for Windows"（[flowlauncher.com](https://www.flowlauncher.com/)）。

### 2.2 当前版本与发布节奏

| 版本 | 日期 | 来源 |
| --- | --- | --- |
| v2.1.4（最新） | 2026-09-20 | [Releases](https://github.com/Flow-Launcher/Flow.Launcher/releases) |
| v2.1.3 | 2026-06-08 | 同上 |
| v2.1.2 | 2026-05-10 | 同上 |
| v2.1.1 | 2026-03-09 | 同上 |
| v2.1.0 | 2026-02-24 | 同上 |

节奏约每 1–3 个月一个 minor。v2.0.0 起仅支持 Windows 10+（v1.20.x 维持 Win 7 兼容），默认插件全部升级至 .NET 9（[v2.0.0 release notes](https://github.com/Flow-Launcher/Flow.Launcher/releases/tag/v2.0.0)）。技术栈：C# / .NET 9 + WPF（iNKORE.UI.WPF.Modern，README）；安装包约 97 MB、便携 ZIP 约 196 MB，winget/scoop/choco 分发（[官网](https://www.flowlauncher.com/)）。

### 2.3 数据获取层

- **不自建文件索引**，README："Supports Everything and Windows Index"。内置 Explorer 插件把三类检索做成独立引擎设置，默认全部 `WindowsIndex`（[Settings.cs @ dev](https://github.com/Flow-Launcher/Flow.Launcher/blob/dev/Plugins/Flow.Launcher.Plugin.Explorer/Settings.cs)）：`IndexSearchEngine`（文件/文件夹名搜索）、`PathEnumerationEngine`（目录枚举，另有 `DirectEnumeration` 档）、`ContentSearchEngine`（内容搜索）。
- **Everything 集成走官方 SDK IPC**：插件捆绑 Everything3.dll（[EverythingSDK/x64](https://github.com/Flow-Launcher/Flow.Launcher/tree/dev/Plugins/Flow.Launcher.Plugin.Explorer/EverythingSDK)），`EverythingApiV3.cs` 经 IPC 连接 Everything 实例（错误码含 `EVERYTHING3_ERROR_IPC_PIPE_NOT_FOUND`，[EverythingApiV3.cs](https://github.com/Flow-Launcher/Flow.Launcher/blob/dev/Plugins/Flow.Launcher.Plugin.Explorer/Search/Everything/EverythingApiV3.cs)）；未检测到 Everything 时可自动下载安装 Everything 1.4.1.1009（[EverythingDownloadHelper.cs](https://github.com/Flow-Launcher/Flow.Launcher/blob/dev/Plugins/Flow.Launcher.Plugin.Explorer/Search/Everything/EverythingDownloadHelper.cs) `DroplexPackage.Drop(App.Everything1_4_1_1009)`）。
- **内容搜索默认关闭**：`EnableEverythingContentSearch = false`，代码注释 "Intentionally require enabling of Everything's content search due to its slowness"；未开启时结果面板给出一键开启项（[SearchManager.cs @ dev](https://github.com/Flow-Launcher/Flow.Launcher/blob/dev/Plugins/Flow.Launcher.Plugin.Explorer/Search/SearchManager.cs)）。
- **路径搜索**：查询为路径串时直接导航；带 `>` 递归指示符时按所选引擎递归枚举子目录，否则顶层目录枚举（`DirectoryInfoSearch.TopLevelDirectorySearch`）；Windows Index 是否适用于该路径由 `WindowsIndex.PathIsIndexed` 逐路径判定，并可配置排除子目录；引擎不可用抛 `EngineNotAvailableException`（[SearchManager.cs](https://github.com/Flow-Launcher/Flow.Launcher/blob/dev/Plugins/Flow.Launcher.Plugin.Explorer/Search/SearchManager.cs)）。
- **实时性口径**：完全取决于所选引擎（Windows Index 或 Everything 各自的口径），Flow 自身无监控逻辑（README）。

### 2.4 交互与集成形态

- **入口**：`Alt+Space` 唤起（默认，可配置）；游戏模式 `Ctrl+F12` 暂停热键（README）。
- **动作关键词**：Explorer 插件注册全局 `*` 与内容搜索 `doc:`（均可在设置改绑，[plugin.json](https://github.com/Flow-Launcher/Flow.Launcher/blob/dev/Plugins/Flow.Launcher.Plugin.Explorer/plugin.json)）。
- **结果动作**：Enter 执行、Ctrl+Enter 打开所在文件夹、Ctrl+Tab 自动补全、F1 预览面板、F5 重载插件数据、Ctrl+Shift+Enter 以管理员运行（README）。
- **Dialog Jump（v2.0.0 新增）**：`Alt+G` 把 Open/Save As 对话框路径跳到活动文件管理器路径，或搜索文件/文件夹后把其路径送进对话框（[v2.0.0 notes](https://github.com/Flow-Launcher/Flow.Launcher/releases/tag/v2.0.0)）——与 Listary Quick Switch 同一生态位。
- **盘符空间显示**：路径查询指向盘根时结果直接展示盘符空间（`CreateDriveSpaceDisplayResult`，[SearchManager.cs](https://github.com/Flow-Launcher/Flow.Launcher/blob/dev/Plugins/Flow.Launcher.Plugin.Explorer/Search/SearchManager.cs)）。
- 其余：拖拽、结果优先级排序、书签/浏览器历史、Windows 设置搜索、环境变量路径搜索、主题/暗色（README）。

### 2.5 扩展与生态

- Plugin Store 内置（`pm install <name>`）；插件语言 C#/.NET、Python、F#、Node.js（README、[官网](https://www.flowlauncher.com/)）。
- 界面 26+ 语言；拼音搜索 + 双拼（v2.0.0 新增 Double Pinyin，需先开启 Search with Pinyin，[v2.0.0 notes](https://github.com/Flow-Launcher/Flow.Launcher/releases/tag/v2.0.0)）。
- 主题与主题构建器、动画与音效自定义（[官网](https://www.flowlauncher.com/)）。

### 2.6 性能公开口径

官方未提供检索延迟、内存或索引规模数字（未找到公开数据）；changelog 仅有 "Improved image icon loading performance" 等定性条目（[Releases](https://github.com/Flow-Launcher/Flow.Launcher/releases)）。

### 2.7 许可与定价

MIT，免费开源（[GitHub README](https://github.com/Flow-Launcher/Flow.Launcher)、官网）。

### 2.8 对 Cairn 的可借鉴点

1. **引擎不可用显式化 + 安装引导**（对照 SVC-04）：Explorer 插件把"依赖谁"做成一等设置，引擎不可用抛 `EngineNotAvailableException` 并在结果面板引导（未装 Everything 时自动下载安装）。Cairn 自带引擎无需委托，但"依赖不可用 → 显式恢复入口 + 引导"与 SVC-04 的服务问题带同构，第三方生态证明这是用户期望形态。
2. **内容搜索默认关闭并写明原因**（对照 CNT-01）：Flow 默认关闭 Everything 内容搜索并注明 "due to its slowness"，未开启时在结果位直接给出一键开启项——与 CNT-01 "内容索引默认关闭 + 启用即回填"同一取舍，"未就绪时把开启动作放在结果位"值得照搬。
3. **路径查询内的 `>` 递归枚举与盘符空间结果**（对照 QRY-04 `parent:`、DSV-07）：`C:\dir\>` 递归列子目录、盘根结果显示剩余空间，把"导航"与"空间"做进搜索结果流；Cairn 的 `parent:` 与盘符限定语法可吸收该交互作为输入提示，盘符空间显示与 Treemap 板块互补而不重复。

---

## 3. PowerToys Run（Microsoft PowerToys）

### 3.1 定位

一句话：微软官方 PowerToys 套件内置的快速启动器，Wox 的官方后继。

官方自述（[Microsoft Learn](https://learn.microsoft.com/en-us/windows/powertoys/run)，ms.date 2026-09-14）："PowerToys Run is a quick launcher for Windows power users that provides instant access to applications, files, calculator functions, and system commands without sacrificing performance."；同页 Attribution 承认承继 [Wox](https://github.com/Wox-launcher/Wox/) 与 Window Walker。

### 3.2 当前版本与发布节奏

- 母项目稳定版 v0.102.2933.0（2026-10-21，[Releases](https://github.com/microsoft/PowerToys/releases)）；稳定版约每 1–2 个月一版（v0.101.2362.0 2026-08-25、v0.100.0 2026-06-10、v0.99.0 2026-04-28，均见 Releases），期间滚动多条 Preview 构建。
- 架构：插件式 .NET（Core）桌面应用，WPF + MVVM；插件实现 `IPlugin`（快查询，"typically return results in less than 100ms"）与 `IDelayedExecutionPlugin`（长查询）（[architecture.md @ main](https://github.com/microsoft/PowerToys/blob/main/doc/devdocs/modules/launcher/architecture.md)）。
- **v2 演进**：官方文档注明 "PowerToys Run is getting an upgrade to v2! Check out the [Command Palette]"（[Run 页](https://learn.microsoft.com/en-us/windows/powertoys/run)）；Command Palette 作为独立新应用随主版本分发（激活键 Win+Alt+Space，[overview](https://learn.microsoft.com/en-us/windows/powertoys/command-palette/overview)，ms.date 2026-10-14）。0.102 为其新增 quick-access shelf（Alt+1..9 启动固定命令）、设置模糊搜索、Copilot 键支持（[v0.102.2933.0 notes](https://github.com/microsoft/PowerToys/releases/tag/v0.102.2933.0)）；同页 release notes 另提及 Command Palette 的 Windows App SDK 运行时目录处理（#51235）与 Native AOT 构建（#50997）。PowerToys Run 本体仅收修复。

### 3.3 数据获取层

- **不自建索引，只走 Windows Search Index**：Windows Search 插件（直接激活命令 `?`）"search for files and folders that are indexed by the Windows Search Index service"（[Run 页](https://learn.microsoft.com/en-us/windows/powertoys/run)）。
- **增强索引检测**：未开启 Enhanced 模式时在结果中显示警告并引导至 Windows "Searching Windows" 设置；插件以注册表 `EnableFindMyFiles = 1` 判定增强模式（[indexer.md @ main](https://github.com/microsoft/PowerToys/blob/main/doc/devdocs/modules/launcher/plugins/indexer.md)）。
- **源码级机制**（同一来源）：OLE DB 连接 SystemIndex catalog；返回 item URL、文件名、文件属性；按修改时间排序；属性位过滤隐藏文件；`CONTAINS` 全文谓词快、`LIKE` 谓词慢——"queries which have the CONTAINS keyword are much faster than those which contain the LIKE keyword"；先跑快查询上屏，慢的 LIKE 全量随后更新列表；文件结果固定 score 0（排于启动器结果列表底部）；`*abc*` 类通配查询由 `IDelayedExecutionPlugin` 承载（[architecture.md](https://github.com/microsoft/PowerToys/blob/main/doc/devdocs/modules/launcher/architecture.md)）。
- **Program 插件自扫**：扫描开始菜单、桌面等位置的可执行与快捷方式文件（.exe/.lnk/.url）；Folder 插件语法：`>` 进目录内搜索、`*` 按掩码、`>*` 组合（[Run 页](https://learn.microsoft.com/en-us/windows/powertoys/run)）。
- **实时性口径**：取决于 Windows Search 索引服务的抓取节奏，无 Listary 式自管监控；官方未给量化口径（未找到公开数据）。

### 3.4 交互与集成形态

- **入口**：`Alt+Space`（默认，可改）；"Use centralized keyboard hook" 解决从提权窗口唤起的焦点问题；"Ignore shortcuts in full-screen mode"（[Run 页](https://learn.microsoft.com/en-us/windows/powertoys/run)）。前提：PowerToys 须常驻后台且 Run 已启用（同页）。
- **直接激活命令**（同一来源的插件表）：`?` 文件、`.` 程序、`>` Shell、`<` Window Walker、`=` 计算器、`!!` 历史、`$` Windows 设置、`:` 注册表、`!` 服务、`)` 时间日期、`%%` 单位换算、`#` GUID/哈希/Base64、`//` URI、`{` VS Code 工作区、`_` Windows Terminal、`o:` OneNote、`??` Web 搜索等。
- **结果动作**：Ctrl+Shift+Enter 管理员运行、Ctrl+Shift+U 以其他用户运行、Ctrl+Shift+E 打开所在文件夹、Ctrl+C 复制路径、Tab 遍历结果与上下文按钮（同页）。
- **输入与排序调优**：Input smoothing、Immediate/Background 插件延迟毫秒数、Results order tuning、Selected item weight（默认 5）、缩略图开关（"may affect speed and stability"）、Use Pinyin（"experimental and may not work for every plugin"）（同页设置表）。

### 3.5 扩展与生态

- 插件管理器启停插件、改绑直接激活命令、开关全局结果（[Run 页](https://learn.microsoft.com/en-us/windows/powertoys/run)）。
- 社区插件经仓库目录分发：v0.102 notes 记录 Claude Desktop、GoogleTranslate 等进入社区插件目录（[v0.102.2933.0 notes](https://github.com/microsoft/PowerToys/releases/tag/v0.102.2933.0)）。
- v2 Command Palette 另有内置 Extension Gallery、Dock 常驻工具条、Bookmarks、WinGet 应用安装（[overview](https://learn.microsoft.com/en-us/windows/powertoys/command-palette/overview)）。

### 3.6 性能公开口径

官方无检索延迟、内存或索引规模基准（未找到公开数据）；仅架构文档给出插件分级口径（快查询 <100ms，慢查询走延迟执行，[architecture.md](https://github.com/microsoft/PowerToys/blob/main/doc/devdocs/modules/launcher/architecture.md)）。

### 3.7 许可与定价

MIT，随 PowerToys 免费开源（[GitHub](https://github.com/microsoft/PowerToys)）。

### 3.8 对 Cairn 的可借鉴点

1. **快慢两段渐进呈现**（对照 QRY-01、QRY-06、CNT-06）：先 `CONTAINS` 快查上屏、`LIKE` 全量后到，配合 IPlugin/IDelayedExecutionPlugin 的 100ms 分级，是"首屏不等全量"的成熟范式；Cairn 单引擎虽无慢路径，正则与 `content:`（Tantivy 求交）查询可沿用"先出可用结果、全量后到、可取消"模式，CNT-06 回填未就绪反馈同理。
2. **输入平滑与习惯加权**（对照 QRY-05）：Input smoothing 与 Selected item weight（选中结果更快上升）+ `!!` 历史插件，与 Listary "smart order according to your habits" 互相印证"按使用习惯调权"是启动器类公认需求；可作 Cairn QRY-05 预排序索引之上的 P2 排序演进候选。
3. **v2 重写的结构信号**（对照 SPEC §1.1、§2.3）：微软把启动器迁往新 UI 栈（Windows App SDK、Native AOT）并另立 Command Palette，旧 Run 仅维护——小窗启动器的 UI 栈迁移成本高。Cairn 走"完整窗口 + 高密度表格 + Treemap"（UI-01、DSV-07）与三者小窗形态结构性不同，无需跟进小窗，UI-08 热键唤起保留即可。

---

## 附录：一手来源索引

| 产品/主题 | 关键一手来源 |
| --- | --- |
| Listary 版本与许可 | [官网](https://www.listary.com/)、[下载页](https://www.listary.com/download)、[Pro](https://www.listary.com/pro)、[changelog](https://help.listary.com/changelog)、[changelog-beta](https://help.listary.com/changelog-beta)、[V7 changelog-beta](https://help.listary.com/v7/changelog-beta) |
| Listary 机制与交互 | [Index Options](https://help.listary.com/options-index)、[Search Files](https://help.listary.com/search-file)、[Quick Switch](https://help.listary.com/quick-switch)、[Hotkeys](https://help.listary.com/hotkeys)、[Integration](https://help.listary.com/options-integration) |
| Flow Launcher 版本 | [Releases](https://github.com/Flow-Launcher/Flow.Launcher/releases)、[v2.0.0 notes](https://github.com/Flow-Launcher/Flow.Launcher/releases/tag/v2.0.0)、[官网](https://www.flowlauncher.com/) |
| Flow Launcher 机制 | [README](https://github.com/Flow-Launcher/Flow.Launcher)、Explorer 插件源码 @ dev：[Settings.cs](https://github.com/Flow-Launcher/Flow.Launcher/blob/dev/Plugins/Flow.Launcher.Plugin.Explorer/Settings.cs)、[SearchManager.cs](https://github.com/Flow-Launcher/Flow.Launcher/blob/dev/Plugins/Flow.Launcher.Plugin.Explorer/Search/SearchManager.cs)、[EverythingApiV3.cs](https://github.com/Flow-Launcher/Flow.Launcher/blob/dev/Plugins/Flow.Launcher.Plugin.Explorer/Search/Everything/EverythingApiV3.cs)、[EverythingDownloadHelper.cs](https://github.com/Flow-Launcher/Flow.Launcher/blob/dev/Plugins/Flow.Launcher.Plugin.Explorer/Search/Everything/EverythingDownloadHelper.cs)、[plugin.json](https://github.com/Flow-Launcher/Flow.Launcher/blob/dev/Plugins/Flow.Launcher.Plugin.Explorer/plugin.json) |
| PowerToys Run | [Microsoft Learn: Run](https://learn.microsoft.com/en-us/windows/powertoys/run)、[Learn: Command Palette overview](https://learn.microsoft.com/en-us/windows/powertoys/command-palette/overview)、[architecture.md](https://github.com/microsoft/PowerToys/blob/main/doc/devdocs/modules/launcher/architecture.md)、[indexer.md](https://github.com/microsoft/PowerToys/blob/main/doc/devdocs/modules/launcher/plugins/indexer.md)、[PowerToys Releases](https://github.com/microsoft/PowerToys/releases) |
