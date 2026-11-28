# 测试与验证

本文件规定 Cairn 的测试分层、价值判定与验证记录方式。新增或修改行为时先确定它属于哪一层，再决定测试写在哪里。

## 分层与落点

| 层 | 适用对象 | 位置 | 例子 |
| --- | --- | --- | --- |
| 纯逻辑 | 无系统调用、输入输出都是值 | `src` 内 `#[cfg(test)] mod tests` | 预算计算、排序比较、过滤器解析、错误码映射 |
| 公开入口 | crate 的公开函数与协调器 Seam，用内存或伪造端口夹具调用 | `src` 内或 `crates/*/tests/` | `DirectorySizeIndex::build`、`StartupCoordinator`、`SearchCoordinator`、`RowPresentationCache`、`FrnLookup` |
| 协议与管道往返 | 真实帧编解码、真实命名管道上的请求与响应 | `crates/*/tests/` | 请求到响应的完整往返、版本拒绝、超长帧、超时与取消 |
| 性能与内存账本门禁 | 10 万条多级目录合成语料上的索引字节预算与查询延迟回归 | `xtask bench-query --check` | `IndexMemoryFootprint` 空闲与单列排序态 `<= 125 B/record`、查询 P95 回归上限 250ms；SM-1 的 400 万条目标由发布前 release 实测 |
| 权限与真机 | 裸盘读取、服务安装、UAC、MSI | 仅受控手工或 `--ignored` 层 | 实机索引、服务生命周期、同意对话框 |

一个行为只在一层测一次，放在最接近用户的那一层；下层不重复上层已经断言过的结果。

查询语法的差分测试以原始查询字符串为输入的朴素参考实现对照生产 `parse_query -> execute_query`，参考实现不共享生产 lexer/AST；随机查询同时断言命中与非命中，避免两侧共享同一缺陷时结果相互抵消。差分测试只建模 QRY-02 的文本文法（AND / OR / 否定 / 分组 / 引号），QRY-03 的函数、修饰符、通配符与正则值语义由精确命中用例覆盖。端到端查询回归断言精确命中集合，防止查询静默退化为全库。

`cairn-app` 的公开入口在 `src` 内以 `CairnApp` 上下文覆盖：启动通过 `StartupCoordinator` 与 `TestBootstrapPorts` / `AppRuntimePorts` 注入免副作用端口；就地重命名的打开、替换与取消（`rename_selected` / `cancel_rename`），查询历史后退（`go_back`），停止查询后的状态与 id（`cancel_in_flight_query`），以及取消后的状态刷新不发起查询（`refresh_results`）；启动去重、搜索防抖取消与每帧渲染缓存通过对应 Seam（`StartupCoordinator`、`SearchCoordinator`、`RowPresentationCache`）以无 OS 真弹窗方式验证；列表几何测试通过 `#[cfg(test)]` 渲染夹具（`SourceRowsHarness`、`JournalHeaderHarness`、`JournalRowsHarness`）在无生产后门的情况下测量真实排布边界。

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

属性增量重抽（USN）：在安装版服务运行、属性索引开启时，创建、修改并删除一个探针图片，并用 `CairnCli "probe dimensions:<宽>x<高>"` 查询，确认结果随文件更新，删除后结果与 `indexed_records` 回落。该路径读取真实 USN 日志，无卷访问的 CI 无法构造。

服务配置的安全写入（`ServiceConfig::save_atomic_secure` 的受限 ACL）需要 `SeRestorePrivilege`：非提权运行时，`apply_config_toml` 只可能返回 `could not persist` 拒绝，测试接受该路径；成功路径在提权 PowerShell 中执行 `cargo test -p cairn-service admin_only_query` 并确认通过，确认 `service.toml` 的所有者与 DACL 已受限。

当前清单：裸盘 MFT/USN 实机索引与增量、服务安装/启动/卸载、UAC 同意对话框的名称与图标、MSI 升级与卸载。

## 被忽略的测试

`#[ignore]` 只用于无法在默认 `cargo test` 中构造前置条件的测试；理由与执行入口随测试登记，新增或删除时同步更新本表。

