# 服务、IPC 与权限边界

本文描述 Cairn 的进程权限拓扑、命名管道协议和安全边界，代码位置见文末。

## 1. 进程拓扑

| 进程 | 权限 | 职责 |
| --- | --- | --- |
| `Cairn.exe` | 标准用户 | GPUI 界面、结果渲染、Win32 Shell 文件操作（`IFileOperation` / 回收站）；不直接读取 `$MFT` |
| `CairnCli.exe` | 标准用户 | 命令行检索、JSON/CSV 导出与索引管理入口；通过同一命名管道与服务交互 |
| `CairnService.exe`（服务） | LocalSystem | 后台索引、NTFS `$MFT`/USN Journal 读取、文件夹源监控、只读索引日志与服务数据目录维护 |
| `CairnWorker.exe` | 低优先级隔离子进程 | Job Object（`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`）、`BELOW_NORMAL_PRIORITY_CLASS` 与后台 I/O 隔离的纯 Rust 文档文本及扩展属性解析子进程；拒绝沿路径遍历重解析点；无命名管道暴露 |

- `CairnService --console` 只用于开发/测试；生产必须由 Windows 服务承载。
- 服务名：`CairnService`。

## 2. 命名管道

| 名称 | 使用方 |
| --- | --- |
| `\\.\pipe\Cairn.Service` | 已安装的 LocalSystem 服务 |

- 桌面客户端只连接这一条服务管道；服务不可用时显示离线态。

## 3. 授权与协议

- 客户端身份由服务端从管道客户端的 **OS access token** 推导；`pid` 仅用于日志，`client_protocol_version` 只参与版本协商，二者都不作为权限依据。
- 无法推导身份的连接在 Windows 上直接断开，不服务任何帧；`CallerAuthorization::Unauthenticated` 仅用于非 Windows 构建与单元测试，除握手外的请求一律拒绝。
- `Hello` 与 `Cancel` 始终允许；索引配置是本地用户操作，`admin_only_query` 是唯一需要管理员的配置项，由配置写锁内的比较保护，并发提交不会各自基于过期值判断。
- 组枚举依赖客户端 `CreateFile` 授予 `SECURITY_IMPERSONATION`；级别不足时探针失败，调用方按标准用户处理。身份判决使用 `CheckTokenMembership`（计入禁用与 deny-only 组），`TokenGroups` 只用于日志审计。
- 管道拒绝远程客户端；DACL 由当前配置生成，DACL 变更对之后创建的实例生效。`admin_only_query` 变更在配置写锁内提交，下一次请求即生效；既有连接上的每个请求都会再次授权。
- 连接数与重请求并发分别由信号量限额，避免连接洪泛与 O(records) 请求同时占用内存和卷读锁。
- 协议版本：帧头与 `Hello` 握手携带显式 `PROTOCOL_VERSION`，与产品版本解耦，只在 wire 布局或消息语义变化时递增，防止未升级旧端误读错位字段。连接必须先完成 `Hello` 握手；协议版本不一致、未握手和未授权分别返回专用错误码。帧头版本在任何反序列化之前校验，版本不一致时按对端版本回帧。
- 查询结果采用会话游标分页，不在一次响应中反序列化完整结果集；查询应答的卷摘要必须在查询释放卷读锁后读取。
- 客户端连接使用退避重试；服务端每个连接独立处理，支持并发客户端。

### 3.1 错误码

`ResponsePayload::Error.code` 为机器可读错误码，常量与数值定义在 `cairn_protocol::message`，文本为英文技术描述，本地化由呈现层负责（UI-07）。

| 错误码 | 常量 | 含义 |
| --- | --- | --- |
| 400 | `ERROR_CODE_HANDSHAKE_REQUIRED` | 请求在 `Hello` 握手前到达，协议版本尚未协商 |
| 401 | `ERROR_CODE_ADMIN_ONLY_QUERY` | `admin_only_query` 已开启且调用方不是管理员 |
| 403 | `ERROR_CODE_UNAUTHORIZED` | 无法从访问令牌推导调用方身份，或策略拒绝该请求（含未认证调用方、非管理员修改 `admin_only_query`） |
| 409 | `ERROR_CODE_PROTOCOL_VERSION_MISMATCH` | 协议版本不一致，需同时更新应用与服务 |
| 410 | `ERROR_CODE_SESSION_EXPIRED` | 查询会话不存在或已过期（失效、重载或压缩后） |
| 416 | `ERROR_CODE_QUERY_TOO_COMPLEX` | 内容检索项超过 `MAX_CONTENT_TERMS`（16）或不同 `child:` 模式超过 `MAX_CHILD_FILTERS`（8），求值前整体拒绝 |
| 417 | `ERROR_CODE_INVALID_REQUEST` | 请求参数无效或引用了未知来源 |

## 4. 权限使用

