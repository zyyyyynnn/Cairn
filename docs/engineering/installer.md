# Windows 安装、更新与卸载

> 对应需求：`REL-01`、`REL-04`。发布流程总览见 [release.md](release.md)。

## 1. 产物与职责

| 产物 | 外壳 | 职责 |
| --- | --- | --- |
| `CairnSetup.exe` | GPUI | 唯一的分发入口，不写入安装目录。负责语言/主题、许可确认、安装位置、进度、完成页；内置 MSI 载荷，不暴露 `msiexec` 界面。 |
| `cairn.msi` | WiX v5 | 事务引擎。安装 GUI、CLI、Worker、后台服务、卸载器与 PATH；隐藏自己的 ARP 条目。也是 Setup 的内部引擎，不作为用户入口发布。 |
| `CairnUninstall.exe` | GPUI | 随 MSI 安装到 `%ProgramFiles%\Cairn`；Apps & features 唯一可见的 Cairn 条目。负责确认、卸载进度与索引/设置清理，并在退出后由临时 helper 删除安装目录。 |
| `cairn.wxs` | WiX | 可审查的 MSI 源，随发布产物保留。 |
| `SHA256SUMS.txt` | — | 覆盖上述发布文件。 |

发布面只包含 `CairnSetup.exe`、`cairn.msi`、`cairn.wxs`、图标与 `SHA256SUMS.txt`；`bundle.wxs`、`theme.xml`、`zh-CN.wxl`、`cairn.yaml` 等退役文件在发布时从 `dist/` 清理。

## 2. 安装流程

1. `CairnSetup.exe` 通过 PE 清单请求管理员权限；UAC 由 Windows 在进程启动时完成，安装器自身不再二次提权。MSI 载荷暂存在管理员拥有的 `%ProgramFiles%\CairnSetup`，不使用用户可写的 `%TEMP%`。
2. 单实例互斥；已有窗口时置前并退出。界面语言跟随 Windows UI 语言（`zh-CN` / `en-US`），明暗配色跟随系统。
3. **许可协议**：展示 Apache License 2.0 名称与“查看完整许可…”；完整 `LICENSE` 写入 `%TEMP%\Cairn-LICENSE.txt`，通过 `explorer.exe` 以标准用户令牌打开。必须勾选接受才能继续。
4. **安装位置**：机器级安装，默认 `%ProgramFiles%\Cairn`；可输入或浏览选择 Program Files 下的子目录。校验空值、相对路径、UNC、根目录、Windows/Program Files 根、`..`、尾随点/空格、分号/百分号、非法字符和文件路径；服务与机器 PATH 只写入管理员拥有的 Program Files，`INSTALLFOLDER` 作为公开 MSI 属性传入。
5. **安装**：从自身载荷解包 MSI，先移除检测到的全部 Burn 注册条目，再通过 `MsiInstallProductW` 在同进程内执行 MSI；进度来自 MSI 的 `INSTALLMESSAGE_PROGRESS` 记录，动作来自 `INSTALLMESSAGE_ACTIONSTART`，不使用计时器伪造。
6. **完成**：显示结果与日志入口；“启动 Cairn”通过 `explorer.exe` 以登录用户令牌启动，避免 GUI 继承管理员令牌。
7. **静默安装**：`/quiet` 或 `/s`；默认安装到已安装位置或默认位置，不启动应用。退出码 `0` 成功、`3010` 需要重启、`1` 失败（具体日志保留在 `%TEMP%\CairnSetup-*.log`）。
8. **更新**：已安装同版本或更新版本进入维护页；较旧版本按更新流程执行 MSI 主升级；已存在的 Burn 注册条目即使其版本高于当前包，也先由注册的卸载器移除，再由当前 MSI 接管。安装交易进行中禁止关闭窗口。

## 3. 卸载流程

