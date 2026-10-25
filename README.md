# Cairn

Cairn 是一个面向 Windows 10/11 的高性能本地文件搜索与磁盘空间可视化工具。界面使用 Rust + GPUI Kit；索引由本地服务或标准用户模式索引器提供，支持 NTFS `$MFT`、USN 增量、可选内容索引与 Treemap 磁盘视图。

## 快速开始

```powershell
cargo build -p cairn-app
cargo run -p cairn-app
```

开发服务可用 `cairnd --console`；生产安装通过 Windows 服务运行。构建、安装与发布见 [docs/engineering/release.md](docs/engineering/release.md)。

## 验证

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
cargo run -p xtask -- check-i18n
```

## 文档

| 路径 | 内容 |
| --- | --- |
| [docs/SPEC.md](docs/SPEC.md) | 产品范围、需求 ID 与验收标准 |
| [docs/product/](docs/product/) | UI、查询语法、Treemap、品牌资产 |
| [docs/engineering/](docs/engineering/) | 架构、IPC/安全、索引、前端、性能、质量、发布 |
| [docs/research/](docs/research/) | 外部产品基准与一手参考文献 |

## 许可

Apache-2.0。
