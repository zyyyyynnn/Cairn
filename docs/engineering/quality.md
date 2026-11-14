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
| **端到端性能与全账本基准** | `xtask bench-query` / `--check` | 10 万条多级目录合成语料下统计 `VolumeIndex::memory_footprint()` 完整堆账本（空闲态与单列懒排序态 `<= 125 B/record`；`--check` 另设查询 P95 回归上限 250ms）；SM-1 的 400 万条 P95 目标由发布前 release 实测。本基准不包含 Everything 对照 |

### 1.2 静态检查与回归门禁
代码变更合入与构建交付必须无条件通过：
1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`（零警告）
3. `cargo doc --workspace --no-deps`（`RUSTDOCFLAGS=-D warnings` 零警告）
4. `cargo run -p xtask -- check-i18n`（`zh-CN.ftl` 与 `en-US.ftl` 翻译键 100% 对齐）
5. `cargo run -p xtask -- check-size`（任何 Rust 源文件不超过 2000 行）
6. `cargo run -p xtask -- check-docs`（必需工程文档与相对链接全部有效）
7. `cargo deny check`（许可证、安全漏洞与 yanked 检查；仓库已提供 `deny.toml`）
8. `cargo run --release -p xtask -- bench-query --check`（CI `Test / Tooling` 执行全账本内存与查询延迟回归门禁）

文档-only PR 只运行 `check-docs`；逐层测试、Tooling 与 Release job 跳过，Dependency Policy 与 Installer Package 以 no-op 成功结束。代码变更必须通过完整门禁，安装器打包等待质量检查通过。

`deny.toml` 只针对 `x86_64-pc-windows-msvc`；许可证白名单与 4 条 GPUI/font 栈的 unmaintained 例外均有理由记录。漏洞与 yanked 不设全局忽略，新增 ignore 必须附上游依据。

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
