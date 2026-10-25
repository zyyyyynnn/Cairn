# Windows 打包与分发

## 1. 产物

| 产物 | 说明 |
| --- | --- |
| `cairn.msi` | WiX v5 构建的 MSI（源码使用 WiX v4 schema） |
| `CairnSetup.exe` | Burn 引导程序，链入 MSI |
| `cairn.exe` | 标准用户 GUI |
| `cairnd.exe` | Windows 后台索引服务 |
| `cairn.ico` | 安装包/PE 图标 |
| `cairn.yaml` | Winget 清单 |
| `SHA256SUMS.txt` | 发布产物校验和 |

## 2. 构建

```powershell
cargo run -p xtask -- package
```

- 默认输出目录：`dist/`。
- 打包过程先生成 WiX 源，再编译 MSI，最后编译 Burn bundle；任一步失败都必须保留上一次成功产物，不能删除旧安装包。
- `xtask` 的测试覆盖 MSI/Burn 生成、旧产物保留、Winget 类型与 sha256 校验；真实 WiX 构建测试需要 WiX 5 在 PATH 上，默认标记为 ignored。
- 安装包必须包含 `cairn.exe`、`cairnd.exe` 与 `cairn.ico`；禁止用改名或伪造脚本冒充安装器。

## 3. 安装模式与服务

- 支持标准用户安装；需要后台 LocalSystem 索引时，以管理员权限安装服务。
- 服务名 `CairnService`，数据目录 ACL 只允许 SYSTEM/Administrators。
- 卸载必须停止并移除服务注册、关闭 IPC 监听、删除程序目录；不得留下可连接的旧管道。
- 开发验证可用 `cairnd --console`；生产环境不得以 console 进程替代服务。

技术选型与供应链约束见 [architecture.md](architecture.md)。

## 4. 发布状态

- 代码签名（REL-02）尚未完成前，不得把未签名产物描述为已签名发布。
- 版本、升级码和 winget 清单由 `xtask` 统一生成/校验；发布前运行完整测试、格式检查与 `check-i18n`。

## 代码位置

- `xtask/src/package.rs`：MSI/Burn/Winget/checksum 生成。
- `dist/*.wxs`、`dist/cairn.yaml`：已生成的发布模板/清单。
- `crates/cairn-app/build.rs`、`assets/icons/`：图标嵌入。
