# Windows 打包与分发

## 1. 产物

| 产物 | 说明 |
| --- | --- |
| `cairn.msi` | WiX v5 构建的 MSI（源码使用 WiX v4 schema） |
| `CairnSetup.exe` | GPUI 安装程序，内嵌 MSI 载荷；负责安装路径、进度与提权启动 |
| `Cairn.exe` | GPUI 桌面端 |
| `CairnService.exe` | Windows 后台索引服务（LocalSystem） |
| `CairnCli.exe` | 命令行检索工具 |
| `CairnWorker.exe` | 隔离式文档文本与扩展属性解析子进程 |
| `CairnUninstall.exe` | 卸载前端 |
| `CairnInstallGuard.exe` | MSI 自定义动作，拒绝 Program Files 之外的安装目录 |
| `cairn.ico` | 安装包与应用图标 |
| `SHA256SUMS.txt` | 发布产物校验和 |

## 2. 构建

```powershell
cargo run -p xtask -- package
```

- 构建机需要 CMake 与 NASM：`aws-lc-rs` 的 Windows 汇编后端由二者编译；CI 的 Windows 作业安装 NASM，CMake 由镜像提供。
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

- 代码签名（REL-02）尚未完成前，不得把未签名产物描述为已签名发布；当前 `xtask package` 只生成 MSI、`CairnSetup.exe` 载荷与校验和，不执行签名。
- 版本取自 workspace `Cargo.toml`，升级码由 `cairn-setup` 持有、由 `xtask` 注入打包产物；发布前运行完整测试、`cargo fmt --all --check`、clippy `-D warnings`、`cargo doc`、`xtask check`（含 `check-i18n`、`check-docs`、`check-size`、`check-hygiene` 与版本治理）、`xtask bench-query --check` 与 `cargo deny check`。

## 6. 版本与兼容性

- 产品版本在发布准备/发布提交中提升；发布准备期间 `main` 可携带下一版本，发布时打 tag。版本号不得回退，发布有缺陷时以新的补丁版本继续前进。
- 发布 tag 必须为 `vMAJOR.MINOR.PATCH`，并与工作区 `Cargo.toml` 版本一致；应用更新器按 semver 解析 tag，非 semver tag 不会被提供。构建环境声明 `GITHUB_REF_TYPE=tag` 时，`xtask check` 与 `xtask package` 校验 tag 与 workspace 版本一致。
- 各存储格式独立演进，由读取方校验；版本标记的单一来源与递增规则见 §6.1。
- MSI `ProductVersion` 只接受数字三段式；`xtask check` 与 `xtask package` 同时校验字段上限（major/minor ≤ 255，build ≤ 65535）与前导零；升级码固定、产品码每构建生成。预发布标签不得直接进入 MSI。

### 6.1 版本登记

本表只登记标识符、单一来源与递增规则，不复制数值；数值只在对应常量或 manifest 中维护。

| 标识 | 单一来源 | 递增规则与消费方 |
| --- | --- | --- |
| 产品版本 | 根 `Cargo.toml` 的 `[workspace.package].version`；成员仅 `version.workspace = true` | 发布准备/发布提交提升；tag `vMAJOR.MINOR.PATCH` 必须一致；MSI 数字三段式与字段上限；应用、更新器、安装器通过 `CARGO_PKG_VERSION` 读取 |
| `PROTOCOL_VERSION` | `crates/cairn-protocol/src/message.rs` | wire 布局或消息语义变化时递增；帧头与 `Hello` 握手校验；`codec::wire_layout` 钉住负载字节 |
| `INDEX_MAGIC` / `INDEX_VERSION` | `crates/cairn-core/src/index_file.rs` | 索引快照格式变化时递增；旧快照按不兼容处理 |
| `CONTENT_BACKFILL_SCHEMA` | `crates/cairn-service/src/sync_engine/content_pipeline/mod.rs` | 内容回填标记结构变化时递增；旧标记丢弃并重建 |
| comment manifest `schema_version` | `xtask/src/comment_check.rs` 的 `COMMENT_SCHEMA_VERSION` | baseline/debt JSON 结构变化时递增；`xtask check` 读取并校验两者 |
| installer payload | `crates/cairn-setup/src/payload.rs` 的 `MAGIC`（`CAIRNMSI`） | 无版本；xtask 与 setup 同一构建产物 |
| 设置文件 | 无版本；serde 默认值演进 | 只做向后兼容的字段增补 |
| `skills-lock.json` `version` | 外部技能锁工具生成 | 外部工具 schema，不参与运行时版本 |
| Common Controls assembly version | `crates/cairn-setup-bin/installer.manifest` | Windows 平台程序集版本，不参与项目版本 |

外部依赖、`Cargo.lock`、系统工具与技能包的版本不属于项目版本，不在本表控制范围。

## 代码位置

- `xtask/src/package.rs`：WiX MSI、GPUI Setup 封装与 checksum 生成。
- `xtask/src/docs_check.rs`：文档相对链接与必需页面检查。
- `xtask/src/size_check.rs`：Rust 源文件 2000 行上限检查。
- `crates/cairn-app/build.rs`、`assets/icons/`：图标嵌入。

技术选型与供应链约束见 [architecture.md](architecture.md)。
