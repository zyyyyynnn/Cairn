<div align="center">
  <img src="assets/icons/cairn-256.png" width="96" alt="Cairn 品牌标识">
  <h1 align="center" style="border-bottom: none">Cairn</h1>
  <p><strong>面向 Windows 10/11 的高性能本地文件搜索与磁盘空间可视化工作台</strong></p>
  <p>
    <a href="LICENSE"><img alt="Apache-2.0 许可证" height="24" src="https://img.shields.io/github/license/zyyyyynnn/Cairn?style=for-the-badge&amp;label=LICENSE&amp;labelColor=3F444A&amp;color=6B6E75"></a>
    <img src="https://img.shields.io/badge/-Rust-786B65?style=for-the-badge&amp;logo=rust&amp;logoColor=white&amp;labelColor=3F444A" height="24" alt="Rust" title="Rust">
    <img src="https://img.shields.io/badge/UI-GPUI%20Kit%200.8-5A6F73?style=for-the-badge&amp;labelColor=3F444A" height="24" alt="GPUI Kit 0.8" title="GPUI Kit 0.8">
    <img src="https://img.shields.io/badge/Kernel-NTFS%20%2F%20USN-566778?style=for-the-badge&amp;labelColor=3F444A" height="24" alt="NTFS / USN" title="NTFS / USN">
    <img src="https://img.shields.io/badge/Search-Tantivy-5C6E64?style=for-the-badge&amp;labelColor=3F444A" height="24" alt="Tantivy" title="Tantivy">
  </p>
</div>

## 项目概览

Cairn 将毫秒级本地文件检索与磁盘空间占用分析收敛于同一桌面工作台。底层直接解析 NTFS `$MFT` 主文件表并订阅 USN 变更日志（`FSCTL_READ_USN_JOURNAL`），在保持低内存占用的同时完成百万级文件冷启动加载、目录体量聚合与实时增量同步。

桌面端基于 GPUI Kit 0.8 构建硬件加速界面，通过异步 Windows Named Pipe 与后台索引服务 `cairnd` 解耦；未安装服务时自动回退至标准用户模式本地索引器。可选全文检索由独立子进程 `cairn-worker` 承担文档提取与 Tantivy 索引写入，隔离重型解析负载与主服务生命周期。

## 核心能力

- **NTFS 实时索引**：直接读取 `$MFT` 记录与非常驻 Runlist，结合 USN Journal 增量回放、缓存校验与离线卷重连恢复。
- **结构化查询引擎**：支持布尔逻辑、通配符、正则表达式、大小/时间范围、类型过滤与多卷并行施瓦茨变换排序。
- **进程隔离全文检索**：内置 CJK 双字与拉丁混合分词器，支持纯文本、代码、PDF 与 DOCX 内容提取及高亮摘要。
- **Treemap 磁盘空间分析**：基于 Squarified 算法构建交互式矩形树图，支持连续缩放平移、下钻导航与类型色彩映射。
- **原生 Shell 与回收站集成**：提供虚拟滚动结果表、中英双语切换、Windows 回收站枚举还原与系统原生属性对话框联动。
- **双模式与权限边界**：命名管道拒绝远程客户端，按调用方访问令牌实施鉴权，配置文件与索引存储强制绑定系统级安全描述符。

## 系统架构

```mermaid
flowchart TB
    subgraph Clients["客户端与表现层"]
        APP["cairn-app (cairn.exe)<br/>GPUI Kit 桌面端 · 虚拟表格 · Treemap"]
        CLI["cairn-cli<br/>命令行检索 · JSON / CSV 导出"]
    end

    CLIENT["cairn-client<br/>异步 Named Pipe 客户端 · 协议版本协商"]
    SERVICE["cairn-service (cairnd.exe)<br/>MultiVolumeEngine · SessionManager · 调用方鉴权"]
    WORKER["cairn-worker<br/>隔离式内容提取 · Tantivy 索引写入"]

    subgraph Domain["核心领域与内核解析层"]
        CORE["cairn-core<br/>查询 AST · 预排序与多卷排序 · Squarify"]
        NTFS["cairn-ntfs<br/>$MFT 解析 · Runlist · USN Journal"]
        CONTENT["cairn-content<br/>Tantivy 检索 · CJK 分词 · 文档提取"]
        PROTO["cairn-protocol<br/>定长帧头 · Postcard 编解码"]
    end

    APP & CLI --> CLIENT
    CLIENT <== "Named Pipe (\\\\.\\pipe\\Cairn.Service)" ==> SERVICE
    SERVICE <== "子进程批处理 IPC" ==> WORKER
    SERVICE --> CORE & NTFS & CONTENT & PROTO
    WORKER --> CONTENT
```

10 个 Crate 的完整职责划分与依赖边界见[系统架构](docs/engineering/architecture.md)，管道 DACL 与访问控制模型见[IPC 与安全](docs/engineering/ipc-security.md)。

