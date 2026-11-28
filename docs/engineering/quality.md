# 质量门禁与测试

> 测试通过标准以实际命令输出为准；未验证必须显式标注。

## 1. 自动化测试与质量门禁体系

### 1.1 分层测试策略

| 测试层级 | 工具与方式 | 验证目标与覆盖内容 |
|:---|:---|:---|
| **单元测试** | `cargo test` | 覆盖语法解析 AST、运算符优先级、通配符、过滤条件、日期与大小计算及各模块协调 Seam |
| **属性与差分测试** | `proptest` / `query_differential` | 查询解析无 panic、通配/正则 DFA/NFA 一致性、`NamePool` 插入取回与 UTF-8 折叠不变量 |
| **NTFS / 索引一致性测试** | `cairn-service` `sync_engine::tests` | 内存 `VolumeIndex` 上的 MFT/USN、硬链接重命名保留、万次随机操作增量与全量重建零差异（SM-5）及单卷自愈 |
| **安全与权限测试** | `cairn-service` `security::tests` | 校验默认与管理员 SDDL（SY/BA/IU 授权、NU/WD 拒绝）、内核安全描述符构造与目录/配置 DACL 收紧 |
| **UI 与无障碍测试** | `CairnApp` / 无头结构化契约 + 真实窗口验收 | 快捷键注册表、AccessKit 角色、导航焦点、布局与视觉渲染需经过实机窗口核验 |
| **输入法专项测试** | 手工与集成核验清单 | 微软拼音、搜狗拼音、微信输入法三款主流 IME 组合无丢字、不重字、候选窗不遮挡 |
| **端到端性能与全账本基准** | `xtask bench-query --check` | 固定 10 万条多级目录合成语料下统计 `VolumeIndex::memory_footprint()` 完整堆账本（空闲态与单列懒排序态 `<= 125 B/record`）并校验查询 P95 回归上限 250ms；不带 `--check` 时对给定语料运行同一账本并校验空闲态内存上限（语料缺失时回退到同一合成夹具）。SM-1 的 400 万条 P95 目标由发布前 release 实测。本基准不包含 Everything 对照 |

### 1.2 静态检查与回归门禁
代码变更合入与构建交付必须无条件通过：
1. `cargo fmt --all --check`（遵循根目录 `rustfmt.toml`）
2. `cargo clippy --workspace --all-targets -- -D warnings`（继承根目录 `[workspace.lints]`，含 `unsafe_op_in_unsafe_fn = "deny"`、`elided_lifetimes_in_paths = "warn"`、全部 8 项 clippy `deny` 规则 `undocumented_unsafe_blocks`/`missing_safety_doc`/`await_holding_lock`/`too_many_arguments`/`large_enum_variant`/`dbg_macro`/`todo`/`unimplemented`、4 项 clippy `warn` 规则 `uninlined_format_args`/`redundant_closure_for_method_calls`/`unnested_or_patterns`/`manual_let_else` 与 `rustdoc` 断链检查，零警告）
3. `cargo doc --workspace --no-deps`（`RUSTDOCFLAGS=-D warnings` 零警告）
4. `cargo run -p xtask -- check`（CI 的 `Repository Gates` 使用 `--no-default-features` 在 `ubuntu-latest` 上统一执行 `check-i18n`、`check-docs`、`check-size`、`check-hygiene` 与版本治理，覆盖排除 `#[cfg(test)]` 后的本地化键值及 `{ $var }` 插值变量对齐、核心文档集与根相对/工作区相对链接、2000 行上限、`[workspace.lints]` 继承、15 处纯安全入口 `#![forbid(unsafe_code)]`、`cairn-service`/`cairn-content` 的 manifest 与 `Cargo.lock` 传递依赖隔离、8 项 `deny` lint 反抑制、按模块限定名登记的 `#[ignore]` 测试台账及全仓非调研文档/注释/文案规范）；`check-hygiene` 同时校验 `.comment-baseline`（JSON schema：`schema_version/unit/generated_by/files/items`，逐项 ID）：拒绝回归、stale 基线、同文件等量替换、任何 `#[allow]`/`#[expect]`、`/* */` 块注释、非 live `SAFETY:` 与失效/越界决策链接；并读取 `.comment-debt` 校验 open 项与基线一致。清理后只允许用 `check-comments --update` 下调基线；任何增加或注释文本变更必须显式 `--allow-increase` 并按新增注释审查。
5. `cargo deny check`（许可证、安全漏洞与 yanked 检查；仓库已提供 `deny.toml`）
6. `cargo run --release -p xtask -- bench-query --check`（CI `Release / Engine` 在 release 编译后直接运行同机 `CairnXtask.exe bench-query --check`，执行全账本内存与查询延迟回归门禁）
7. 版本治理：`cargo run -p xtask -- check` 同时校验当前产品版本字面量只允许出现在 `Cargo.toml`、`Cargo.lock` 与 `CHANGELOG.md`（若存在）；成员 manifest 必须使用 `version.workspace = true`；打包前要求数字三段式与 MSI 字段上限；comment manifest 的 `schema_version` 来自 `COMMENT_SCHEMA_VERSION`。

