# 交叉形态：搜索 + 空间分析一体

> 本文是外部产品研究，不是 Cairn 当前实现状态；Cairn 的规范以 [SPEC.md](../SPEC.md) 与 [product/](../product/) 下的文档为准。单品机制见各产品档案：[everything.md](everything.md)、[search-launchers.md](search-launchers.md)、[disk-analyzers.md](disk-analyzers.md)、[disktree.md](disktree.md)。

## 1. 两条产品线的相向演进

| 方向 | 产品 | 已走到的位置 | 未走完的部分 |
| --- | --- | --- | --- |
| 分析器加搜索 | WizTree | File Search Filter 从 4.00 演进到 4.27，可在结果中按名称/路径过滤 | 搜索独立于 Treemap 视图，不驱动空间过滤 |
| 分析器加搜索 | WinDirStat 2.x | `Ctrl+F` 文件名搜索、重复检测、删除恢复与清理工具 | 同上；且写操作进入了分析器 |
| 搜索器加空间维度 | Everything 1.5 | `Index folder size` 实时维护目录尺寸（每目录 8 字节），`size:` 即时查询 | 纯列表呈现，没有内置可视化（用户请求无开发者回应） |

结论：**"常驻实时索引 + 内嵌 Treemap + 搜索语法直驱视图过滤"没有产品完成闭环**。Everything 用户在 2026-08 请求内置 SpaceSniffer 式可视化，社区回复中无开发者表态（[t=17866](https://www.voidtools.com/forum/viewtopic.php?t=17866)）；第三方 Total Commander 插件 VisualSpace 把 Everything 当 folder size 数据源（[totalcmd.net](https://totalcmd.net/plugring/13954.html)），说明需求真实存在。

## 2. 可视化竞品共同的结构性短板

可视化竞品的 Treemap 全部是"扫描会话快照"：WizTree 每次冷扫 MFT，disktree 按需重扫（MFT 失败回退 walk），SpaceSniffer/WinDirStat 的变更感知限于单次会话。没有一个以"常驻服务 + USN 增量"为底座的活树——这与 Cairn 的服务架构（SPEC §1.1、IDX-03）形成结构性差异。

## 3. Cairn 差异化定位建议

对照 [SPEC.md](../SPEC.md)（§1.1 产品定义、SM-1..5 北极星、DSV-01..07）与 [product/treemap.md](../product/treemap.md)（当前实现口径）：

1. **把"服务的常驻性"变成 Treemap 的常驻性（最核心的结构性差异）**。服务端已有 USN 追赶（IDX-03、SM-4）与自底向上聚合（DSV-01，400 万 ≤ 100 ms），"打开即整盘 + 树随 USN 增量刷新"在架构上顺理成章。落地时遵守 IDX-07 的零静默过期原则：非 Synced 卷的 Treemap 需显式标注陈旧状态（SPEC §1.3 目标 2）。SpaceSniffer 的"闪烁提示变更元素"是低成本的表现层参考（[features 页](http://www.uderzo.it/main_products/space_sniffer/features.html)）。
2. **搜索语法直驱 Treemap 过滤，作为 DSV 的演进方向而非当前承诺**。当前 treemap.md 明确"文件名/扩展名/尺寸文本过滤仍由经典搜索表格承担"，DSV-05 只有类别图例筛选。竞品证据（WizTree File Search Filter、WinDirStat `Ctrl+F`、Everything 的即时 `size:` 查询）说明"查询语言 × 空间视图"是公认需求但无人完成闭环。建议保持 DSV-07 双态隔离与 DSV-05 口径不变，把 `size:` / `ext:` / `dm:` 映射为 Treemap 上的淡化/高亮覆盖层列入 P2 演进候选——布局不重排、只做视觉通道，与现有"类别筛选只原地淡化"的机制同型（treemap.md §3.3）。
3. **用公开数字重校 IDX-02 基线**。disktree 在源码注释中公开了 400 万记录 3.44 s 的实测与读穿空闲带策略（[disktree.md](disktree.md)），WizTree 官方宣称 46× WinDirStat 与 4× SSD 提速；Cairn IDX-02 的"400 万 ≤ 15 s"宽于 disktree 的公开实测。建议 REL-04 基准报告把两者列为参照点；在 §3.7 `$BITMAP` 预跳过待接线项的验证中，一并测量"精确跳过 vs 短空闲带读穿"，以自测数字定案。
4. **不跟做的边界保持清晰**。WizTree 4.29 的文件重命名、WinDirStat 2.x 的删除恢复/清理工具都把写操作做进了分析器；Cairn 服务只读红线（SPEC §2.3）与 GUI 侧 `IFileOperation` 写面分工不变，disktree 的标记/删除流程已被 [disktree.md](disktree.md) §3.2 排除。
5. **IME 与双语仍是无人对标的独立差异**。全部竞品中只有 PowerToys Run（实验性开关）与 Flow Launcher（拼音搜索）触及拼音，没有任何产品声明针对中文输入法组合输入的深度适配；SM-3 的 IME 四大缺陷根治继续作为独立差异化项维护。

## 4. 采用边界

Cairn 可以借鉴交互与机制，但不能直接复制其他产品的主题、权限实现或 IPC 细节；任何采用项必须落到本项目的 SPEC、测试和当前 Windows/GPUI 约束中。