| 测试 | 忽略理由 | 运行方式与前条件 |
| --- | --- | --- |
| `cairn-service` `sync_engine::content_pipeline::tests::backfill::a_full_backfill_reaches_ready_and_indexes_existing_files` | 测试 spawn 真实 `CairnWorker`，需要先构建 Worker；测试通过 `FixedWorkerBinaryLocator` 注入工作区构建的二进制，生产通过 `AdjacentWorkerBinaryLocator` 在服务可执行文件旁查找。 | CI 的 `Test / Engine & Tooling` 作业在编译引擎与工具测试集后直接运行已产出的测试二进制；本地先 `cargo build -p cairn-worker` 再运行 `cargo test -p cairn-service a_full_backfill_reaches_ready_and_indexes_existing_files -- --ignored`。 |
| `cairn-service` `tests::platform_seam::property_index_extracts_through_real_worker_and_recovers_from_corrupt_files` | 测试 spawn 真实 `CairnWorker` 进行扩展属性抽取与损坏文件隔离，需要先构建 Worker。 | CI 的 `Test / Engine & Tooling` 作业在编译引擎与工具测试集后直接运行已产出的测试二进制；本地先 `cargo build -p cairn-worker` 再运行 `cargo test -p cairn-service --test platform_seam property_index_extracts_through_real_worker_and_recovers_from_corrupt_files -- --ignored`。 |
| `cairn-service` `sync_engine::journal_reconciliation::tests::test_live_scan_reconciles_with_the_real_change_journal` | 需要启用 USN 日志的真实 NTFS 卷，并以管理员身份读取裸卷。 | 提权后执行 `cargo test -p cairn-service test_live_scan_reconciles_with_the_real_change_journal -- --ignored`；`C:` 必须已启用 USN 日志。 |
| `cairn-app` `shell::os::tests::a_deleted_file_lists_and_restores_through_the_shell` | 手工验证：经 Shell 在真实回收站创建并恢复一个探针文件。 | 仅在改动 Shell 管道时执行 `cargo test -p cairn-app a_deleted_file_lists_and_restores_through_the_shell -- --ignored`；探针限定为临时目录中的 `cairn-bin-round-trip.txt`，不得操作其他回收站项。 |
| `cairn-app` `shell::update::tests::the_update_check_reaches_the_release_api` | 需要访问 GitHub releases API 的真实网络；CI 不保证出网。 | 在网络可用时执行 `cargo test -p cairn-app the_update_check_reaches_the_release_api -- --ignored`；HTTP/TLS 依赖升级后用它确认请求能穿过 TLS 后端与系统代理。 |
| `cairn-setup` `platform::tests::staging_root_is_locked_to_administrators` | 需要管理员权限对安装包暂存目录写入 `SYSTEM` 与 `Administrators` 受保护 DACL。 | 提权后执行 `cargo test -p cairn-setup staging_root_is_locked_to_administrators -- --ignored`。 |
| `xtask` `package::tests::real_package_builds_msi_and_setup` | 需要 WiX 5、`mt.exe` 与 `target/release` 下的发布二进制。 | CI `Installer Package` 作业用 `package-test.exe package::tests::real_package_builds_msi_and_setup --ignored --nocapture --exact` 执行；本地构建 release 资产后执行 `cargo test -p xtask real_package_builds_msi_and_setup -- --ignored`。 |

## CI 门禁

`.github/workflows/ci.yml` 按正交依赖拆分为 Ubuntu 静态门禁与 Engine / Desktop 双路 Windows 作业，不重复编译无关依赖：

- `Repository Gates`（`ubuntu-latest`）：执行 `cargo run -p xtask --no-default-features -- check`（纯文档变更传入 `--docs-only` 只跑 `check-docs`、`check-hygiene` 与版本治理）。
- `Lint & Rustdoc`（`windows-latest`，纯文档变更跳过）：执行 `cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo doc --workspace --no-deps`。
- `Code Quality & Static Analysis`（`ubuntu-latest`）：汇总 `Repository Gates` 与 `Lint & Rustdoc` 的结果，作为必需状态检查。
- `Dependency Policy`（`ubuntu-latest`，纯文档变更跳过）：执行 `cargo fmt --all --check` 与预编译 `cargo deny check`。
- `Test / Engine & Tooling`（`windows-latest`，无 GPUI / NASM）：单次编译并测试 `cairn-protocol` `cairn-core` `cairn-ntfs` `cairn-content` `cairn-client` `cairn-worker` `cairn-service` `cairn-cli` `xtask`，并直接运行 `cairn-service` 内容回填与扩展属性抽取 `#[ignore]` 端到端测试、导出 `package-test.exe`。
- `Test / Desktop & Setup`（`windows-latest`）：编译并测试 `cairn-ui` `cairn-app` `cairn-setup` `cairn-setup-bin`。
- `Domain & System Test Suite`（`ubuntu-latest`）：汇总两个测试作业结果，作为必需状态检查。
- `Release / Engine`（`windows-latest`）：发布编译 `cairn-service` `cairn-cli` `cairn-worker` `xtask`，并在同机产物上直接执行 `CairnXtask.exe bench-query --check`。
- `Release / Desktop`（`windows-latest`）：发布编译 `cairn-app` `cairn-setup-bin`。
- `Installer Package`（代码变更用 `windows-latest` + WiX 缓存，纯文档变更在 `ubuntu-latest` 空转通过）：待发布产物与 `package-test.exe` 就绪后运行 `real_package_builds_msi_and_setup`。

新增测试默认必须被上述命令覆盖；无法进入 CI 的测试在“需要权限的验证”或“被忽略的测试”中登记。
