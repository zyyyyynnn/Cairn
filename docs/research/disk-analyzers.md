# 磁盘空间可视化工具单品调研：SpaceSniffer、WinDirStat 2.x、WizTree、TreeSize Free、RidNacs

> 本文是五个磁盘空间可视化工具的逐品档案，覆盖定位、发布状态、数据获取层、可视化与交互、性能公开口径、许可与对 Cairn 的借鉴点。研究索引见 [README.md](README.md)，交叉形态空白见 [cross-form.md](cross-form.md)；Cairn 的规范以 [SPEC.md](../SPEC.md) 与 [product/treemap.md](../product/treemap.md) 为准。版本号、日期与数字逐条附一手来源链接；查不到的一律标注"未找到公开数据"。

---

## 0. 结论摘要

1. **五款产品的视图全部是"扫描会话快照"**：SpaceSniffer 以会话内文件系统事件闪烁同步，WinDirStat 2.x 的 File Watcher 监视限于会话内，WizTree 存在监控条目但机制未公开，TreeSize Free 与 RidNacs 未见实时监控说明。没有一款以常驻服务 + USN 增量为底座（§1–§5 逐品证据）。
2. **MFT 直读已是半数基线**：WizTree 整盘默认直读 MFT，WinDirStat 2.5.0 起作为可选项，TreeSize Free 4.2 起管理员模式启用；SpaceSniffer 与 RidNacs 均无公开证据。除 WizTree 的对比宣称（46×/4×/2×）外，所有性能口径均为定性表述。
3. **过滤语言深度分化明显**：WizTree File Search Filter 已演进为"运算符 + allocated 大小 + 正则 + 相对时间"的完整过滤语言；SpaceSniffer 是 `;` 组合的轻量过滤 + 四色标签；WinDirStat 多条件 + 正则。"查询语言直驱 Treemap 视图"仍无人闭环，是 DSV-05 演进方向的直接证据。
4. **发布节奏**：WinDirStat 2.x 自 2026-06 起转入月度发布（2.6.2→2.9.3 五个月五个稳定版）；WizTree 维持数月一更并扩展至 macOS（WizTreeMac）与 Windows ARM。

---

## 1. SpaceSniffer（Uderzo Software）

### 1.1 定位与发布状态

