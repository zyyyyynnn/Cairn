# 服务、IPC 与权限边界

本文描述 Cairn 当前的进程权限拓扑、命名管道协议和安全边界。规范以当前代码为准，代码位置见文末。

## 1. 进程拓扑

| 进程 | 权限 | 职责 |
| --- | --- | --- |
| `cairn.exe` | 标准用户 | GPUI 界面、结果渲染、用户交互；不直接读取 `$MFT` |
| `cairnd.exe`（服务） | LocalSystem | 后台索引、NTFS `$MFT`/USN Journal 读取、服务数据目录维护 |
| `cairnd.exe --user-mode` | 启动它的标准用户 | 服务不可用时的本地降级索引器；只遍历当前账户可读目录，跳过 reparse point |

- 用户模式索引器通过 `--parent-pid` 随桌面进程退出，不注册为服务。
- `cairnd --console` 只用于开发/测试；生产必须由 Windows 服务承载。
- 服务名：`CairnService`。

## 2. 命名管道

| 名称 | 使用方 |
| --- | --- |
| `\\.\pipe\Cairn.Service` | 已安装的 LocalSystem 服务 |
| `\\.\pipe\Cairn.User` | 用户模式索引器 |

两种管道分开命名，运行中的服务始终优先于本地扫描，两者不会争用同一实例。

## 3. 授权与协议

- 客户端身份由服务端从管道客户端的 **OS access token** 推导；请求中的 `pid` / `client_version` 只是提示，不可作为权限依据。
- 管道拒绝远程客户端；DACL 由当前配置生成。`admin_only_query` 变更对后续管道实例生效，并对既有连接上的每个请求再次授权。
- 协议版本：`PROTOCOL_VERSION = 4`。连接必须先完成 `Hello` 握手；版本不匹配、未握手和未授权分别返回专用错误码。
- 查询结果采用会话游标分页，不在一次响应中反序列化完整结果集。
- 客户端连接使用退避重试；服务端每个连接独立处理，支持并发客户端。

## 4. 权限使用

- `SeRestorePrivilege`：用于服务数据目录的 owner 修复；只有该特权允许把任意有效 SID 设为 owner。使用后按原状态释放。
- `SeTakeOwnershipPrivilege`：只在确有必要的所有权修复路径中使用；单独持有它并不能把 owner 设为任意 SID。
- 服务数据目录的 ACL 只允许 SYSTEM 与内置 Administrators；用户模式索引器不写入服务数据目录。
- 索引/查询代码不得跨线程共享可变状态；管道读写必须带超时、背压与取消路径。

## 5. UAC 与提权操作

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
| **Unsafe 代码严格收敛** | `cairn-core` 声明 `#![forbid(unsafe_code)]`；Windows API 与裸扇区操作收敛于 `cairn-ntfs`，每个 `unsafe` 块配 `// SAFETY:` 说明 |
| **第三方抽取进程隔离** | 富文档与外部文件内容抽取在独立 `cairn-worker.exe` 中执行，主服务不受异常格式或崩溃波及 |
| **隐私第一原则** | 零遥测、零回传、零网络通信；内容索引默认关闭，只有用户显式开启后才建立本地索引 |

## 代码位置

- `crates/cairn-protocol/src/message.rs`：管道名、协议版本、请求/响应类型。
- `crates/cairn-service/src/pipe_server.rs`：管道实例、DACL、握手与请求处理。
- `crates/cairn-service/src/caller_auth.rs`：基于 access token 的调用方识别与授权。
- `crates/cairn-service/src/security.rs`：数据目录 ACL 与特权使用。
- `crates/cairn-service/src/user_mode.rs`：用户模式降级索引器。
- `crates/cairn-service/src/main.rs`：`cairnd` 的 console / service / user-mode 入口。