- `SeRestorePrivilege`：服务数据目录 owner 修复所必需；只有该特权允许把任意有效 SID 设为 owner。`SeTakeOwnershipPrivilege` 单独只能把 owner 设为自己可合法声明的 SID，只有它时 elevated `--console` 会以 `ERROR_INVALID_OWNER` 失败。`SeRestorePrivilege` 在使用后按原状态释放，长期索引进程不保留该权限。
- 服务数据目录的 ACL 只允许 SYSTEM 与内置 Administrators；桌面客户端不写入服务数据目录。
- 服务数据根整体受保护，配置与日志随根目录覆盖，不逐个子目录枚举；启动时未被认证的 `service.toml` 不会因后续 reload 重新读取而变为可信。
- `Path::exists()` 不能用于创建决策：它把“不存在”和“存在但无权查询”都报为 `false`。存在性按 I/O 错误类型区分，只有 `NotFound` 才是缺失；`PermissionDenied` 表示目录存在但不可列出，应直接进入 ACL 检查而不是 `create_dir_all`。
- 创建与修复分离：先创建证明缺失的目录，再对每个位置独立执行 DACL/owner 修复；一个位置失败不阻止其余位置，首个失败最后返回。目录存在性由读取安全描述符本身证明，`READ_CONTROL` 是 owner 的隐式权限，不依赖目录项查询。
- DACL 写入只需对象 owner 的隐式 `WRITE_DAC`，非特权进程也能完成；受保护 DACL（`PROTECTED_DACL`）切断 `%ProgramData%` 对 `BUILTIN\Users` 的继承。`enforce` 先写 DACL 再修 owner，owner 修复失败不回滚 DACL，DACL 校验优先。
- owner 与 primary group 分开写；省略 group 是为了避免无法以 `SY` 为主组的进程连 owner 写入都被拒绝。
- 服务启动先完成数据目录的 ACL/owner 修复，修复失败则拒绝索引，不能在未受控的目录上运行。
- 索引/查询代码不得跨线程共享可变状态；管道读写必须带超时、背压与取消路径。

## 5. UAC 与提权操作

- 运行期唯一的管理员请求是启动已安装的服务，由 `Cairn.exe --start-service` 承担：提升的是产品自身的映像，同意对话框因此显示 Cairn 的名称与图标，服务名固定写在代码里，不接受任何参数指定。
- `--start-service` 出现时独占该进程：单独出现才执行启动，混入任何其他参数时以 `ERROR_INVALID_ARGUMENT` 退出，不初始化界面，管理员令牌不会被花在一次普通窗口上。
- 被提升的映像就是本进程所在目录里的 `Cairn.exe`，因此这次提权的可信度取决于该目录的写入权限：安装目录由安装器施加受保护 DACL，只有 SYSTEM 与 Administrators 可写。
- 需要管理员的验证/安装步骤合并为一个脚本，一次 UAC 提示，不按检查项分散弹窗。
- UAC 取消或超时后不循环重试；先完成所有非提权事项，再集中处理提权项。
- 提权脚本必须包含异常退出恢复逻辑，不能把服务或配置留在半完成状态。

## 6. 卸载

卸载必须移除服务注册、IPC 监听和程序目录；不得留下仍可连接的管道或服务残留。


## 7. 安全基线

| 安全措施 | 具体实现与验收标准 |
|:---|:---|
| **命名管道访问控制** | 管道创建时指定 `PIPE_REJECT_REMOTE_CLIENTS`；显式注入 Win32 SDDL 安全描述符 `D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)`，严格限制仅允许本地交互式登录用户连接 |
| **管道配置权限边界** | 未认证连接禁止修改服务端配置；本地已认证用户可提交索引配置，由 LocalSystem 服务校验并原子落盘，GUI 无需提权（SVC-05）；`admin_only_query` 仅管理员可改 |
| **索引文件存储权限** | 磁盘持久化缓存目录 `%ProgramData%\Cairn\` 严格限制仅 SYSTEM 与管理员组具备读写权限 |
| **Unsafe 代码严格收敛** | 15 处纯安全入口声明 `#![forbid(unsafe_code)]`（覆盖 `cairn-core`、`cairn-protocol`、`cairn-content`、`cairn-client`、`cairn-cli`、`cairn-service` 二进制入口、`cairn-worker` 二进制与解析模块、`cairn-setup-bin` 全部入口及 `xtask`）；Win32 FFI 与裸扇区 `unsafe` 收敛于 6 个 crate（`cairn-ntfs`、`cairn-service` 库、`cairn-app`、`cairn-setup`、`cairn-ui`、`cairn-worker` 库），所有 `unsafe` 块配 `// SAFETY:` 说明、所有 `unsafe fn` 配 `# Safety` 文档 |
| **第三方抽取进程隔离** | 富文档与外部文件内容/属性抽取在独立受限子进程 `CairnWorker.exe` 中执行；`cairn-service` 与 `cairn-content` 对工作进程及外部解析依赖（`lopdf`、`zip`、`quick-xml`、`chardetng`、`encoding_rs`）零直接及传递依赖 |
| **隐私第一原则** | 零遥测、零后台回传；除用户主动触发的更新检查（强制 HTTPS、GitHub 主机白名单与下载体积上限校验，下载后校验 SHA-256 并经 `MoveFileExW` 原子替换）外不发起网络请求；内容索引默认关闭，只有用户显式开启后才建立本地索引 |

## 代码位置

- `crates/cairn-protocol/src/message.rs`：管道名、协议版本、请求/响应类型。
- `crates/cairn-service/src/pipe_server/`：管道实例、DACL、握手与请求处理。
- `crates/cairn-service/src/sync_engine/worker_runner.rs`：`CairnWorker.exe` Job Object、低优先级与超时隔离。
- `crates/cairn-service/src/caller_auth.rs`：基于 access token 的调用方识别与授权。
- `crates/cairn-service/src/security.rs`：数据目录 ACL 与特权使用。
- `crates/cairn-service/src/main.rs`：`CairnService` 的 console / service 入口。