## 技术栈

| 层级 | 技术 | 职责 |
| --- | --- | --- |
| 桌面界面 | Rust、GPUI Kit 0.8、Fluent | GPU 渲染界面、固定行高虚拟滚动、Treemap 画布与中英双语本地化 |
| 进程通信 | Tokio Named Pipe、Postcard、CRC32 | 定长帧编解码、超时与背压控制、会话分页缓存与增量状态同步 |
| 文件系统 | Win32 Storage / IO API、`cairn-ntfs` | `$MFT` 扇区流式解析、USN 变更日志轮询与目录大小聚合 |
| 全文索引 | Tantivy 0.27、lopdf、quick-xml、zip | CJK 双字分词、多格式文本提取、倒排索引构建与匹配摘要生成 |
| 系统服务 | `windows-service`、Win32 Security / Shell COM | SCM 服务托管、客户端令牌鉴权、SDDL 权限加固与回收站操作 |
| 分发打包 | `xtask`、WiX Toolset v5、Winget | MSI 安装包、Burn 引导安装器（`CairnSetup.exe`）、校验清单与构建门禁 |

## 快速开始

### 环境要求

- Windows 10 / 11（x64）与 PowerShell 7+
- Rust 1.85+（`x86_64-pc-windows-msvc`）
- WiX Toolset v5（仅构建 MSI 与 `CairnSetup.exe` 安装包时需要）

### 本地开发与运行

```powershell
# 启动用户态控制台索引服务（或直接启动桌面端自动回退本地索引）
cargo run -p cairn-service -- --console

# 启动桌面客户端
cargo run -p cairn-app

# 命令行检索与状态查询
cargo run -p cairn-cli -- --status
cargo run -p cairn-cli -- "ext:rs" --max 50 --sort size --asc=false
```

### 构建与打包发布

```powershell
cargo build --workspace --release
cargo run -p xtask -- package
```

打包产物输出至 `dist/`（含 `CairnSetup.exe`、`cairn.msi`、`cairn.yaml` 与 `SHA256SUMS.txt`）。完整发布规程见[打包与发布](docs/engineering/release.md)。

## 验证

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
$env:RUSTDOCFLAGS="-D warnings"; cargo doc --workspace --no-deps
cargo test --workspace
cargo deny check
cargo run -p xtask -- check-i18n
```

## 仓库结构

```text
crates/
  cairn-core/      查询语法 AST、过滤评估、自然排序与 Treemap 布局算法
  cairn-protocol/  Named Pipe 帧协议、Postcard 载荷与状态数据结构
  cairn-ntfs/      NTFS 引导扇区、$MFT 记录、Runlist 与 USN 日志解析
  cairn-content/   Tantivy 全文索引、CJK 分词器与文档内容提取器
  cairn-client/    异步 Named Pipe 客户端、版本握手与错误分类
  cairn-service/   cairnd 系统服务、多卷同步引擎、会话缓存与调用方鉴权
  cairn-worker/    全文索引提取与合并隔离子进程
  cairn-app/       GPUI Kit 桌面客户端、虚拟表格、Treemap 与 Shell 集成
  cairn-cli/       命令行检索与 CSV / JSON 导出工具
xtask/             国际化对齐校验、基准测试与 WiX 安装包构建自动化
assets/icons/      应用 SVG 母版与固定阶梯光栅图标资产
docs/              产品规格、工程架构与外部调研文档
```

## 文档

| 文档 | 范围 |
| --- | --- |
| [产品规格](docs/SPEC.md) | 产品范围、需求 ID、性能预算与验收标准 |
| [界面与交互](docs/product/ui.md) | 窗口布局、设计令牌、键盘导航与无障碍规范 |
| [查询语法](docs/product/search-syntax.md) | 布尔表达式、宏、属性过滤与全文检索语法 |
| [Treemap 视图](docs/product/treemap.md) | 磁盘空间聚合、Squarified 布局与缩放平移交互 |
| [品牌资产](docs/product/brand.md) | 图标母版、光栅尺寸阶梯与使用位置 |
| [系统架构](docs/engineering/architecture.md) | Crate 边界、进程拓扑与模块职责 |
| [索引与同步](docs/engineering/indexing.md) | `$MFT` 扫描、USN 对齐、缓存持久化与内容索引流水线 |
| [IPC 与安全](docs/engineering/ipc-security.md) | 协议帧限制、管道 ACL、调用方令牌鉴权与配置保护 |
| [性能与质量](docs/engineering/quality.md) | 性能基线、测试分层与 CI 质量门禁 |
| [打包与发布](docs/engineering/release.md) | WiX MSI、Burn 引导程序与 Winget 清单生成 |

## 许可证

Cairn 采用 [Apache-2.0 License](LICENSE)。
