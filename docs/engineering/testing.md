# 测试与验证

本文件规定 Cairn 的测试分层、价值判定与验证记录方式。新增或修改行为时先确定它属于哪一层，再决定测试写在哪里。

## 分层与落点

| 层 | 适用对象 | 位置 | 例子 |
| --- | --- | --- | --- |
| 纯逻辑 | 无系统调用、输入输出都是值 | `src` 内 `#[cfg(test)] mod tests` | 预算计算、排序比较、过滤器解析、错误码映射 |
| 公开入口 | crate 的公开函数与协调器 Seam，用内存或伪造端口夹具调用 | `src` 内或 `crates/*/tests/` | `MultiVolumeEngine::treemap_tree`、`StartupCoordinator`、`SearchCoordinator`、`RowPresentationCache`、`FrnLookup` |
| 协议与管道往返 | 真实帧编解码、真实命名管道上的请求与响应 | `crates/*/tests/` | 请求到响应的完整往返、版本拒绝、超长帧、超时与取消 |
| 性能与内存账本门禁 | 多层级索引全账本字节预算与确定性查询延迟 | `xtask bench-query --check` | `IndexMemoryFootprint` 空闲与单列排序态 `<= 125 B/record`、多级目录查询 P95 阈值 |
| 权限与真机 | 裸盘读取、服务安装、UAC、MSI | 仅受控手工或 `--ignored` 层 | 实机索引、服务生命周期、同意对话框 |

一个行为只在一层测一次，放在最接近用户的那一层；下层不重复上层已经断言过的结果。

`cairn-app` 的公开入口在 `src` 内以 `CairnApp` 上下文覆盖：汇总块清单的进入、后退与退出（`open_block_listing` / `go_back` / `escape_query`），就地重命名的打开、替换与取消（`rename_selected` / `cancel_rename`），停止查询后的状态与 id（`cancel_in_flight_query`），以及取消后的状态刷新不再发起查询（`refresh_results`）；启动去重、搜索防抖取消与每帧渲染缓存通过对应 Seam（`StartupCoordinator`、`SearchCoordinator`、`RowPresentationCache`）以无 OS 真弹窗方式验证。

## 价值判定

测试断言聚焦于外部可观察的结果与关键边界：

- 验证外部可观察结果：返回值、协议帧、退出码、错误码及磁盘状态。
- 构造有效红绿周期：缺陷修复前先复现失败用例，修复后再转绿。
- 覆盖阈值临界点：精确断言预算边界值、溢出临界值及空输入状态。
- 保持断言高信息密度：断言体现业务契约与系统状态，避免针对内部中间状态、常量字面值或源码文本扫描进行机械镜像断言；禁止在生产模块内保留服务于单测的状态后门或行为绕行（可写状态、伪造分支、绕过生产路径）。跨模块测试确需只读观测时，暴露 `#[cfg(test)]` 无副作用访问器或真实的只读诊断接口，不改变生产行为。

## 强制规则

1. 行为改动必须在它的公开入口或显式 Seam 上有测试。只测私有 helper 不构成覆盖：预算、构建与调用方之间的偏差只有公开入口能暴露。
2. 修复缺陷先写能红的测试；该层无法在 CI 构造时，改为在 PR 中记录一次人工验证，并说明为什么无法自动化。
3. 新增公开接口同步新增测试；删除或收敛实现时同步清理只服务于旧实现的测试与临时测试钩子。
4. 测试不依赖执行顺序、真实用户目录与网络，临时目录统一用 `tempfile`。

## 需要权限的验证

只有真机才出现的差异（权限、SCM、UAC、DPI）走人工验证，方法固定为：

1. 把步骤写成脚本，经 `Start-Process -Verb RunAs` 以管理员执行，每一步写入日志文件。
2. 在 PR 的 Evidence 中给出日志关键行与结论。
3. 恢复机器状态：停止的服务重新启动，临时进程退出，临时文件清理。

需要管理员运行的开发入口：`CairnService.exe --console`（先停止已安装的 `CairnService`，验证结束后重新启动）。

