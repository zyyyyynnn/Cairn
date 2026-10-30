# Windows 打包与分发

## 1. 产物

| 产物 | 说明 |
| --- | --- |
| `CairnSetup.exe` | GPUI 安装前端 + 内置 MSI 载荷；管理员清单；官方下载入口 |
| `cairn.msi` | WiX v5 事务引擎；服务、文件、PATH、卸载器与唯一可见 ARP |
| `CairnUninstall.exe` | 随 MSI 安装的 GPUI 卸载器（来源 `cairn-setup-bin`） |
| `Cairn.exe` | 标准用户 GUI |
| `CairnService.exe` | Windows 后台索引服务 |
| `CairnCli.exe` | 命令行检索与受控文件操作工具 |
| `CairnWorker.exe` | 文档抽取 Worker |
| `cairn.ico` / `cairn-64.png` | 安装包/PE 图标与品牌图 |
| `cairn.wxs` | 可审查的 MSI 源 |
| `SHA256SUMS.txt` | 发布产物校验和 |

安装与卸载流程、ARP 策略、载荷格式和迁移规则见 [installer.md](installer.md)。

## 2. 构建

```powershell
cargo build --workspace --release
cargo run -p xtask -- package
```

- 默认输出目录：`dist/`。
- 依赖：WiX 5（`CAIRN_WIX` 可覆盖）与 Windows SDK `mt.exe`（`CAIRN_MT` 可覆盖）；release 二进制从 `target/release` 读取（`CAIRN_BIN_DIR` 可覆盖）。
- 打包先在私有 staging 目录完成 MSI、清单注入、载荷追加与校验和；任一步失败都保留 `dist/` 中上一次成功产物。
- 发布成功后清理 `bundle.wxs`、`theme.xml`、`zh-CN.wxl`、`cairn.yaml` 等退役文件；`dist/` 只保留当前安装器产物。
- 安装包必须包含 GUI、服务、CLI、Worker 与 GPUI 卸载器；禁止用改名或伪造脚本冒充安装器。
- CI 的 Installer Package job 构建 release 二进制并运行真实打包测试（WiX 5、`mt.exe`、MSI 校验、载荷回读与解包）。

## 3. 安装模式与服务

- `CairnSetup.exe` 为机器级安装，通过清单请求管理员权限；同时安装桌面端、CLI、Worker 与后台服务 `CairnService`，服务在安装时自动注册并启动。
- 日常双击 `Cairn.exe` 或使用 `CairnCli.exe` 均以当前标准用户身份运行；完成页启动应用时通过 shell 令牌避免继承管理员权限。
- 安装目录加入机器 PATH；卸载时由 MSI 移除 PATH 项。
- 搜索范围切换通过 UAC 停止/启动服务并切换启动类型（Automatic/Manual）。
- 服务名 `CairnService`，数据目录 ACL 只允许 SYSTEM/Administrators。
- 卸载停止并移除服务、关闭 IPC 监听、删除 MSI 组件树、PATH 与注册表，并清理 `%ProgramData%\Cairn`、`%APPDATA%\Cairn`、`%LOCALAPPDATA%\Cairn` 和当前用户自启动项；临时 helper 只清理产品可执行文件，共享目录中的用户文件保留。
- 开发验证可用 `CairnService --console`；生产环境不得以 console 进程替代服务。

技术选型与供应链约束见 [architecture.md](architecture.md)。

## 4. 发布状态

- 代码签名（REL-02）尚未完成前，不得把未签名产物描述为已签名发布。
- 版本、升级码与校验和由 `xtask` 统一生成/校验；发布前运行完整测试、格式检查与 `check-i18n`。
- 真实安装/升级/卸载矩阵需要在干净 Windows VM 上验证。

## 代码位置

- `crates/cairn-setup/`：GPUI 安装/卸载流程、MSI 调用、载荷与系统边界。
- `crates/cairn-setup-bin/`：两个 exe 与管理员清单。
- `crates/cairn-ui/`：与桌面端共享的品牌与主题 token。
- `xtask/src/package.rs`：MSI、清单注入、载荷与 checksum 生成。
- `dist/*.wxs`：已生成的发布模板。
- `assets/icons/`：图标唯一母版与固定尺寸产物。
