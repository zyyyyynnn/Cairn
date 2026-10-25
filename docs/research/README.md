# 竞品研究索引

Cairn 的外部产品研究按"索引 + 分类单品"组织：本文件是索引与结论，每个类别一份完整档案，单品深潜单独成文。Cairn 当前规范以 [SPEC.md](../SPEC.md) 与 [product/](../product/) 下的文档为准，研究内容不构成实现承诺。

## 文档地图

| 文档 | 内容 |
| --- | --- |
| [everything.md](everything.md) | Everything（voidtools）完整单品档案：1.5 线的索引、folder size、Content、服务与安全、平台能力 |
| [search-launchers.md](search-launchers.md) | 搜索 + 启动器类：Listary、Flow Launcher、PowerToys Run |
| [disk-analyzers.md](disk-analyzers.md) | 磁盘空间可视化类：SpaceSniffer、WinDirStat 2.x、WizTree、TreeSize Free、RidNacs |
| [disktree.md](disktree.md) | disktree 完整单品：Treemap 配色、布局与视觉交互深潜（固定 v0.11.0） |
| [cross-form.md](cross-form.md) | 交叉形态：搜索 + 空间分析一体的空白、Cairn 差异化定位建议、采用边界 |
| [references.md](references.md) | 工程与权威出处：查询语法官方文档、GitHub 工程参照、Rust 生态、IME 缺陷追踪、Treemap 论文 |

## 分类矩阵

| 产品 | 类别 | 数据获取层 | 可视化 | 实时性 | 许可 |
| --- | --- | --- | --- | --- | --- |
| Everything 1.5 | 实时文件搜索 | MFT + USN 常驻索引 | 无（纯列表） | 实时（USN 监控） | 免费可用于商业；Server 商用需 Site License |
| Listary | 搜索 + 启动器 | NTFS 分区自动索引 + 实时监控 | 无 | 近实时 | 免费；Pro $19.95 |
| Flow Launcher | 启动器 | 不自建（依赖 Everything / Windows Index） | 无 | 取决于数据源 | MIT |
| PowerToys Run | 启动器 | Windows Search Index | 无 | 取决于 Windows 索引 | MIT |
| SpaceSniffer | 空间可视化 | 目录枚举（官方未宣称 MFT） | Treemap | 文件系统事件反应（扫描会话内） | 免费软件 |
| WinDirStat 2.x | 空间可视化 | 目录枚举 + 提权 MFT 直读（2.5+） | Treemap / Sunburst / Flame | 可监视文件系统变化（会话内） | GPL-3.0+ |
| WizTree | 空间可视化 | MFT 直读（NTFS）；其他卷常规扫描 | Treemap | 快照式 | 个人免费；商用付费 |
| TreeSize Free | 空间可视化 | 目录枚举 + 提权 MFT（4.2+） | Treemap（4.8 起默认） | 快照式（未见实时监控说明） | 仅个人非商业免费 |
| RidNacs | 空间可视化 | 多线程目录枚举 | 树 + 柱状图 | 快照式（未见实时监控说明） | 免费软件（官方页未标注条款） |
| disktree | 空间可视化 | 整盘 MFT 直读 + rayon 目录遍历回退 | Treemap | 快照式按需重扫 | MIT |

## 结论

1. **两条产品线正在互相进入对方的地盘，但都没有走完。** 磁盘分析器在加搜索（WizTree File Search Filter、WinDirStat 2.x `Ctrl+F` 与重复检测），搜索器在加空间维度（Everything 1.5 folder size 索引、即时 `size:` 查询）；但没有任何产品同时具备"常驻实时索引 + 内嵌 Treemap + 搜索语法直驱视图过滤"。Everything 用户在 2026-08 请求内置可视化，开发者未回应。
2. **MFT 直读已经是公开且可复现的基线。** WizTree 官方宣称 46× 于 WinDirStat；WinDirStat 2.5 起提供直接 MFT 扫描选项；disktree 在源码中公开了 400 万记录 3.44 s 的实测与读穿空闲带的读取策略。Cairn 的 IDX-02 目标（400 万 ≤ 15 s）在公开数字面前已偏宽。
3. **可视化竞品的 Treemap 全部是"扫描会话快照"**，没有一个以常驻服务 + USN 增量为底座的"活树"。这与 Cairn 服务架构（SPEC §1.1、IDX-03）形成结构性差异，是差异化定位的核心机会。

差异化落地建议与采用边界见 [cross-form.md](cross-form.md)。
