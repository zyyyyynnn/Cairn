# Cairn

Windows 10/11 本地文件搜索与磁盘空间分析工作台。

## 快速开始

环境：Windows 10/11 x64、Rust stable（见 `rust-toolchain.toml`）；打包另需 WiX 5。

```powershell
cargo run -p cairn-app
cargo run -p cairn-service -- --console
cargo run -p cairn-cli -- --status
cargo test --workspace
cargo run -p xtask -- check-i18n
cargo run -p xtask -- check-docs
cargo run -p xtask -- package
```

## 文档

| 文档 | 范围 |
| --- | --- |
| [产品规格](docs/SPEC.md) | 产品范围、需求 ID 与验收标准 |
| [界面与交互](docs/product/ui.md) | 窗口布局、设计令牌、键盘导航与无障碍 |
| [查询语法](docs/product/search-syntax.md) | 查询表达式、过滤函数与内容检索语法 |
| [Treemap 视图](docs/product/treemap.md) | 磁盘空间聚合、布局与交互 |
| [品牌资产](docs/product/brand.md) | 图标母版、尺寸阶梯与使用位置 |
| [系统架构](docs/engineering/architecture.md) | Crate 边界、进程拓扑与模块职责 |
| [索引与同步](docs/engineering/indexing.md) | `$MFT` 扫描、USN 对齐、缓存与内容索引 |
| [前端与 GPUI](docs/engineering/frontend.md) | 布局、焦点、IME 与窗口行为 |
| [IPC 与安全](docs/engineering/ipc-security.md) | 协议帧、管道 ACL、调用方鉴权与配置保护 |
| [性能](docs/engineering/performance.md) | 性能目标、内存与 Treemap 预算 |
| [质量](docs/engineering/quality.md) | 测试分层与 CI 质量门禁 |
| [打包与发布](docs/engineering/release.md) | GPUI Setup、WiX MSI、校验与发布门禁 |
| [研究与竞品](docs/research/README.md) | 外部产品调研与证据索引 |
| [贡献指南](CONTRIBUTING.md) | 开发环境、工程规范与 PR 流程 |

## 许可证

Cairn 采用 [Apache-2.0 License](LICENSE)。