全局热键验证：启动应用后按 `Ctrl+Alt+Space`，前台窗口应隐藏，再次按下应从托盘恢复并聚焦；先用其他程序占用该组合后启动，应用应提示一次且托盘仍可打开窗口。

服务配置的安全写入（`ServiceConfig::save_atomic_secure` 的受限 ACL）需要 `SeRestorePrivilege`：非提权运行时，`apply_config_toml` 只可能返回 `could not persist` 拒绝，测试接受该路径；成功路径在提权 PowerShell 中执行 `cargo test -p cairn-service admin_only_query` 并确认通过，确认 `service.toml` 的所有者与 DACL 已受限。

当前清单：裸盘 MFT/USN 实机索引与增量、服务安装/启动/卸载、UAC 同意对话框的名称与图标、MSI 升级与卸载。

## 被忽略的测试

`#[ignore]` 只用于无法在默认 `cargo test` 中构造前置条件的测试；理由与执行入口随测试登记，新增或删除时同步更新本表。

| 测试 | 忽略理由 | 运行方式与前条件 |
| --- | --- | --- |
| `cairn-service` `a_full_backfill_reaches_ready_and_indexes_existing_files` | 测试进程旁必须有真实的 `CairnWorker.exe`，需要先构建 Worker。 | CI 的 `Test / Backfill` 作业在构建 `cairn-worker` 后执行 `cargo test -p cairn-service a_full_backfill_reaches_ready_and_indexes_existing_files -- --ignored`；本地先 `cargo build -p cairn-worker` 再运行同一命令。 |
| `cairn-service` `test_live_scan_reconciles_with_the_real_change_journal` | 需要启用 USN 日志的真实 NTFS 卷，并以管理员身份读取裸卷。 | 提权后执行 `cargo test -p cairn-service test_live_scan_reconciles_with_the_real_change_journal -- --ignored`；`C:` 必须已启用 USN 日志。 |
| `cairn-app` `a_deleted_file_lists_and_restores_through_the_shell` | 手工验证：经 Shell 在真实回收站创建并恢复一个探针文件。 | 仅在改动 Shell 管道时执行 `cargo test -p cairn-app a_deleted_file_lists_and_restores_through_the_shell -- --ignored`；探针限定为临时目录中的 `cairn-bin-round-trip.txt`，不得操作其他回收站项。 |
| `cairn-app` `the_update_check_reaches_the_release_api` | 需要访问 GitHub releases API 的真实网络；CI 不保证出网。 | 在网络可用时执行 `cargo test -p cairn-app the_update_check_reaches_the_release_api -- --ignored`；HTTP/TLS 依赖升级后用它确认请求能穿过 TLS 后端与系统代理。 |
| `xtask` `real_package_builds_msi_and_setup` | 需要 WiX 5、`mt.exe` 与 `target/release` 下的发布二进制。 | CI `Installer Package` 作业用 `package-test.exe real_package_builds_msi_and_setup --ignored --nocapture` 执行；本地构建 release 资产后执行 `cargo test -p xtask real_package_builds_msi_and_setup -- --ignored`。 |

## CI 门禁

`.github/workflows/ci.yml` 在 `windows-latest` 上执行 `cargo clippy --workspace --all-targets -- -D warnings`、`cargo doc --workspace --no-deps`、`xtask check-i18n`、`xtask check-size` 与 `xtask check-docs`（在 `ubuntu-latest` 上执行 `cargo fmt --all --check` 与 `cargo deny check`），并按以下作业运行测试与回归门禁：

- `Test / Core & Protocol`：`cairn-protocol` `cairn-client` `cairn-core` `cairn-ui`
- `Test / Content & Filesystem`：`cairn-ntfs` `cairn-content`
- `Test / Service`：`cairn-service`
- `Test / Desktop`：`cairn-app`
- `Test / Backfill`：构建 `cairn-worker` 并运行 `cairn-service` 内容回填 `#[ignore]` 端到端测试
- `Test / Tooling`：`cairn-cli` `cairn-worker` `cairn-setup` `cairn-setup-bin` `xtask`，并执行 `cargo run --release -p xtask -- bench-query --check`

新增测试默认必须被上述命令覆盖；无法进入 CI 的测试在“需要权限的验证”或“被忽略的测试”中登记。