单文件便携的动态 Treemap 磁盘可视化器，主打"扫描中即可导航"。官方自述："SpaceSniffer, find lost disk space the easy way."；页面致谢 Treemap 概念来自 Ben Shneiderman，"who kindly permitted the use of his concept into this tool"（[官网首页](http://www.uderzo.it/main_products/space_sniffer/index.html)）。

- **当前版本**：2.2.0.27——官网首页 "Latest release is 2.2.0.27"（[官网首页](http://www.uderzo.it/main_products/space_sniffer/index.html)）。
- **发布日期**：官网不提供版本历史页，未找到官方 changelog；官网列出的发行镜像 SnapFiles 记录 "Last updated: Mar 26, 2026"（[SnapFiles](http://www.snapfiles.com/get/spacesniffer.html)，第三方镜像日期佐证）。
- **官方文档形态**：产品区仅 index/features/download/donate 四页；下载走官方镜像 dAppCDN（[下载页](http://www.uderzo.it/main_products/space_sniffer/download.html)），extras 页仅提供用户翻译的说明书（"SpaceSniffer User Manual 1.1.4.0" 葡语版，[extras 页](http://www.uderzo.it/main_products/space_sniffer/download_extras.html)）。官方无 FAQ、无英文手册页、无版本历史（未找到公开数据）。

### 1.2 数据获取层

- 官方描述为智能缓存式目录扫描 + 流式渲染："Smart cached scanning engine, multiple views, single scan"；"You can navigate even during the scan"；"You can refine a zoomed area by a secondary scan process, even with main scan active"——主扫描进行中可对放大区域发起二次精扫（[features 页](http://www.uderzo.it/main_products/space_sniffer/features.html)）。
- "Can scan NTFS Alternate Data Streams"——NTFS 备用数据流纳入扫描（同一来源）。
- MFT 直读 / USN Journal：官方页面未宣称，**未找到公开数据**。

### 1.3 可视化与交互

- **布局**：Treemap（Shneiderman 概念授权）；具体变体官方未指明，改良 Squarified 的判断依据见 [references.md](references.md) 的经典论文谱系。
- **缩放与下钻**："Zoomable elements, navigate like a web browser"——浏览器式前进/后退与缩放；单击进入目录、双击放大至填满视图（[官网首页](http://www.uderzo.it/main_products/space_sniffer/index.html)）。
- **过滤语法体系**（示例均为官方首页原文）：模式 `*.jpg`；`|` 前缀取反——`|*.jpg`（"The view will show everything but JPEG files."）；年龄阈值 `>2years`；大小阈值 `>1mb`；标签过滤 `:red`（红标签）、`:all`（全部已标签）、`|:red`（排除红标签）；分号组合多条件：`*.jpg;>1mb;<3months;|:yellow`。
- **临时标签**：红/黄/绿/蓝四色（`Ctrl+1..4` 标注），可对标签过滤与取反（同一来源）。
- **多视图**："More views can be opened on the same disk with different filters and a single scan will be performed"——同一次扫描支撑多个不同过滤视图（[features 页](http://www.uderzo.it/main_products/space_sniffer/features.html)）。
- **变更感知**："Reacts to file system events, keeps always in synch, warns you by blinking elements"——会话内事件反应，变更元素闪烁提示（同一来源）。
- **其他**：右键调出 Windows 原生弹出菜单（"The Windows folders and file popup menu is accessible by right mouse click"）；"Export module lets you produce customizable textual reports"；"It doesn't clutter your registry, only a plain XML configuration file"；支持拖放，"Customizable in geometry, colors, behaviors"（[features 页](http://www.uderzo.it/main_products/space_sniffer/features.html)）。过渡动画细节：未找到公开数据。

### 1.4 性能、许可与平台

- **性能**：官网未给出任何扫描耗时数字（未找到公开数据）。
- **许可**："It's freeWare (donations are welcome)"；未附个人/商业区分条款（未找到公开数据）。
- **平台**：官方页面未标注最低 Windows 版本（未找到公开数据）；发行形态为免安装单文件包。

### 1.5 对 Cairn 的可借鉴点

1. **过滤语法的 `|` 取反与 `;` 组合**：把 `size:`/`ext:`/`dm:` 映射为 Treemap 覆盖层（P2 演进候选）时，"`|` 前缀排除 + 分号串联"是与 Cairn 逻辑组合语义（QRY-02 的 `!`/与）最易对齐的轻量参照；被过滤元素淡出不重排，与 DSV-05 现行机制同型（[官网首页](http://www.uderzo.it/main_products/space_sniffer/index.html)）。
2. **"multiple views, single scan"**：一次扫描、多过滤视图的视图模型可作为 DSV-05 之后的演进参照，但需评估与 DSV-07 双态隔离的冲突（[features 页](http://www.uderzo.it/main_products/space_sniffer/features.html)）。
3. **闪烁提示变更元素**：以极低成本表达"这里刚变了"，不重排、不重建布局；treemap.md §3.3 现行口径是"区块没有动效"，该做法仅作为未来活树方案的候选参考。

---

## 2. WinDirStat 2.x（windirstat，作者 NoMoreFood）

### 2.1 定位与发布状态

老牌 GPL 分析器的现代化重写，从"treemap 分析器"扩展为"扫描 + 搜索 + 重复检测 + 权限分析 + 恢复"的清理工作台。仓库自述："Windows disk usage analyzer and cleanup tool with interactive treemaps, large-file discovery, duplicate detection, and file-system monitoring."（[GitHub README](https://github.com/windirstat/windirstat)）。

| 版本 | 日期 | 关键演进（[CHANGELOG.md](https://github.com/windirstat/windirstat/blob/master/CHANGELOG.md) 原文摘录） | 日期来源 |
| --- | --- | --- | --- |
| 2.0.1 | 2024-10-15 | 2.x 起点（2.0.0 未公开发布，changelog 自 2.0.1 始）："Duplicate file finder based on file hashes"、"Native 64-bit build now available"、CSV 导出、MSI 安装器、per-drive 多线程扫描 | [Releases p2](https://github.com/windirstat/windirstat/releases?page=2) |
| 2.0.3 | 2024-10-22 | 选中项空间合计、稀疏文件属性（Z）显示、MSI 与渲染修复 | 同上 |
| 2.1.1 | 2024-12-08 | 目录/文件/大小排除项、空文件夹清理 | 同上 |
| 2.2.0–2.2.2 | 2025-01-05 / 2025-01-18 | 2.2.1 引入 "Largest Files" 标签页；2.2.2 支持 `\?\Volume{GUID}` 路径扫描 | 同上 |
| 2.5.0 | 2025-01-18 | "Added direct NTFS MFT scanning option"；"Added dark mode"；搜索扫描结果、移动文件、文件哈希、硬链接追踪、二进制前缀；放弃 Win7/Server 2008 R2/ARM32 | 同上 |
| 2.6.0 | 2025-05-17 | JSON 扫描结果、可选哈希算法、Microsoft Store MSIX 与 Chocolatey 分发、恢复 Win7 支持 | 同上 |
| 2.6.2 | 2026-06-01 | DLL 侧加载防护等修复 | [Releases](https://github.com/windirstat/windirstat/releases) |
| 2.7.0 | 2026-07-01 | "Added Storage Analytics and cost optimization view"；权限扫描与导出；XXH3（XXH3）哈希 | 同上 |
| 2.8.0 | 2026-08-02 | "Added flame graph as an alternative visualization view"、"Added sunburst graph as an alternative visualization view"、Hilbert/Moore treemap 布局、可取消哈希 | 同上 |
| 2.9.0 | 2026-09-26 | "Added deleted file recovery for NTFS and exFAT volumes"；MTP 设备扫描；"Added cancellation support while loading NTFS MFT records"；大小比例 tooltip；MFC 迁移至原生 Win32/GDI | 同上 |
| 2.9.2 | 2026-10-06 | 恢复扩展至 FAT12/16/32；"Changed application license to GPL v3 or later" | 同上 |
| 2.9.3 | 2026-10-11 | "Improved NTFS direct volume scanning performance"；File Watcher 内存与枚举分配优化 | 同上 |

### 2.2 数据获取层

- 默认目录枚举，多目标并行："Scan multiple drives or folders together, with pause/resume, refresh, and accelerated local NTFS scanning when elevated"——提权后加速本地 NTFS 扫描（[README](https://github.com/windirstat/windirstat)）。
- **MFT 直读为可选项**：2.5.0 "Added direct NTFS MFT scanning option"；2.9.0 加载 MFT 记录支持取消；2.9.3 定性改进直读性能（[CHANGELOG.md](https://github.com/windirstat/windirstat/blob/master/CHANGELOG.md)）。
- **变更监视**：README "Watch file-system changes"；2.9.0 "Added live path filtering with regular expressions in File Watcher"。File Watcher 是否基于 USN Journal：官方未明说，**未找到公开数据**。
- 扫描范围：本地 NTFS 之外支持网络共享、管理共享路径的卷/空闲统计（2.9.0）与 MTP 便携设备（2.9.0）。

### 2.3 可视化与交互

- **布局**：可配置 Treemap / Sunburst（2.8.0）/ Flame graph（2.8.0），外加 Hilbert 与 Moore 曲线式 treemap 变体（2.8.0）；2.9.0 增加 treemap style presets、2.7.0/2.8.0 增加 folder frames 与绘制阈值。
- **缩放与下钻**：changelog 未描述缩放下钻模型的系统文档（未找到公开数据）；2.9.0 "Added size proportion tooltips showing percentages of the total scan and parent folder"。
- **搜索与过滤**：`Ctrl+F` 按名称、大小、文件/文件夹类型、所有者搜索（README）；过滤按路径/名称/大小/年龄且支持正则（README）；2.9.0 增加大小区间、条目类型与所有者搜索过滤器及搜索结果大小合计。
- **重复检测**：2.0.1 基于哈希起步 → 2.7.0 XXH3 → 2.8.0 可取消哈希 → 2.9.0 64 MiB 以上可选稀疏哈希，抽样匹配标记为 "Probable duplicates"；云端文件防护（README）。
- **删除恢复**：2.9.0 NTFS/exFAT → 2.9.2 FAT12/16/32；"recovery depends on surviving metadata"，需管理员（README）。
- **快照持久化**："save/load scans as CSV or JSON"；命令行 "Scripting via `/SaveTo`, `/SaveDupesTo`, or `/SavePermsTo` plus `/LoadFrom` reopens a saved scan"（README）。
- **清理动作**："Open, move, or delete files; run Windows cleanup tools, NTFS compression, hardlink deduplication, and custom cleanup commands"（README）；2.5.0 增加 dism analyze、VHD 优化、程序与功能入口，2.9.0 "Added support for an arbitrary number of custom cleanups"——分析器持续向"写操作工作台"扩张。
- **扩展名分析**：扩展名列表色彩化（1.x 传承），2.7.0 "Added option to group unregistered file extensions"、双击搜索扩展名；2.5.0 "Added ability to auto-search from extensions menu"。
- **其他**：逻辑/物理大小切换、free/unknown space 展示（README）、权限条目查看与按修改年龄估算存储分层成本（2.7.0 Storage Analytics）、dark mode（2.5.0）、目录历史（2.5.0 "Added previous folder tracking history"）。

### 2.4 性能公开口径

- 官方未给出扫描耗时或对比数字（未找到公开数据）；2.9.3 "Improved NTFS direct volume scanning performance" 为定性表述（[CHANGELOG.md](https://github.com/windirstat/windirstat/blob/master/CHANGELOG.md)）。
- WizTree 官网 "46x faster than WinDirStat!" 以本产品为对比对象（见 §3.4）。

### 2.5 许可与分发

- **许可**：GPL v3 or later（[README](https://github.com/windirstat/windirstat)）；2.9.2 changelog 明确 "Changed application license to GPL v3 or later"；logo CC BY 3.0。
- **分发**：Microsoft Store、MSI（x64/arm64/x86）、MSIX bundle、便携 ZIP/7z（README）；Chocolatey（2.6.0）与 WinGet MSIX 发布（2.7.0）由 changelog 证实。
- **平台**：实测 Windows 7/8/8.1/10/11 与 Server 2008 R2–2025；"may work on older or newer operating systems, but those systems are not supported"（README）。

### 2.6 对 Cairn 的可借鉴点

1. **搜索 ⇄ 可视化双向联动**：搜索结果与 Treemap tile 互相定位，是 DSV-06"在表格中查看"逆向方向（表格 ⇄ Treemap 联动）的现成范式（[README](https://github.com/windirstat/windirstat)）。
2. **MFT 直读的取消路径**：直读作为可选加速 + "Added cancellation support while loading NTFS MFT records"（2.9.0），与 QRY-06 协作式取消同构——Cairn 的 DSV 回退扫描流同样需要中途取消语义（[CHANGELOG.md](https://github.com/windirstat/windirstat/blob/master/CHANGELOG.md)）。
3. **快照文件 + 命令行导入导出**：CSV/JSON 快照与 `/SaveTo`//`LoadFrom` 为无头回归提供夹具，呼应 IDX-10 MFT 镜像离线构建的可测试性方向。

---

## 3. WizTree（Antibody Software）

### 3.1 定位与发布状态

以 MFT 直读为卖点的最快磁盘分析器。官方自述："The FASTEST Disk Space Analyzer"——"Find the files and folders using the most space on your hard drive - QUICKLY!"（[官网](https://diskanalyzer.com/)）。

- **当前版本**：4.33（2026-09-16，下载页 "V 4.33 16 September 2026"）；同版发布 WizTreeMac 1.00："WizTree is now available for macOS 12 or later and is called 'WizTreeMac'."（[Download 版本历史](https://diskanalyzer.com/download)）。

### 3.2 数据获取层

- **MFT 直读**："When scanning NTFS formatted drives, WizTree reads the hard drive's Master File Table (MFT) directly from the disk (like Everything Search does)."（[官网](https://diskanalyzer.com/)）——与 Everything 同路线，正是 Cairn IDX-02 的同一路径。
- **提权边界**："high speed NTFS scanning is only possible when running with admin rights"（[FAQ](https://diskanalyzer.com/faq)）；便携版 4.21 起默认以非管理员启动（版本历史）。
- **目录级 MFT**：4.20 新增 tweak "Scan MFT (if possible) when a folder is selected for analysis"（默认关，按住 CTRL 强制）（[版本历史](https://diskanalyzer.com/download)）。
- **非 NTFS/网络卷**走常规快速扫描；4.21/4.28 两次针对该路径的线程化提速（同一来源）。
- **硬链接**："WizTree correctly handles hard linked files (doesn't count them more than once)"（[官网](https://diskanalyzer.com/)）。
- **USN**：版本历史存在 "file system monitoring" 条目（4.22 修复 4.21 引入的删除文件漏检回归），但监控是否基于 USN Journal **未找到公开数据**。

### 3.3 可视化与交互

- **布局与缩放**：Treemap + File View 列表；4.02 treemap 显示文件/文件夹名并支持双击缩放；4.03 缩放时隐藏 free space；4.06 treemap 显示文件大小并支持命令行导出 treemap 图像；4.26 修复超大 treemap 导出。过渡动画：未找到公开数据。
- **File Search Filter 演进**（全部出自[版本历史](https://diskanalyzer.com/download)）：
  - **4.00（2021-06-10）**：搜索重写对齐 WizFile——"Use operators '=', '>', '>=', '<', '<=' to filter files based on size or modified date"；k/m/g/t 后缀（`>=500m <=1g`）；allocated 大小 `a=`/`a>`；日期 `yyyy/mm/dd` 与 `today` 常量（`>=today-7`）；引号强制文件名搜索；命中高亮。
  - **4.02（2021-09-20）**：`=folder`/`=file` 类型词与 `namelen`/`pathlen`。
  - **4.08（2022-02-14）**：Include/Exclude 扫描结果过滤器（`Ctrl+Shift+F`）。
  - **4.13（2023-03-15）**：正则（`/` 前导，含空格需双引号）、搜索历史下拉、`/filterfullpath` 全路径开关。
  - **4.18（2024-02-02）**：Include 过滤改为扫描完成后应用（扫描中仅 Exclude 生效）；`=folder >1g` 按大小定位文件夹。
  - **4.19（2024-03-05）**：重复文件查找（Name+Size 或 Name+Size+Date）；4.24（2025-01-31）起新装默认改为 "name, size AND date"。
  - **4.27（2025-08-26）**：时间过滤——`>NOW-5`（最近 5 分钟）、`>NOW-1h`、`<NOW-1h >NOW-4h`、`>TODAY-5m`、`<=3am`、`=13:00:00`。
- **变更监控**：4.21 引入文件系统监控、4.22（2024-10-28）修复其删除文件漏检回归——存在监控但机制未公开（未找到公开数据）。
- **运维面**：4.27 支持注册表集中配置（`HKLM\Software\Antibody Software\WizTree\ConfigOverride\frmWizTreeMain`）与安装器旗标 `/supportercode`、`/runasadmin`、`/checkforupdates`（`!` 前缀只读），面向批量部署。
- **写操作**：4.29（2026-03-04）加入文件/文件夹重命名（F2 或慢速双击，经 "Tweaks->Enable Rename" 开关）——分析器向写操作扩张的又一例。
- **导出**：CSV（4.25 起导出选项 UI 化、默认含卷容量信息；4.27 `/exportUTCTime=1`）；命令行 CSV 导出与 MFT dump（官网 Guides）；CSV 导入（4.25 大幅提速）。

### 3.4 性能公开口径（全部官方口径）

| 宣称 | 出处 |
| --- | --- |
| "46x faster than WinDirStat!"（WizTree vs WinDirStat 对比区） | [官网](https://diskanalyzer.com/) |
| "MFT scanning speed up to 15% faster in certain cases"（4.20，2024-07-29） | [版本历史](https://diskanalyzer.com/download) |
| "more than 4x faster scanning of SSD drives"（4.21，2024-10-24，非管理员/非 NTFS/网络路径线程化） | 同上 |
| "up to 2x speed increase in certain cases"（4.28，2025-11-24，非管理员与非 NTFS/网络卷） | 同上 |

均未公布测试机型号、语料规模与逐项数据（官网未提供基准白皮书，未找到公开数据）。

### 3.5 许可与平台

- **许可**："WizTree is free for personal use"；商用需购买 Supporter Code；"Large organisations should purchase an Enterprise license for unlimited use"（[官网](https://diskanalyzer.com/)）。4.00 起 "licensing changed to per-business-size annual licenses"；4.01 起旧 Supporter Code 转为终身授权（[版本历史](https://diskanalyzer.com/download)）。
- **平台**："Windows Version (32/64 bit) 11, 10, 8, 7, Vista, XP, Server (2003+), Arm (via Prism Emulation)"；4.32（2026-08-05）"officially confirmed to work on Windows Arm devices via Prism Emulation"（同一来源）。

### 3.6 对 Cairn 的可借鉴点

1. **相对时间过滤语法**：`>NOW-5`、`>TODAY-5m` 与 Cairn `dm:` 相对时长（QRY-04）同构互为参照；若 DSV-05 演进为查询直驱过滤层，这套"分钟/小时/天粒度 + 区间组合"是现成的语法测试集（[版本历史](https://diskanalyzer.com/download)）。
2. **Include/Exclude 的"扫描中 vs 扫描后"分离**（4.18）：扫描中只应用 Exclude、Include 一律扫描后应用——与 DSV-05"过滤不重排、只淡化"的视图语义一致，值得固化为 Cairn 过滤层的时序约束。
3. **命令行 MFT dump 与 CSV 往返**：原始 MFT 记录可导出、扫描快照可导入复现，是 IDX-10 离线索引构建与 SM-5 零差异回归的现成产品范式。

---

## 4. TreeSize Free（JAM Software）

### 4.1 定位与发布状态

商业家族（Personal/Professional）的免费入口：目录树列表 + 一键 Treemap。官方自述："One Click Visualization of Storage Space Allocation down to File Level"（[产品页](https://www.jam-software.com/treesize_free/)）。

- **当前版本**：4.8.1（2025-11-20，[Changes 页](https://www.jam-software.com/treesize_free/changes.shtml)最新条目）。

### 4.2 数据获取层

- **MFT（管理员模式）**：4.2（2018-07-11）"Increased the performance when scanning local drives as administrator (using the MFT) significantly."（[Changes 页](https://www.jam-software.com/treesize_free/changes.shtml)）——MFT 直读限定于管理员扫描本地盘。
- **持续优化**：4.8（2025-10-06）"The scan performance has been improved drastically again in most situations."（机制未明说）。
- **USN**：未找到公开数据。4.5.1 有 "file-system change processing fix" 条目，未见实时监控说明。
- **扫描对象**：本地驱动器、网络共享、云存储；"Automatically detects USB drives, SD cards, and external hard drives like HDDs and SSDs. Supported Formats are NTFS as well as exFat."（[产品页](https://www.jam-software.com/treesize_free/)）。

### 4.3 可视化与交互

- **Treemap 演进**：4.0（2017-03-27）"TreeSize Free now offers the hierarchical treemap chart known from TreeSize Professional."（滚轮缩放、双击导航、悬停详情；同期采用 Ribbon 界面、Windows Store 版取代 TreeSize Touch）→ 4.1 treemap 可移至底部并显示 free-space tile、前进/后退导航 → 4.3 ≥15% 占用元素加粗高亮、treemap 位置/尺寸持久化 → 4.4 dark mode（Win10 1809+）与 treemap 拖放（默认关）→ **4.8（2025-10-06）"TreeSize Free now uses a treemap chart to visualize disk space capacities by default."**（全部出自 [Changes 页](https://www.jam-software.com/treesize_free/changes.shtml)）。
- **架构演进**：4.5（2021-07-02）移除 Metro 触控界面、放弃 Vista/7；4.6（2022-08-22）"TreeSize Free is now provided as a 64-bit application."；4.8 无需管理员安装、接入 Win11 资源管理器上下文菜单。
- **列表侧**：列显隐/排序（4.6）、过滤对话框（4.7 移入 "Scan" 页并重设计）、PDF 报告（4.6）、Win11 上下文菜单（4.8）。
- 过渡动画：未找到公开数据。

### 4.4 性能公开口径

仅两条定性表述：4.2 的 MFT 提速（"significantly"）与 4.8 的 "improved drastically again"；均无数字、无基准集（未找到公开数据）。

### 4.5 许可与定价

- **许可**："For private use in a non-commercial environment."（[产品页](https://www.jam-software.com/treesize_free/)）；服务器明确排除："No, TreeSize Free is not usable on Windows Servers."，4.7.3（2024-05-07）起 Windows Server 使用不再支持。
- **付费版边界**：Top 大文件过滤、重复文件查找与去重、磁盘空间历史对比、计划任务、命令行选项、Excel/HTML 导出、UNC/服务器扫描留给 Personal/Professional（产品页对比表）。
- **4.8 行为变化**：无需管理员安装（用户配置文件安装）、收集匿名使用数据（可关闭）、放弃 Windows 8/8.1（[Changes 页](https://www.jam-software.com/treesize_free/changes.shtml)）。

### 4.6 对 Cairn 的可借鉴点

1. **Treemap 从附加视图升为默认视图**（4.8）：商业厂商九年演进（2017 引入 → 2025 设为默认）佐证"高密度可视化优先"的路线判断，与 Cairn 以 DSV 为 P0 板块的定位互证（[Changes 页](https://www.jam-software.com/treesize_free/changes.shtml)）。
2. **安装期 DLL 劫持加固**：4.8.1 "A possible vulnerability due to a DLL hijacking attack during installation in the user profile has been fixed."——REL-01/REL-02 安装包与签名工程的现实警示案例。
3. **免费版与服务器场景切割**：以许可条款而非技术手段区分桌面/服务器场景，是 Cairn（Apache-2.0 全量开源、无此边界）审视自身定位的对照样本。

---

## 5. RidNacs（SplashSoft）

### 5.1 定位与发布状态

轻量传统分析器：目录树 + 可换肤柱状图，无 Treemap。官方自述："RidNacs ist ein kostenloses Programm zur Analyse des Speicherverbrauchs der Datenträger im Rechner (Disk Space Analyzer)."（[产品页](https://www.splashsoft.de/ridnacs/)，德语）。

- **当前版本**：3.0——下载页标注 "Aktuelle Version / Most Recent Version"，提供 Installer（2.7 MB）与 portable zip（1.6 MB）（[下载页](https://www.splashsoft.de/ridnacs-download/)）。
- **发布日期**：官方 [changelog](https://www.splashsoft.de/ridnacs/changelog/) 最新条目仅到 2.0.3 且全部条目无日期——3.0 与 2.0.3 的发布日期**未找到公开数据**；官网首页新闻仍停留在"Die aktuelle Version RidNacs 2.0.3 ist kompatibel mit Windows 10."（[官网首页](https://www.splashsoft.de/)）。

### 5.2 数据获取层与交互

- **多线程目录枚举**："Dateisystem wird im Hintergrund eingelesen (Multi-Threading)"——后台多线程读取文件系统，"wodurch bereits während des Scans innerhalb des Verzeichnisbaums navigiert werden kann"（扫描进行中即可在目录树内导航）（[产品页](https://www.splashsoft.de/ridnacs/)）。MFT/USN：未找到公开数据。
- **树 + 柱状图**：文件夹尺寸树视图，柱状图支持 skins 换肤定制；小文件按可配置阈值归组（2.0 起），未完成目录以符号标记（2.0 "unvollständige Verzeichnisse mit einem Symbol markiert"）；2.0.2 起单文件不再强制归组。
- **操作**：文件可直接打开/删除（2.0 起）；Explorer 上下文菜单集成（含启动外部程序）；收藏夹（驱动器与最近文件夹，2.0）。
- **changelog 可考条目**（[changelog](https://www.splashsoft.de/ridnacs/changelog/)，全部无日期）：2.0.3 改进目录选择对话框显示网络驱动器、修复 Windows 7 下文件夹图标；2.0.2 修复百分比列首位时的访问冲突与 TB 尺寸小数截断；2.0.1 修复百分比列与 TB 尺寸显示；1.0.x 保存 TreeView 列位置、安装语言同步为应用设置。
- **导出**：结果导出为 XML/HTML/CSV/TXT，官方定位为与后续分析做对比（"enabling comparison with later analyses"）；2.0 起新 XML 结果格式（同一来源）。
- 过渡动画：未找到公开数据。

### 5.3 性能与许可

- **性能**：仅定性宣称 "hohe Analyse-Geschwindigkeit und den geringen Arbeitsspeicherverbrauch"（高分析速度与低内存占用）；无数字（同一来源）。
- **许可**：官网称 "Freeware-Tool" / "kostenloses Programm"；官方页面未附任何开源许可证文本——第三方 listing 曾称 3.0 为 LGPL-3.0，无官方一手来源佐证，**不采用**。平台：Windows XP–11（"Unterstützt alle Windows-Versionen ab Windows XP inklusive Windows 11"）。

### 5.4 对 Cairn 的可借鉴点

1. **扫描中即可导航的渐进呈现**：RidNacs 与 SpaceSniffer 一致验证了"扫描未完成即开始下钻"的用户价值；Cairn treemap.md §1 的流式扫描 + 骨架呈现已覆盖该能力，无需改动。
2. **导出快照做前后对比**：XML/CSV 导出 + 人工 diff 的"清理前后对照"用法，是 IDX-05 一致性测试与 SM-5 零差异验证的终端用户版，可启发 CLI-02 `--export-csv` 的对拍场景设计。
3. **柱状图对照组**：无 Treemap 的树 + 柱状呈现无法同时表达层级占比与大面积下钻，反向验证 DSV-02 嵌套 Treemap 路线的必要性。

---

## 6. 五款横向对照

| 维度 | SpaceSniffer | WinDirStat 2.x | WizTree | TreeSize Free | RidNacs |
| --- | --- | --- | --- | --- | --- |
| 数据获取层 | 目录枚举 + ADS | 目录枚举 + 提权 MFT 选项（2.5.0+） | NTFS 整盘 MFT 直读（默认） | 目录枚举 + 管理员 MFT（4.2+） | 多线程目录枚举 |
| 视图 | Treemap（多过滤视图） | Treemap/Sunburst/Flame/Hilbert/Moore | Treemap + File View | 树列表 + Treemap（4.8 默认） | 树 + 柱状图 |
| 过滤语言 | 模式 + `\|` 取反 + 大小/年龄 + 标签 | 名称/大小/类型/所有者 + 正则 | 运算符 + allocated + 正则 + `>NOW` 相对时间 | 简单过滤对话框 | 无 |
| 变更感知 | 会话内事件闪烁 | File Watcher（会话内） | 有监控条目，机制未公开 | 未见说明 | 未见说明 |
| 快照与导出 | 文本报告（导出模块） | CSV/JSON 快照 + `/SaveTo`//`LoadFrom` | CSV 往返 + MFT dump + treemap 图像导出 | PDF 报告 | XML/HTML/CSV/TXT |
| 写操作 | 无（仅调系统菜单） | 删除/移动/压缩/清理工具 + 删除恢复 | 4.29 起重命名 | 删除（依赖系统确认） | 打开/删除 |
| 许可 | 免费软件 | GPL v3+ | 个人免费/商用付费 | 仅个人非商业 | 免费软件 |

三个横向结论对 Cairn 直接有效：其一，"变更感知"全部止步于扫描会话或机制不明，没有一个产品能声明"当前视图与文件系统处于同步状态"——IDX-07 的卷状态机（Synced/CatchingUp/Rebuilding/SnapshotMode/Offline）在同类中没有任何对应物，是可宣传的结构性差异；其二，MFT 直读的产品形态分三档（默认整读/可选项/管理员加速），Cairn 的"服务常驻 + 一次全量 + USN 追赶"是第四种形态，成本结构不同而非同一维度的快慢；其三，过滤语言与空间视图在所有产品中都是两个分离的界面（WizTree 的搜索在 File View、SpaceSniffer 的过滤在画布），"查询直驱 Treemap 覆盖层"仍无先例，DSV-05 的"淡化不重排"是实现它的最小可行语义。

---

## 附录：一手来源索引

| 产品 | 关键一手来源 |
| --- | --- |
| SpaceSniffer | [官网首页](http://www.uderzo.it/main_products/space_sniffer/index.html)、[features 页](http://www.uderzo.it/main_products/space_sniffer/features.html)、[下载页](http://www.uderzo.it/main_products/space_sniffer/download.html)、[SnapFiles 镜像（日期佐证）](http://www.snapfiles.com/get/spacesniffer.html) |
| WinDirStat 2.x | [GitHub README](https://github.com/windirstat/windirstat)、[CHANGELOG.md](https://github.com/windirstat/windirstat/blob/master/CHANGELOG.md)、[Releases](https://github.com/windirstat/windirstat/releases)（[第 2 页](https://github.com/windirstat/windirstat/releases?page=2)） |
| WizTree | [官网](https://diskanalyzer.com/)、[Download 版本历史](https://diskanalyzer.com/download)、[FAQ](https://diskanalyzer.com/faq) |
| TreeSize Free | [产品页](https://www.jam-software.com/treesize_free/)、[Changes](https://www.jam-software.com/treesize_free/changes.shtml) |
| RidNacs | [产品页](https://www.splashsoft.de/ridnacs/)、[下载页](https://www.splashsoft.de/ridnacs-download/)、[changelog](https://www.splashsoft.de/ridnacs/changelog/)、[官网首页](https://www.splashsoft.de/) |
