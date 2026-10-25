# 权威出处与外部基准

> 研究溯源，不构成当前实现承诺。

## 1. 权威出处与外部一手基准文献（Authoritative References）

### 1.1 Everything 官方一手支持文档与公告
本规范的搜索文法、修饰符、函数体系与 Windows 服务模型深度吸收自 voidtools 官方支持站（`https://www.voidtools.com/support/everything/`）与开发者 `void`（David Carpenter）发布的权威公告：
- [What's New in Everything（1.5 特性划分官方口径）](https://www.voidtools.com/support/everything/whats_new/)
- [Search Syntax（运算符、优先级、通配符、转义规范）](https://www.voidtools.com/support/everything/search_syntax/)
- [Search Modifiers（修饰符清单与行为定义）](https://www.voidtools.com/support/everything/search_modifiers/)
- [Search Functions（函数、日期取值与大小语法）](https://www.voidtools.com/support/everything/search_functions/)
- [Keyboard Shortcuts（经典键盘操作与快捷键映射）](https://www.voidtools.com/support/everything/keyboard_shortcuts/)
- [Everything Service（Windows 服务提权与安全模型）](https://www.voidtools.com/support/everything/everything_service/)
- [Understanding Indexing and USN Journal（MFT 读取与 USN 监控技术解析）](https://www.voidtools.com/forum/viewtopic.php?p=53547)
- [Everything 1.5 Beta 完整更新记录与新特性公告 (t=9787)](https://www.voidtools.com/forum/viewtopic.php?t=9787)
- [Everything 1.5 Indexing 公告 (p=35398)](https://www.voidtools.com/forum/viewtopic.php?p=35398)
- [Everything SDK for 1.5 (IPC3 命名管道) (p=75613)](https://www.voidtools.com/forum/viewtopic.php?p=75613)

### 1.2 GitHub 现代文件管理器与检索工具一手基准
- [files-community/Files](https://github.com/files-community/Files) 与 [官方文档](https://files.community/docs)：WinUI 3 现代化桌面文件管理器，其 `Omnibar`（整合路径面包屑与搜索框）、侧边栏组织结构与基于 Win32 `IFileOperation` 的安全回收站交互为 Cairn 桌面交互设计的核心参考。
- [sharkdp/fd](https://github.com/sharkdp/fd)：简洁现代的命令行搜索体验、Smart Case 大小写自适应与直观的类型（`-t`）、扩展名（`-e`）、大小（`-S`）与时间筛选设计。
- [BurntSushi/ripgrep](https://github.com/BurntSushi/ripgrep)：基于 SIMD 向量化加速与自动编码识别的高性能流式文本检索标杆。
- [junegunn/fzf](https://github.com/junegunn/fzf)：交互式即时过滤算子、`--tiebreak` 相关性排序与管道无头供数参考。
- [jhspetersson/fselect](https://github.com/jhspetersson/fselect)：基于元数据与派生属性的结构化查询参考。
- [Canop/broot](https://github.com/Canop/broot)：紧凑树状呈现与自定义动词快捷流转参考。
- [sxyazi/yazi](https://github.com/sxyazi/yazi) 与 [jarun/nnn](https://github.com/jarun/nnn)：全异步 I/O、极致内存控制与纯键盘高效表格排版基准。
- [eza-community/eza](https://github.com/eza-community/eza)：现代化列元数据展示与时间格式化标准。

### 1.3 Rust 生态底层依赖与参考实现
- [ntfs-reader](https://crates.io/crates/ntfs-reader)：NTFS 卷 Boot Sector 与 MFT 记录解析参考。
- [usn-journal-rs](https://crates.io/crates/usn-journal-rs)：Windows USN Change Journal IOCTL 交互参考。
- [UltraSearch](https://github.com/Dicklesworthstone/ultrasearch)：进程隔离与低优先级内容抽取参考。

### 1.4 GPUI Kit 与 Windows 平台 IME 缺陷追踪
- [Zed #56327：Windows IME 中文输入法重复插入字符缺陷修复](https://github.com/zed-industries/zed/issues/56327)
- [Zed #56149：多显示器与高 DPI 下输入法候选窗坐标偏移修复](https://github.com/zed-industries/zed/issues/56149)
- [Zed #64850：输入法候选窗与输入文本行发生物理重叠缺陷修复](https://github.com/zed-industries/zed/issues/64850)
- [Zed #64389：UTF-16 代码单元与 UTF-8 字节偏移换算 Panic 修复](https://github.com/zed-industries/zed/issues/64389)

### 1.5 磁盘空间分析与 Treemap 可视化一手权威文献
- **SpaceSniffer（Uderzo Software）**：[官方支持与用户手册](http://www.uderzo.it/main_products/space_sniffer/)。层级嵌套矩形树图（Nested Treemap）、动态扫描流式分区、细节级别（Level of Detail, LoD）阈值与过滤语法体系标杆。
- **WizTree（Antibody Software）**：[官方主站与 MFT 技术解析](https://diskanalyzer.com/)。直接流式解析 NTFS `$MFT` 裸卷记录，配合在内存中沿 `parent_idx` 自底向上（Bottom-up Rollup）极速聚合目录尺寸与即时 Treemap 视口投影。
- **Treemap 算法经典论文**：
  - Ben Shneiderman (1992). *Tree visualization with tree-maps: 2-d space-filling approach*. ACM Transactions on Graphics (TOG), 11(1), 92-99.
  - Mark Bruls, Kees Huizing, & Jarke J. van Wijk (2000). *Squarified Treemaps*. In Joint Eurographics and IEEE TCVG Symposium on Visualization (pp. 33-42).
  - Jarke J. van Wijk & Huub van de Wetering (1999). *Cushion Treemaps: Visualization of Hierarchical Information*. IEEE Symposium on Information Visualization (InfoVis).
