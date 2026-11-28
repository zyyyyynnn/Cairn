# 索引与一致性防线

本文记录 Cairn 索引层的核心设计边界，作为架构设计、回归审查与测试验证的基准。

## 1. 数据真值与持久化

- **先落盘，再宣布同步完成**：状态上报必须基于实际持久化结果；索引快照写入未确认时不得向 UI 上报 Synced。
- **原子替换**：索引/配置文件采用临时文件加原子替换；异常退出不能留下半个可解析文件。
- 快照校验和覆盖实际写入的编码负载；记录计数与目录大小有效性位按固定头偏移读写，加载时按版本与长度校验，不信任超出文件内容的计数。
- 目录大小随记录增量维护：每个新文件沿父链加一次，扫描结束后整体聚合作为持久化权威值；墓碑祖先已在其删除时移除整棵子树总量。
- 索引快照的魔数与版本号（`INDEX_MAGIC` / `INDEX_VERSION`）必须与当前格式匹配；恢复流程不得把损坏或不完整快照当成有效索引。

## 2. USN 与增量修复

- 监控 USN Journal 时处理溢出、断电重置、跨卷变更和日志失效。
- 监控断线后能够回退到快速扫描与增量修复。
- 硬链接变更必须被识别；不能把链接计数变化误判为独立文件增删。
- 状态机测试必须驱动真实调用现场。只测纯辅助函数，调用现场的参数错误（例如 `tail_usn` / `had_changes`）仍可能全绿。
- 缓存 USN 位置必须属于当前 journal 且落在 `[first_usn, next_usn]` 窗口内；越界位置直接重建，避免永久停留在 `CatchingUp`。
- 全量扫描先记录 T0，扫描结束后回放其间的变化；回放未到 journal tail 前只能报 `CatchingUp`。每次轮询（包括空批次）都查询 tail：空读取不等于已到 tail。
- 无 USN Journal 的卷以 `SnapshotMode` 提供缓存快照；快照本身不证明新鲜度。
- USN `Created` 记录不携带文件大小，只有事件时间；去抖阶段重新读取磁盘元数据、大小与属性位，否则服务停机期间创建的文件会一直显示 0 B。
- 目录重命名不产生子项 USN 记录，却会改变整个子树的路径，可能把子树移入或移出排除规则。
- `Synced` 只能由 journal catch-up 或活跃目录 watcher 产生；`VolumeStatus::set` 拒绝 `Synced`，`restore` 只回滚卷曾持有的状态。
- 启动时先压缩旧缓存，回收服务数据记录与无界墓碑历史，再让卷服务查询。
- journal backlog 超过 64 MiB 时改用全新 `$MFT` 扫描，避免回放数十万条记录拖长冷启动。
- USN 轮询复用固定 512 KiB 读缓冲（`usn_read_buf`），冷热轮询不再重复分配。

## 3. 并发、内存与流式查询

- 不跨线程共享可变状态；`MultiVolumeEngine` 遵循全局锁序：
  `persist_lock -> config_apply_lock -> volumes -> folder_sources -> folder_watchers -> property_pipeline.(tables -> refresh -> status) -> index_journal`。
  次要边：`volumes -> config | summary_cache`、`folder_sources -> folder_scanning`、`config_apply_lock -> config`。
