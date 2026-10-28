# CLI 规范

> 对应需求 ID：`CLI-*`。本文是产品口径；CLI 核心与 TUI 尚未全部实现，实现状态见 `docs/SPEC.md` §3.10。

CLI 是与桌面端并列的第一等产品面，服务于脚本、CI 与 agent；它不是后台服务的薄包装。核心形态对标 `rg` / `fd`：非交互、可组合、结构化输出和稳定退出码优先，交互式 TUI 作为后续独立前端。

## 1. 安装与来源

- `CairnSetup` 随 Cairn 本体安装 `Cairn.exe`、`CairnService.exe` 与 `CairnCli.exe`，并把安装目录加入机器 PATH；`CairnService` 作为产品组件注册，不存在“CLI 单独安装服务”或“未安装服务”的正常模式。
- 查询来源按顺序解析：
  1. `Cairn.Service` 可用 → 服务索引，范围 `all`；
  2. 服务停止（例如桌面端选择了“可读文件”）→ 复用共享用户索引器 `Cairn.User`；不存在则按需启动，范围 `readable`；
  3. 用户索引器冷启动时等待就绪，进度只写 stderr；stdout/JSON 不被进度污染。
- CLI 不启动/停止系统服务、不修改服务启动类型、不触发 UAC；这些只属于桌面端的“搜索范围”切换。
- 用户索引器按需启动、10–15 分钟无租约后退出；GUI 打开时持有租约，CLI/agent 查询会续租；同一用户只允许一个实例。
- 不静默返回部分结果：冷启动默认等待完整可读索引；`--timeout` / `--no-wait` 可作为高级逃生选项，但仍必须在 `--json` 元数据中标记 `indexing`。

## 2. 输出契约

- 人类输出默认只写结果；诊断、进度和“当前为可读文件范围”的提示写 stderr；`--quiet` 可静默非错误提示。
- `--json` / `--jsonl` 至少包含：
  - `source: service | user-index`
  - `scope: all | readable`
  - `indexing: null | { state, processed?, total? }`
  - `total_matches` 与 `total_bytes`（匹配文件的自身大小之和；目录不重复计字节）
  - 结果路径使用绝对路径；需要管道分隔时用 `--null`。
- 退出码：`0` 有结果，`1` 无结果，`2` 使用错误/不可用（与 `rg` 语义一致）。
- 支持 `--files-with-matches`、`--null`、`--limit`、`--type`、`--sort`、`--content` 等检索参数；普通查询仍以文件名/路径为主。
- `content:` / `--content` 仅在服务可用且内容搜索可用时工作；可读文件模式、暂停或准备中时明确报不可用，不静默忽略。
- `CairnCli status --json` 只读展示来源、范围、索引/内容搜索状态与计数；不提供切模式或清空内容索引的命令。

## 3. 受控文件修改

- 命令与查询分离：`CairnCli file rename|move|delete ...`。
- 允许：重命名、移动、删除到回收站。
- 禁止：永久删除、创建/写入/改属性、切换索引模式、清空内容索引、启动/停止系统服务。
- 默认 dry-run；必须 `--apply --yes` 才执行；批量或查询结果集操作必须额外 `--all`。单一显式路径无需 `--all`，但仍需 `--apply --yes`。
- 输入必须是显式绝对路径或 stdin 路径列表；拒绝相对路径、UNC、`..` 和未解析的查询隐式结果。批量模式允许 agent 执行，但 `--all` 是对“整批结果”的显式确认。
- 不覆盖已有目标；删除只进系统回收站，回收站不可用时失败，绝不回退永久删除。
- 审计：每次 `--apply`（尤其批量）追加 JSONL，记录时间、agent 标识、命令、查询/输入、解析后的路径、每条结果与错误；审计文件只保存在当前用户目录。
- 服务保持只读；文件操作使用当前用户令牌与 Win32 Shell 文件操作，不经过 LocalSystem 服务。
- TUI 的交互式修改可以复用同一核心与审计；不得绕过 `--all` / `--apply` / dry-run 规则。

## 4. TUI

- `CairnCli tui` 是后续独立前端，复用同一查询/修改核心；不把全屏交互逻辑塞进脚本可用的 CLI。
- TUI 中的删除、移动、重命名必须显示与 CLI 相同的作用对象、scope、dry-run/审计信息；不新增后台服务控制能力。

## 5. 验收

- 服务停止时 CLI 自动使用共享用户索引器，`--json` 报告 `scope=readable`；服务恢复后下一次查询使用 `scope=all`。
- 冷启动等待期间 stdout/JSON 干净，stderr 有真实进度；超时/失败有稳定退出码。
- 批量修改必须显式 `--all`、`--apply`、`--yes`，并产生可核查的审计记录；无永久删除路径。
- 同一个 agent 查询在 GUI/TUI/CLI 中返回一致的路径与 scope 语义。
