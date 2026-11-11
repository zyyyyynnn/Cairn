# Windows 打包与分发

## 1. 产物

| 产物 | 说明 |
| --- | --- |
| `cairn.msi` | WiX v5 构建的 MSI（源码使用 WiX v4 schema） |
| `CairnSetup.exe` | GPUI 安装程序，内嵌 MSI 载荷；负责许可、路径、进度与提权启动 |
| `Cairn.exe` | GPUI 桌面端 |
| `CairnService.exe` | Windows 后台索引服务（LocalSystem） |
| `CairnCli.exe` | 命令行检索工具 |
| `CairnWorker.exe` | 隔离式内容抽取子进程 |
| `CairnUninstall.exe` | 卸载前端 |
| `CairnInstallGuard.exe` | MSI 自定义动作，拒绝 Program Files 之外的安装目录 |
| `cairn.ico` | 安装包与应用图标 |
| `SHA256SUMS.txt` | 发布产物校验和 |

## 2. 构建

```powershell
cargo run -p xtask -- package
```

- 默认输出目录：`dist/`。
- `xtask` 生成 `cairn.wxs`，调用 WiX v5 构建 MSI，把 MSI 追加到 GPUI `CairnSetup.exe`，并写出 `SHA256SUMS.txt`；任一步失败都必须保留上一次成功产物。
- 真实 WiX 构建测试需要 WiX 5、`mt.exe` 与 release 二进制，默认标记为 ignored；其余打包测试覆盖旧产物保留、校验和与失败路径。
- 安装包必须完整封装 `Cairn.exe`、`CairnService.exe`、`CairnCli.exe`、`CairnWorker.exe`、`CairnSetup.exe`、`CairnUninstall.exe`、`CairnInstallGuard.exe` 与 `cairn.ico` 资产。

## 3. 安装模式与服务

- 安装包采用每机器（Per-Machine）安装模式，`CairnSetup.exe` 声明 `requireAdministrator`。
- MSI 注册服务 `CairnService`（LocalSystem），数据目录 `%ProgramData%\Cairn` 只允许 SYSTEM/Administrators。
- 安装器暂存目录 `%ProgramData%\CairnSetup` 使用受保护 DACL（SYSTEM/Administrators，所有者 Administrators）；已存在目录会按句柄重新加固，无法加固时 release 拒绝继续。
- 卸载必须停止并移除服务注册、关闭 IPC 监听、删除程序目录与产品数据；机器级产品数据会覆盖所有本地 profile。
- 开发验证可用 `CairnService --console`；生产环境不得以 console 进程替代服务。

## 4. 更新

- 应用从 GitHub Releases 读取最新稳定版，下载 `CairnSetup.exe` 与 `SHA256SUMS.txt`，校验 SHA-256。
- 启动前以拒绝写入/删除的句柄锁住已校验的安装器；`CairnSetup.exe /quiet /restart-app /expected-sha256 <hex>` 在提权启动后重新校验自身哈希，并从同一个拒绝替换的句柄读取 MSI 载荷。
- 目标版本写入 LocalAppData 标记；安装未完成时，下次启动报告中断，成功时报告新版本。

## 5. 发布状态

- 代码签名（REL-02）尚未完成前，不得把未签名产物描述为已签名发布。
- 版本取自 workspace `Cargo.toml`，升级码由 `cairn-setup` 持有、由 `xtask` 注入打包产物；发布前运行完整测试、`cargo fmt --all --check`、clippy `-D warnings`、`cargo doc`、`check-i18n`、`check-docs`、`check-size` 与 `cargo deny check`。

## 6. 版本与兼容性

- 产品版本只在发布提交中提升；`main` 在两次发布之间保持最近一次发布的版本。版本号不得回退，发布有缺陷时以新的补丁版本继续前进。
- 命名管道负载由 postcard 位置编码固定，`WIRE_GUARD` 由产品主次版本推导。`crates/cairn-protocol/src/codec.rs` 的 wire 布局测试钉住查询帧字节：重构必须保持这些字节不变，类型化字段通过 serde 编码为发布时的取值形态（例如排序枚举编码为原有 key 字符串）。
- 只有发布提交可以改变负载布局：提升 minor 并同步更新 wire 布局测试，新旧版本在帧头与握手阶段互相拒绝。补丁版本不得改变负载布局。
- 各存储格式独立演进，由读取方校验：

| 格式 | 版本标记 | 变化时 |
| --- | --- | --- |
| 文件名索引快照 | `INDEX_MAGIC` + `INDEX_VERSION`（cairn-core） | 格式变化时 `INDEX_VERSION` +1，旧文件按不兼容处理 |
| 内容回填标记 | `CONTENT_BACKFILL_SCHEMA`（cairn-service） | 结构变化时 +1，旧标记丢弃并重建 |
| 安装器 payload | `CAIRNMSI` 魔数，无版本 | xtask 与 setup 同一构建产物，不跨版本读取 |
| 设置文件 | 无版本，serde 默认值演进 | 只做向后兼容的字段增补 |

- MSI `ProductVersion` 只接受数字三段式；升级码固定、产品码每构建生成。预发布标签不得直接进入 MSI。

## 代码位置

- `xtask/src/package.rs`：WiX MSI、GPUI Setup 封装与 checksum 生成。
- `xtask/src/docs_check.rs`：文档相对链接与必需页面检查。
- `xtask/src/size_check.rs`：Rust 源文件 2000 行上限检查。
- `crates/cairn-app/build.rs`、`assets/icons/`：图标嵌入。

技术选型与供应链约束见 [architecture.md](architecture.md)。