- 叶锁：`volume.exclusion_cache`、`content_pipeline.(commands, cancel_lock, inline_clear_lock)`。`usn_read_buf` 不是叶锁：USN 轮询持有它跨越 `volumes`、`config`、`property_pipeline.tables` 与 `index_journal`，新增路径必须沿用该顺序。
- 响应信封从 `summary_cache` 读取卷摘要，避免握手与状态探测排在秒级查询的卷读锁之后。
- `config_apply_lock` 覆盖从配置比较到内容管线切换的全过程，反向切换不能超越先提交的变更。
- 路径排除在每次查询/内容批次读取一次：段规则由卷级子树状态表回答，其他规则按已解析规则集检查完整路径，逐记录不重新解析。
- 全量视图分页偏移按可见行计数，而不是原始顺序槽位；深分页用累计计数块定位。
- 会话缓存总条目上限 16M（约 256 MiB），按 LRU 淘汰。
- 客户端最后一个连接释放时，连接注册锁一直持有到会话清理完成，重连的客户端不能在清理前登记新会话。
- 卷表的 `generation` 随 `clear`、`ingest_items` 与压缩递增，分页会话据此拒绝失效的记录索引；压缩保留按 FRN 记录的待处理集合（数据变更、重命名、内容删除），启动压缩不得清空。
- 服务自有目录（索引、内容索引、配置与日志）按 FRN 排除；内容抽取不得回读这些目录，否则索引自身的写入会作为新内容再次进入索引。
- 内容索引的 Tantivy 写入在 Worker 中完成；服务进程只在没有内容线程时执行显式清空（`clear_index_inline` 会打开 `ContentWriter`）。
- 查询求值通过只读借用投影 [`SourceView`](../../crates/cairn-core/src/query.rs) 与 `EvalContext` 执行，使多卷索引与文件夹源共享同一过滤、评分及排序语义。
- 管道查询使用超时、背压、取消和分页；不在一次响应中构造全量结果。
- 结果表使用虚拟列表，只渲染可见行；文件记录保持紧凑，文件名进入共享池，避免每节点独立堆分配。
- UI 分页拉取不得在 `render` 中触发；使用偏移钩子或 `defer`，并设置防重入标记。

### 3.1 排除规则只有一套语义（IDX-09）

- 一条规则按**路径段边界**匹配：`node_modules` 命中名为 `node_modules` 的那一段，不是 `node_modules_cache` 这类只是前缀相同的目录；带分隔符的规则（`target\debug`）命中的是整段路径序列；`C:\Users` 这类盘符前缀命中到路径段边界为止。
- 文件名查询与内容索引使用同一套规则**语义**（路径段边界、通配符、绝对前缀）；两者读不同的规则集：文件名查询读 `excluded_paths`，内容索引叠加 `content_excluded_paths`，因此系统目录可以只在内容索引中被排除。
- 排除规则可由用户经 `SetConfig` 热改；文件名查询在下一次查询即采用新规则，内容索引在下一次建立时采用新规则；无需重启服务。

## 4. 内容索引隔离

- 内容索引默认关闭；启用后覆盖配置允许的卷与扩展名，跳过明确排除目录。
- 默认内容排除系统、应用与依赖目录：它们占正文语料的大头，文件名查询仍然覆盖。
- 正文解析/索引在工作进程中完成，宿主服务不承接第三方解析器的崩溃风险。
- 关闭内容索引先停止活动 worker 批次，再清理索引；关闭失败不得在活动配置下清空内容索引。
- 回填完成标记带格式版本；索引布局变化时递增，旧标记强制全量重建。
- 首次启用需要可见的建立进度与失败原因；建立完成前不得宣称内容可搜。关闭会停止建立、释放 Worker/searcher，并移除完成标记；下次开启会先清空并重建内容索引。“索引 > 内容索引”中的清空是显式删除；两者都不影响文件名索引。

## 5. 扩展属性索引

- 属性只来自 `CairnWorker` 内的内置纯 Rust 解析器，Shell 与第三方 property handler 不参与抽取；属性索引未启用时不分配侧表。
- 单个文件抽取失败只丢弃该文件的结果并计入失败数，进度继续推进；部分失败保持 `Ready` 并在日志中记录失败数，全部文件失败时状态置为 `Error` 并带上失败文件数。

## 6. 缺陷修复的验收格式

重复出现的同类缺陷必须同时交付：

1. 调用现场的精准修复；
2. 能解释复发的调用链溯源；
3. 会在缺陷重现时变红的回归测试。

测试应断言真实行为、关键边界或系统状态；不要用哈希快照、无需求的内部字符串或实现细节充当回归证据。

## 代码位置

- `crates/cairn-service/src/sync_engine/`：USN、索引日志、内容/属性双管线、`worker_runner.rs`、`folder_source.rs` 与 `volume_index/`（`mod.rs`、`presort.rs`、`refresh.rs`）。
- `crates/cairn-core/src/engine.rs`、`crates/cairn-core/src/query.rs`、`crates/cairn-core/src/syntax.rs`：`SourceView`、`EvalContext`、语法解析与查询过滤。
- `crates/cairn-core/src/dir_size.rs`、`crates/cairn-core/src/file_kind.rs`：目录后代大小聚合与文件类别判定。卷索引在构建与增量更新后按 `parent_idx` 链维护目录大小，表格的大小列、排序与 `size:` 过滤共用同一值。