- Apps & features 只显示 `Cairn`，指向已安装的 `Cairn\CairnUninstall.exe`；MSI 自己的条目通过 `ARPSYSTEMCOMPONENT=1` 隐藏。
- 确认页写明卸载会移除程序、CLI、服务、索引、设置与注册表项；清理失败会记录日志，不提供保留数据选项。
- 卸载通过 `MsiConfigureProductW(..., INSTALLSTATE_ABSENT)` 执行；MSI 停止并移除服务、删除文件、PATH 条目、`HKLM\Software\Cairn` 与自定义 ARP 键。
- MSI 完成后，worker 删除 `%ProgramData%\Cairn`、`%APPDATA%\Cairn`、`%LOCALAPPDATA%\Cairn` 与 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Cairn`；UI 退出后，预复制的临时 helper 只删除安装目录中的产品可执行文件，目录非空时保留用户文件，不递归删除共享目录。helper 本身也暂存在管理员拥有的 `%ProgramFiles%\CairnSetup`。
- 卸载失败页用卸载口径措辞命名每个失败（已取消、另一个安装程序在运行、安装记录损坏、平台不支持、MSI 致命错误、找不到安装记录），不复用安装器文案，也不声称发生过安装动作。
- 静默卸载为 `CairnUninstall.exe --quiet`；没有产品注册时视作已卸载并返回 `0`。

## 4. 技术实现

### 4.1 Crate 分层

- `cairn-setup`：流程状态机、MSI 调用、载荷、系统边界与 GPUI 页面；可单测。
- `cairn-setup-bin`：仅承载 `CairnSetup.exe` 与 `CairnUninstall.exe` 两个 bin，并在链接后由 xtask 注入管理员清单。
- `cairn-ui`：桌面端与安装器共享的品牌常量、灰阶主题、系统语言/深浅色判定。安装器不是第二套设计系统。

### 4.2 为什么是 GPUI Setup + 静默 MSI

GPUI 安装器直接复用产品 token 与组件，窗口固定为紧凑的 **520×240**：许可确认、安装位置、进度、错误和维护页都在同一尺寸内完成，不靠大窗口留白。底栏只在左侧保留进度/状态文字；取消、返回、关闭等所有动作与主操作一起在右侧成组排列，主操作位于最右。安装完成页与维护页共用同一已安装界面：图标、标题和动作一致，只有辅助行区分首次安装说明与已安装版本；浏览按钮文案为“浏览… / Browse…”。表单输入与复选框边界使用 `control_stroke`（对页面背景与 muted 卡面均 ≥3:1），结构线仍使用 `border`。字体：西文衬线 Cormorant 已嵌入并用于字标、setup/uninstall 顶栏副标题（“安装程序/卸载程序”，按光学匹配取 `text_sm` 12.25 px）与页面标题（16 px），中文在系统已安装 Noto Serif SC 时回退到该字体，否则使用系统衬线回退；正文使用系统 UI 字体（Segoe UI / 微软雅黑 UI），不嵌入无衬线副本。页面标题行高取 `SERIF_LINE_HEIGHT`=1.25，避免圆字母底部过冲被行框裁切；顶栏词标与中英文副标题由 `WindowTextSystem::layout_line` 的实测 ascent/descent 计算基线，再按同一基线定位，不写死像素偏移。MSI 仍是唯一事务引擎，避免重写服务注册、回滚、升级与文件引用计数。

### 4.3 MSI 调用

- `cairn-setup` 使用 `MsiSetInternalUI(INSTALLUILEVEL_NONE)` + `MsiSetExternalUIRecord`，在专用线程运行 `MsiInstallProductW` / `MsiConfigureProductW`，通过无界 channel 把进度送回 GPUI。
- MSI 日志写入 `%TEMP%\CairnSetup-<timestamp>-*.log`；错误页提供“查看日志”。
- 不使用 `msiexec.exe` 子进程：同进程回调能提供真实动作/百分比，并且 Setup 进程本身就是管理员上下文。

### 4.4 载荷与清单

- `xtask` 在 MSI 构建完成后，把 MSI 追加到 `CairnSetup.exe` 尾部：`[MSI][length][crc32][reserved][CAIRNMSI]`；运行时校验长度与 CRC 后解包到管理员拥有的 `%ProgramFiles%\CairnSetup`，结束后删除。
- GPUI 自身会链接一个 `asInvoker` 清单资源；直接再嵌一个清单会触发 `CVT1100`。`xtask` 在链接后用 Windows SDK `mt.exe -outputresource:...;#1` 替换为 `crates/cairn-setup-bin/installer.manifest`（`requireAdministrator`、PerMonitorV2、longPathAware、Common-Controls 6）。
- 发布构建不读取任意 MSI 路径；调试构建允许 `CAIRN_SETUP_MSI_PATH` 便于 UI 联调。

### 4.5 数据清理边界

MSI 负责它安装的文件、服务、快捷方式、PATH 和注册表组件；Setup 负责产品数据目录、当前用户自启动值与注册卸载器无法覆盖的残留。两层都以“可重复执行”为前提：目录不存在、产品已移除或静默卸载重复调用都视为成功。

## 5. 构建与验证

```powershell
cargo build --workspace --release
cargo run -p xtask -- package
```

要求：WiX 5、Windows SDK（`mt.exe`，可通过 `CAIRN_MT` 覆盖）和 release 二进制。`xtask package` 与 CI 的 Installer Package job 会：

- 生成并编译 `cairn.msi`，校验 `wix msi validate`；
- 替换两个 GPUI exe 的管理员清单，验证清单包含 `requireAdministrator` 与 PerMonitorV2；
- 追加并回读 MSI 载荷，校验 SHA-256 与 CRC；
- 反编译 MSI，检查服务、隐藏 ARP、自定义卸载入口、PATH 与注册表清理；
- 失败时不替换 `dist/` 中上一次成功产物。

单元测试覆盖路径校验、版本比较、载荷编解码/损坏检测、MSI 错误码映射、i18n 占位符与主题对比度。真实安装/卸载仍需在干净 Windows VM 上验证服务注册、UAC、首次启动和卸载后的数据清理。

## 6. 已知边界

- 未完成 Authenticode 签名（`REL-02`）；在签名前不得把产物描述为已签名发布。
- 真实 MSI 安装/升级/卸载矩阵、Burn 注册迁移和静默安装需在干净 Windows VM 执行。
- 安装器跟随系统语言，不提供语言选择器；与 `docs/SPEC.md` 的界面语言策略一致。