注释策略：公共 API 文档（含 clap/doctest 消费）保留；私有/内部 `///` 与 `//!` 不是公共 API 例外，新增需评审；当前自动化预算只覆盖非豁免 `//`，文档注释的可见性预算列为后续项。`xtask` 的 `comment_check` 单测覆盖回归、stale、等量替换、孤儿 `SAFETY:`、越界/失效链接、字符串误报、空白/行内抑制、基线重置与增额拒绝；`version_check` 单测覆盖扫描边界、外部版本碰撞、UTF-16、成员继承与 MSI 范围。

纯文档变更仅在 `ubuntu-latest` 执行 `check-docs`、`check-hygiene` 与版本治理（`--docs-only`），跳过所有 Windows 编译、测试与发布作业，`Code Quality & Static Analysis`、`Dependency Policy`、`Domain & System Test Suite` 与 `Installer Package` 均以最小开销通过。代码变更必须通过全部必需门禁，`Installer Package` 在 Release 构建与 `Test / Engine & Tooling` 完成后执行。

`deny.toml` 只针对 `x86_64-pc-windows-msvc`；许可证白名单与 GPUI 栈的 unmaintained 例外均有理由记录。漏洞与 yanked 不设全局忽略，新增 ignore 必须附上游依据。

### 1.3 中文输入法（IME）验收清单
对微软拼音、搜狗拼音、微信输入法分别执行以下 6 项标准用例：
1. 连续输入长拼音字符串并选词上屏，验证无多余字符、无重复字符、无丢字。
2. 在拼音组合期间使用 Backspace 退格、方向键移动游标、数字键选词，缓冲区状态与高亮保持稳定。
3. 检查输入法候选窗始终紧随光标右下方，当主窗口位于屏幕底部边缘时，候选窗安全上翻或避让，不遮挡输入行。
4. 在搜索框内反复切换中英文模式（Shift 键与 CapsLock 键），行为自然不产生乱码。
5. 在输入框已有中文前缀（如 `content:架构 `）后继续输入拼音，验证光标与选区坐标计算准确稳定。
6. 在 1.0×、1.25×、1.5×、1.75× 等多显示器缩放比例下重复 1~5 项验证，位置坐标保持精确。

### 1.4 关键技术验收清单

| 验收项目 | 验收通过标准 |
|:---|:---|
| GPUI Kit Input + 三款主流输入法 | 完整通过 §1.3 专项清单 1~6 项 |
| 虚拟 DataTable 渲染 100 万行 | 连续高速滚动稳定 >= 60 FPS，内存平稳无泄漏 |
| CJK 字体回退、高 DPI、多显示器 | 中文字符无豆腐块方块字，跨缩放比拖动排版与光标无位移错位 |
| MFT 流式分块解析吞吐量 | 满足 IDX-02 与 NFR-01，NVMe 首次全量扫描 <= 15 秒 |
| 管道访问控制与安全性 | 标准交互用户可正常连接；远程与未授权连接被严格阻断 |
| Tantivy + CJK bigram 索引 | 中英混排召回完全满足 CNT-03，索引体积控制在预期预算内 |
| 托盘、单实例与事件循环协同（全局热键 `Ctrl+Alt+Space`） | 三者协同稳定运行，多实例启动正确唤醒前台窗口，无死锁 |
| Windows 10 (1809+) 与 Windows 11 兼容性 | 两个系统版本下后台服务与桌面端均能顺畅启动并执行各项功能 |
