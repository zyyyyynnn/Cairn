# 竞品研究索引

Cairn 的外部产品研究按单品组织：本文件汇总索引与对照矩阵，各单品研究独立成文。Cairn 产品需求与工程实现基准以 [SPEC.md](../SPEC.md) 及 [product/](../product/)、[engineering/](../engineering/) 相关规范为准。

## 文档地图

| 文档 | 内容 |
| --- | --- |
| [everything.md](everything.md) | Everything（voidtools）完整单品档案：1.5 线的索引、folder size、Content、服务与安全、平台能力 |
| [search-launchers.md](search-launchers.md) | 搜索 + 启动器类：Listary、Flow Launcher、PowerToys Run |
| [references.md](references.md) | 工程与权威出处：查询语法官方文档、GitHub 工程参照、Rust 生态、IME 缺陷追踪 |

## 外部产品对照

| 产品 | 类别 | 数据获取层 | 实时性 | 许可 |
| --- | --- | --- | --- | --- |
| Everything 1.5 | 实时文件搜索 | MFT + USN 常驻索引 | 实时（USN 监控） | 免费可用于商业；Server 商用需 Site License |
| Listary | 搜索 + 启动器 | NTFS 分区自动索引 + 实时监控 | 近实时 | 免费；Pro $19.95 |
| Flow Launcher | 启动器 | 不自建（依赖 Everything / Windows Index） | 取决于数据源 | MIT |
| PowerToys Run | 启动器 | Windows Search Index | 取决于 Windows 索引 | MIT |

## 结论

1. **MFT + USN 常驻索引是公开且可复现的基线。** Everything 1.5 以 MFT 直读和 USN 监控维护常驻索引，并公开 folder size 的成本口径；Cairn 的 IDX-02/IDX-06 目标以自测为准，外部数字只作参照。
