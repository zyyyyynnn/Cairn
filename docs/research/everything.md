# Everything（voidtools）深度调研：1.5 索引平台全档案

> 研究对象：Everything 1.5 线，当前版本 1.5.0.1424b Beta。Cairn 的规范以 [SPEC.md](../SPEC.md) 与 [product/](../product/) 为准；横向竞品对比见 [cross-form.md](cross-form.md)。

---

## 0. 结论摘要

1. 1.5 的实质是把 1.x 的"文件名索引器"扩展为**属性 + 内容 + 文件夹 + 服务器共享**的索引平台，服务器面（HTTP/ETP/Everything Server）整体插件化（[Plugins](https://www.voidtools.com/support/everything/plugins/)）。
2. 数据获取层仍是 MFT 直读 + USN 增量；1.5 新增 FAT/FAT32/exFAT 索引（默认每日重扫）与虚拟文件夹索引，ReFS 有独立索引页（[t=9796](https://www.voidtools.com/forum/viewtopic.php?t=9796)、[Indexes](https://www.voidtools.com/support/everything/indexes/)）。
3. folder size 索引以**每目录 8 字节**常驻内存并实时维护，是"目录尺寸进常驻索引"的唯一公开量化参照（[Options → Indexes](https://www.voidtools.com/support/everything/options/)）。
4. 安全模型是"服务 + 签名对端校验 + 只供文件名"三件套；1.5 新增用户会话内提权索引进程作为免服务的备选路径（[Everything Service](https://www.voidtools.com/support/everything/everything_service/)）。
5. 没有任何可视化：仍是纯结果列表，2026-08 的内置 Treemap 请求无开发者回应（[t=17866](https://www.voidtools.com/forum/viewtopic.php?t=17866)）。

## 1. 定位与发展方向

- 官方对新特性的自我表述（[1.5 更新记录帖](https://www.voidtools.com/forum/viewtopic.php?t=9787)首帖，2021-03-13）：Property indexing, searching, displaying and sorting；Dark mode；Update in background；Faster searching；Natural sort；Index Journal；Everything Server；Find duplicates；Index virtual folders；Undo System；Mix files and folders；FAT indexing；Tabs；Findbar；Sessions；Folder sidebar。改进项含 Weighted searches、Hard link tracking、Omit results、Ignore Punctuation/white space、Prefix and suffix、Plugin support。
- 三个扩展方向的官方口径：
  - **内容**："Everything 1.5 adds support for content indexing. Enabling content indexing will make finding text inside files instant"（[Content Indexing 帖 t=9793](https://www.voidtools.com/forum/viewtopic.php?t=9793)）。
  - **文件夹**："Folder indexing allows Everything to include any folder in the Everything index, such as folders on local drives, external drives, network drives, network shares, cloud folders, and virtual folders"（[Folder Indexing](https://www.voidtools.com/support/everything/folder_indexing/)）。
  - **服务器**：Everything Server 让用户 "Share a centralized index with multiple users" 并 "Keep client indexes up to date"（[Everything Server](https://www.voidtools.com/support/everything/everything_server/)）；Enterprise 页宣称可管理 "indexes for thousands of clients"（[Enterprise](https://www.voidtools.com/en-us/enterprise)）。
- **架构方向**：原内置服务器变为插件（"The ETP/FTP/HTTP servers are now plug-ins in Everything 1.5."，[Plugins](https://www.voidtools.com/support/everything/plugins/)），检索器本体保持单文件形态，扩展能力外置——Cairn 的"服务内核 + 可选能力"分层与其同构。

## 2. 版本与组件矩阵

| 组件 | 版本 | 分发形态与说明 | 来源 |
| --- | --- | --- | --- |
| Everything 1.5.0.1424b | **Beta** | x64/x86/ARM64/ARM 安装包、MSI 与便携 ZIP；附 SHA256 校验和文件 | [Downloads](https://www.voidtools.com/downloads/) |
| HTTP Server 插件 | 1.0.5.6 | x64/x86/ARM64/ARM + `src.zip`（GitHub 开源） | 同上 |
| ETP/FTP Server 插件 | 1.0.2.5 | 仅 x64/x86 + `src.zip` | 同上 |
| Everything Server 插件 | 1.0.4.5 | 仅 x64/x86；"requires Everything 1.5.0.1408 or later" | [Downloads](https://www.voidtools.com/downloads/)、[Plugins](https://www.voidtools.com/support/everything/plugins/) |
| ES 命令行 | 1.1.0.38 | x64/x86/ARM64/ARM + `src.zip`（[voidtools/es](https://github.com/voidtools/es)） | [Downloads](https://www.voidtools.com/downloads/) |
| 附属 | Language Pack / SDK 164 KB / voidImageViewer 1.0.0.15 | 官网同页分发 | 同上 |

- 版本线状态：1.5 帖 2021-03-13 以 alpha 1237a 开帖（[t=9787](https://www.voidtools.com/forum/viewtopic.php?t=9787)）；1410b–1412b（2026-05-14/15）起宣布 "Everything 1.5 is now beta"（同帖更新记录）；1424b 发布于 2026-10-15（同帖）。
- 1424b 修复项含 "improved Everything Service connection security" 与 "improved Everything IPC security"；1423b 修复 "a security issue to help prevent DoS attacks using complex wildcard searches with the HTTP server plugin"（[t=9787](https://www.voidtools.com/forum/viewtopic.php?t=9787)）。

## 3. 数据获取层深潜：MFT、USN、FAT、Folder Indexing

### 3.1 MFT 初始索引与 USN 监控

- void 对机制的直接表述（[t=12371](https://www.voidtools.com/forum/viewtopic.php?t=12371)）："Everything reads the NTFS Master File Table (MFT) to quickly index your files."；"The USN Journal is not used when indexing your files."；"Everything uses the USN Journal to keep your indexes up to date."。
- USN 事件流："The system will write file system changes to your USN Journal. Everything will see these changes immediately and update its database appropriately."；负载预期 "Hopefully you only have a few hundred file system changes a minute."（[t=12371](https://www.voidtools.com/forum/viewtopic.php?t=12371)）。
- USN 日志归属与参数（[Options → NTFS](https://www.voidtools.com/support/everything/options/)）："The USN Journal is managed by the NTFS driver (not Everything)."；容量建议 "If Everything is rebuilding the database after restarting Windows, try increasing this size to at least 32768 KB."；Allocation delta 控制"日志达到上限时从起点删除多少字节"。ReFS 页同样声明 "The USN Journal is managed by the ReFS driver (not Everything)"，且 "Allocation delta is currently ignored for ReFS volumes"。
- USN 记录版本：void 提示调用方需让 READ_USN_JOURNAL_DATA 请求版本与 USN_RECORD 结构匹配，"Everything uses version 3.3"（原文如此，[t=12371](https://www.voidtools.com/forum/viewtopic.php?t=12371)）。
- 日志缺口回退：论坛版主 NotNull 描述——长时间未运行后 "Everything will notice that the USN Journal is no longer holding the needed entries … and Everything will fall back to building the index for this drive from scratch. From -- you probably guessed it already -- the Master File Table."；void 补充 "The USN Journal is maintained by the NTFS driver. (Not Everything)"、"The system will typically re-create the USN Journal on next boot"（[t=12779](https://www.voidtools.com/forum/viewtopic.php?t=12779)）。
- 初始索引进度可中断："A progress bar is shown in the status bar for the initial index"，可 `Ctrl+Break` 取消，"will allow you to manually setup your own indexes"；"Everything will now use a separate thread for each device for faster indexing"（[t=9796](https://www.voidtools.com/forum/viewtopic.php?t=9796)）。
- 数据库持久化（1.5）："The database is now periodically saved to disk"，计划为每日凌晨 4 点且"仅当计算机空闲 1 分钟"；错过的保存延到下次关闭搜索窗口时执行（[t=9796](https://www.voidtools.com/forum/viewtopic.php?t=9796)）。路径 `%LOCALAPPDATA%\Everything\Everything.db`，运行期常驻内存（[Indexes](https://www.voidtools.com/support/everything/indexes/)）。
- Recent changes：`Index recent changes` 维护一张独立"最近变更"排序库，可用 `rc:` 搜索；`Load recent changes from USN journal` 把整卷 USN 日志灌入该库（[Options → Indexes](https://www.voidtools.com/support/everything/options/)）。
- 索引范围与排除：NTFS 卷支持 `Include only`——"Specifies which folders are indexed on this volume. Use a semicolon (;) delimited list of folder paths to include."；Exclude 页按属性排除：hidden、system、"Exclude not content indexed"（P 标志）、"Exclude files with the recall on data access attribute"——后者官方说明动机是 "Enable to prevent downloading content from the cloud."（[Options](https://www.voidtools.com/support/everything/options/)）。默认索引内容口径："Everything always indexes name and path information."；"Everything indexes file size and date modified information by default."（[Indexes](https://www.voidtools.com/support/everything/indexes/)）。
- 监控可暂停：`/monitor_pause` 与 `/monitor_resume` 搜索命令挂起/恢复全部卷监控（[Indexes](https://www.voidtools.com/support/everything/indexes/)）。

### 3.2 FAT/FAT32/exFAT（1.5 新增，重扫口径）

- "Everything 1.5 introduces FAT/FAT32/exFAT and virtual folder indexing."；"FAT volumes are rescanned daily by default."（[t=9796](https://www.voidtools.com/forum/viewtopic.php?t=9796)）。
- FAT 页标注 "Requires Everything 1.5"；逐项能力：Include in database、Attempt to monitor changes（尽力监控）、Buffer size（"Network shares are limited to 65536 bytes"、"Changes will be missed if Everything is unable to process this buffer fast enough"）、Rescan on full buffer、Update（重扫计划）（[Options → FAT](https://www.voidtools.com/support/everything/options/)）。
- Fast rescan（按目录修改时间只扫变化目录）对 FAT 不可用："FAT volumes are not supported."（[t=9796](https://www.voidtools.com/forum/viewtopic.php?t=9796)）。FAT 索引是否需要管理员，官方文档未说明（未找到公开数据）。

### 3.3 Folder Indexing：无提权路径

- "Folder indexing does not require administrative privileges or the Everything service."；"Standard users can index folders when running the portable version of Everything without administrator access."（[Folder Indexing](https://www.voidtools.com/support/everything/folder_indexing/)）。
- 监控与重扫："Everything monitors indexed folders for changes."；"Everything schedules a daily rescan of the indexed folders to detect changes that have been missed while Everything was not running."；错过计划时下次启动补扫（高级项 `folder_update_rescan_asap` 可关）。网络位置监控缓冲固定 64 KB，`Rescan on full buffer` 作兜底；Fast rescan "only works for NTFS folders"（[Folder Indexing](https://www.voidtools.com/support/everything/folder_indexing/)）。
- 边界："Everything tracks folders by their path, not by their physical volume"（盘符复用会互相覆盖）；离线索引保留在库中；虚拟文件夹可索引 "Mobile phones, Cloud drives, SD Cards, FTP sites, Control Panel, Windows Libraries, Recycle Bin"（[Folder Indexing](https://www.voidtools.com/support/everything/folder_indexing/)、[t=9796](https://www.voidtools.com/forum/viewtopic.php?t=9796)）。

### 3.4 ReFS 与 junction

- "Everything will automatically index all fixed ReFS volumes."（[Indexes](https://www.voidtools.com/support/everything/indexes/)）；ReFS 页含 Include in database / Enable USN Journal / Monitor changes 等与 NTFS 同构的选项（[Options](https://www.voidtools.com/support/everything/options/)）。
- "Everything does not follow junctions with NTFS/ReFS indexing."（[Indexes](https://www.voidtools.com/support/everything/indexes/)）。

## 4. Folder size 索引深潜

- 选项口径（[Options → Indexes](https://www.voidtools.com/support/everything/options/)）："Index folder size: Include folder size information in the index."；"Size information is maintained in real time."；"Requires an additional 8 bytes of memory per folder."；"Folder size will not include excluded files."。文件尺寸同口径：每文件 8 字节；日期字段各 8 字节/条目，属性 4 字节，fast size sort 另需 4–8 字节/文件。
- 计算与语义（void，2024-11-14，[t=15770](https://www.voidtools.com/forum/viewtopic.php?t=15770)）："Everything calculates the folder size from all the child files and grandchild files in your index."；junction 不跟随："Everything doesn't follow folder junctions with NTFS parsing."、"Everything will report folder junctions with a size of 0."。
- API 面：folder size 索引关闭时 "The SDK will return UINT64_MAX (0xffffffffffffffffui64)"；1.5 新增 `Everything3_GetFolderSizeW`，且 "There shouldn't be any delay if you keep the pipe connection alive."；同帖预告 "Everything 1.5 IPC will use pipes instead of WM_COPYDATA which is much faster"（[t=15770](https://www.voidtools.com/forum/viewtopic.php?t=15770)）。
- 实时性来源：文件夹尺寸随 USN 事件即时重算（"maintained in real time"），`size:` 查询与排序因此即时（[Options → Indexes](https://www.voidtools.com/support/everything/options/)）。
- 与磁盘分析器的口径差异：Everything 的 folder size 是**索引内文件名条目的逻辑尺寸聚合**（排除项不计、junction 报 0、子树成员随索引增减），磁盘分析器（如 WizTree）按"物理占用 + 硬链接去重"口径输出快照（WizTree 官方："correctly handles hard linked files (doesn't count them more than once)"，[diskanalyzer.com](https://diskanalyzer.com/)）。Everything 对硬链接在 1.5 中 "now tracks changes to all hardlinks" 但 "deleted hard links are ignored"（[t=9796](https://www.voidtools.com/forum/viewtopic.php?t=9796)）；folder size 是否对硬链接去重，官方未说明（未找到公开数据）。

## 5. Content 内容索引深潜

- 启用与范围（[Content Indexing 帖 t=9793](https://www.voidtools.com/forum/viewtopic.php?t=9793)）：经高级项 `content_indexing_enabled` 开启，"Content is indexed in the background."（状态栏进度可右键暂停/恢复）；默认 `content_indexing_include_only_files` 为 `*.doc;*.docx;*.pdf;*.txt;*.xls;*.xlsx`；定位是 "Content indexing is intended for indexing user documents only."，"For the best performance, set an include only folder."
- 内存口径："Everything will keep content in memory."；"Indexing over 1GB of text is not recommended."；实际规模看 Debug → Statistics 的 "Content data size"（[t=9793](https://www.voidtools.com/forum/viewtopic.php?t=9793)）。
- handler 分层：1.5 起内置 handler 优先——void："Everything 1.5 uses a built-in content handler for known text/plain files, then falls back to ifilters."（[t=15938](https://www.voidtools.com/forum/viewtopic.php?t=15938)）。具体规则（[t=9793](https://www.voidtools.com/forum/viewtopic.php?t=9793)）：长清单内的扩展名按纯文本读；"For other file types, Everything will read the content with the extension-associated iFilter."；"If there is no extension-associated iFilter, the file content is treated as text/plain."；"Windows 10 comes with iFilters built-in to read common files types such as docx and pdf."
- IFilter 加载接口：早期 "Currently, Everything only supports IPersistStream to load the file."；"Everything 1.5.0.1381a adds support for loading iFilters with IInitializeWithStream."（[t=15171](https://www.voidtools.com/forum/viewtopic.php?t=15171)）。
- 进程隔离：1416b 增加独立的 IFilter 进程选项（"added a separate process option for iFilters"，即 `content_ifilter_separate_process`，同版把内容索引选项移入 Advanced）；1419b 修复开启该选项后内容索引不生效的问题（[t=9787](https://www.voidtools.com/forum/viewtopic.php?t=9787)）。
- 查询面：`content:` 仅搜已入索引内容（"content: will only search files included by your index content settings."），支持 `content:<abc | 123>` 或运算；未入索引用 `notindexed:content:` 即时读盘；void 另给出口径覆盖开关 "Use fromdisk:content: to override your indexed content."（[t=9793](https://www.voidtools.com/forum/viewtopic.php?t=9793)、[t=15938](https://www.voidtools.com/forum/viewtopic.php?t=15938)）。
- 文本类型表可编辑：纯文本扩展名清单由 `text_plain_extensions` 配置项承载，经 Tools → Debug → Config 修改（[t=9793](https://www.voidtools.com/forum/viewtopic.php?t=9793)）。

## 6. 服务与安全模型

- 服务定位："The Everything service allows Everything to quickly index NTFS volumes and monitor NTFS volumes for changes without running Everything as administrator."；默认安装、开机自启、按需自动拉起；"The Everything service uses about 1 MB of RAM and has no noticeable CPU usage."（[Everything Service](https://www.voidtools.com/support/everything/everything_service/)）。
- 安全三原则（同页原文）："Any local user can use the Everything service to obtain a list of all NTFS filenames."；"Only the Everything process code-signed by voidtools PTY LTD can access the Everything service."；"The Everything service can only provide filename information." 且 "It cannot be used to access the files themselves."
- 1.5 的备选路径："Run indexing process as administrator"——"Run an additional Everything indexing process with administrative privileges to index and monitor system NTFS volumes."，"When enabled, Everything can run as a standard user without the Everything service"；与服务的取舍："The Everything indexing process is the same as the Everything service."，差别只在"The indexing process runs in your user session and requires a UAC prompt when Everything starts"，而服务"runs in the background as a Windows service and does not require a UAC prompt"（[Everything Service](https://www.voidtools.com/support/everything/everything_service/)、[Options → General](https://www.voidtools.com/support/everything/options/)）。
- 两条路径都不可用时退到 Folder 索引："but indexing is much slower."（[Everything Service](https://www.voidtools.com/support/everything/everything_service/)）。
- 持续加固：1424b 改进服务连接与 IPC 安全，1423b 修 HTTP 插件 DoS（[t=9787](https://www.voidtools.com/forum/viewtopic.php?t=9787)）；IPC 层已换管道（§4，[t=15770](https://www.voidtools.com/forum/viewtopic.php?t=15770)）。

## 7. 1.5 平台能力

| 能力 | 口径 | 来源 |
| --- | --- | --- |
| Property indexing | 属性提取优先级：User values → .metadata.efu → 属性 ADS → Windows Property System → Summary Information → Everything 内置 handler（"built-in property handler only support common file formats (eg: mp3/mp4/jpg)"）；"all the properties for a file are rescanned when it is modified"；Fast Sort 使排序 "instant and persistent"；未入索引用 `notindexed:` | [Properties](https://www.voidtools.com/support/everything/properties/) |
| Index Journal + Undo | "a list of all the changes made to your Everything index"；默认 1 MB（约 5000 条变更，可调）；回收站删除记为 File Move；"Think of it as a global file system undo feature."（撤销重命名/移动）；"The Index Journal is essential to the Everything Server as an audit of all changes to your index."；"Clients use the Index Journal on the server to synchronize a local copy of the server index." | [Index Journal](https://www.voidtools.com/support/everything/index_journal/) |
| Result Omissions | "hide files and folders from your search results without removing them from the index"，且 "Everything continues to monitor omitted files and folders for changes."；切换 "instant"；存 `Omit Results.csv`；`disable-result-omissions:` 可绕过 | [Result Omissions](https://www.voidtools.com/support/everything/result_omissions/) |
| Find duplicates | 默认按 name；`dupe:size;sha256` 时 "The SHA-256 value is only calculated for files with the same size."；最多 8 个属性；配套 `distinct:` / `unique:` | [Find Duplicates](https://www.voidtools.com/support/everything/find_duplicates/) |
| Sessions | 1.5.0.1372a 引入；记忆窗口/标签/搜索/过滤/排序/列等；存 `%APPDATA%\Everything\Session.json`，改动后至多 15 秒落盘（`session_store_save_delay`），启动时轮转为 `Session.backup.json` 再恢复 | [t=14903](https://www.voidtools.com/forum/viewtopic.php?t=14903) |
| Tabs | 1.5.0.1357a 实验引入、1361a 正式；`Ctrl+T` 新建、`Ctrl+Tab` 循环、1362a 起可恢复最近关闭的标签 | [t=13987](https://www.voidtools.com/forum/viewtopic.php?t=13987) |
| Plugin 体系 | 服务器功能插件化；"Plugins created by voidtools are automatically enabled."，第三方插件需手动启用；设置存 `Plugins.ini`；`-safe-mode` 免插件启动；有 1.5 Plugin SDK | [Plugins](https://www.voidtools.com/support/everything/plugins/) |
| Everything Server | 默认端口 14630，"Connections are encrypted with AES."；依赖 Index Journal 同步客户端（建议 Journal 上限调至 32768 KB）；"The Everything Server shares indexes and index changes only"（文件仍走 Windows 共享）；默认只共享 "filename, size and date modified"；支持用户账号、路径重映射、Group Policy | [Everything Server](https://www.voidtools.com/support/everything/everything_server/) |

### 7.1 服务器面与 CLI 细节

- **HTTP Server**（[HTTP](https://www.voidtools.com/support/everything/http/)）：默认端口 80；"The HTTP server uses HTTP Basic authentication."，"The username and password are sent using Base64 encoding."；"The HTTP server does not support TLS."；官方警示 "Do not expose an HTTP server directly to the internet. Use a VPN or an SSH tunnel instead."；可关闭 "Allow file download" 只留浏览/搜索，支持 JSON 输出。
- **ETP/FTP Server**（[ETP](https://www.voidtools.com/support/everything/etp/)）："ETP is FTP with the Everything extension."（`SITE EVERYTHING` 扩展，FTP `FEAT` 探测）；"The ETP server only provides a limited window of search results."；"ETP has been superseded by the Everything Server, which does not have this limitation."；凭据 "sent as raw text and are not encrypted"，同样警示勿暴露公网；支持四种链路类型（原路径 / Windows 共享 / 管理共享 / 经 ETP/FTP 服务器自身取文件）。
- **ES CLI**（[CLI 文档](https://www.voidtools.com/support/everything/command_line_interface/)）："Everything must be installed and running."——ES 经 IPC 与运行中的 Everything 通信（退出码 8 = "The Everything IPC window was not found"），`-ipc1/-ipc2/-ipc3` 选择 IPC 版本；输出格式 `-csv -efu -json -m3u -tsv -txt` 与 `-export-*` 系列导出；1.5 专属能力：任意属性排序（如 `-sort length`）、扩展列、全部 journal 选项（`-journal -watch -from-date -action-filter` 等）；退出码 0–9，`-no-result-error` 时无结果返回 9。

## 8. 性能特征（官方公开口径）

- [FAQ](https://www.voidtools.com/faq/)："A fresh install of Windows 11 (about 250,000 files) will take about 5 seconds to index."，"will use about 35 MB of ram" 与 "less than 14 MB of disk space"；"1,000,000 files will take about 1 minute."，"about 100 MB of ram and 45 MB of disk space."
- 平台与前提：支持 "Windows XP, Vista, Windows 7, Windows 8, Windows 10 and Windows 11"；"NTFS indexing requires the Everything service or running Everything as administrator."（[FAQ](https://www.voidtools.com/faq/)）。
- 增量与资源："Everything is very efficient with updating its database and should consume minimal resources."；负载预期每分钟数百条变更（[t=12371](https://www.voidtools.com/forum/viewtopic.php?t=12371)）；服务常驻 "about 1 MB of RAM"（[Everything Service](https://www.voidtools.com/support/everything/everything_service/)）。
- 落盘节流：数据库每日 4 点、空闲 1 分钟才保存（[t=9796](https://www.voidtools.com/forum/viewtopic.php?t=9796)）。
- 400 万以上规模的官方数字：未找到公开数据。

## 9. 许可与平台

- 免费软件，官方确认可用于商业环境（[t=7508](https://www.voidtools.com/forum/viewtopic.php?t=7508)）；Everything Server 在企业环境托管需 Site License，"The Everything Server is free for personal use."，企业客户获 "Access to priority email support"（[Enterprise](https://www.voidtools.com/en-us/enterprise)）。
- 1.5 分发覆盖 x64/x86/ARM64/ARM；HTTP 插件与 ES 同为四架构并开源（GitHub `src.zip`）；发布物附 SHA256 校验和（[Downloads](https://www.voidtools.com/downloads/)）。
- 数据与配置路径：数据库 `%LOCALAPPDATA%\Everything\Everything.db`（[Indexes](https://www.voidtools.com/support/everything/indexes/)）；会话/插件设置/忽略表在 `%APPDATA%\Everything\`（§7 各来源）。

## 10. 对 Cairn 的可借鉴点

对照 [SPEC.md](../SPEC.md) 现状：

1. **DSV-01 / IDX-06（尺寸字段成本与口径）**：每目录 8 字节、每文件 8 字节、日期各 8 字节、fast sort 4–8 字节（[Options → Indexes](https://www.voidtools.com/support/everything/options/)）是"常驻索引维护目录尺寸"的公开成本锚点，可校准 Cairn 自底向上聚合（400 万 ≤ 100 ms）的内存预算。但注意口径差：Everything 聚合的是索引内条目逻辑尺寸且"excluded files 不计"、junction 报 0；Cairn DSV-01 要求整盘物理占用与 Win32 卷信息误差 ≤ 2%，两者是不同承诺，不能混用口径。
2. **IDX-02（MFT 全量基准）**：void 确认 MFT 直读（[t=12371](https://www.voidtools.com/forum/viewtopic.php?t=12371)），FAQ 公开 25 万文件约 5 秒、100 万约 1 分钟（[FAQ](https://www.voidtools.com/faq/)）——Cairn 的 400 万 ≤ 15 s 在 100 万/60 s 的官方斜率内成立；官方无 400 万级数字，REL-04 基准需自测。
3. **IDX-03 / IDX-07（USN 细节与状态可见性）**：值得吸收的是工程细节——USN 记录结构版本匹配（"Everything uses version 3.3"，[t=12371](https://www.voidtools.com/forum/viewtopic.php?t=12371)）、日志容量 32768 KB 级建议（[Options](https://www.voidtools.com/support/everything/options/)）、缺口后整卷 MFT 重建（[t=12779](https://www.voidtools.com/forum/viewtopic.php?t=12779)）。差异点：Everything 的重建是静默的，没有"卷非同步"的用户可见状态；Cairn IDX-07 的零静默过期与卷状态机正是对它的超越，不是照抄对象。
4. **CNT-04（内容抽取隔离）**：`content_ifilter_separate_process` 单独进程开关（[t=9787](https://www.voidtools.com/forum/viewtopic.php?t=9787)）与 1381a 的 `IInitializeWithStream` 支持（[t=15171](https://www.voidtools.com/forum/viewtopic.php?t=15171)）印证 Cairn `cairn-worker.exe` 隔离方向；分层也可对照——Everything 内置文本 handler 优先、IFilter 回退（[t=15938](https://www.voidtools.com/forum/viewtopic.php?t=15938)）与 Cairn CNT-02/08 的"纯文本内置 + 富文档 trait"一致。资源取向相反：Everything 内容驻内存（1 GB 上限建议），Cairn Tantivy 落盘（CNT-07），后者在内容规模上无上限包袱。
5. **SVC-02（对端认证）**："Only the Everything process code-signed by voidtools PTY LTD can access the Everything service" 是 DACL 之外的**签名对端校验**思路；Cairn 已有 SVC-02 的 DACL + `PIPE_REJECT_REMOTE_CLIENTS` 与 SVC-03 协议版本校验，REL-02 的 Authenticode 签名落地后可把"校验对端签名者"补进服务端连接校验，形成同款双保险。
6. **SVC-07（免提权降级谱系）**：Everything 提供三档——服务（无 UAC、但任何本地用户可查全盘文件名）、用户会话内提权索引进程（每次启动弹 UAC）、Folder 索引（无提权但慢）（[Everything Service](https://www.voidtools.com/support/everything/everything_service/)、[Folder Indexing](https://www.voidtools.com/support/everything/folder_indexing/)）。Cairn 的 `cairnd --user-mode` 只走账户可读目录、不提权不读裸盘，比"会话内提权进程"保守；"无 UAC 常驻服务 + 明示代价"的取舍说明值得写进 IPC 安全文档作对照。
7. **IDX-09（排除 vs 遗漏的分层）**：Result Omissions 证明"不移出索引、继续监控、瞬时切换的显示层忽略"是可行且有用的独立层（[Result Omissions](https://www.voidtools.com/support/everything/result_omissions/)）；Cairn 现有排除项是"不入索引"的单层设计，未来可把"显示层忽略"作为 IDX-09 的补充语义（服务端零改动）。
8. **SVC-06（变更流复用）**：Index Journal 既是用户可查的变更审计与全局撤销（[Index Journal](https://www.voidtools.com/support/everything/index_journal/)），又是 Server 客户端同步的数据源——一条变更流喂三类消费者。Cairn 的 USN 追赶流（IDX-03）与 SVC-06 状态广播可借鉴同思路：同一事件流同时服务索引、状态推送与未来审计。

## 11. 附录：一手来源索引

| 主题 | 来源 |
| --- | --- |
| 版本与组件矩阵 | [Downloads](https://www.voidtools.com/downloads/)、[1.5 更新记录 t=9787](https://www.voidtools.com/forum/viewtopic.php?t=9787)、[What's New](https://www.voidtools.com/support/everything/whats_new/) |
| MFT/USN 机制 | [t=12371（void）](https://www.voidtools.com/forum/viewtopic.php?t=12371)、[t=12779](https://www.voidtools.com/forum/viewtopic.php?t=12779)、[Options](https://www.voidtools.com/support/everything/options/)、[Indexes](https://www.voidtools.com/support/everything/indexes/) |
| 1.5 索引扩展（FAT/虚拟文件夹/持久化/多线程） | [t=9796（void）](https://www.voidtools.com/forum/viewtopic.php?t=9796)、[Folder Indexing](https://www.voidtools.com/support/everything/folder_indexing/) |
| Folder size | [Options → Indexes](https://www.voidtools.com/support/everything/options/)、[t=15770（void，2024-11-14）](https://www.voidtools.com/forum/viewtopic.php?t=15770) |
| Content 索引 | [Content Indexing t=9793（void）](https://www.voidtools.com/forum/viewtopic.php?t=9793)、[t=15938](https://www.voidtools.com/forum/viewtopic.php?t=15938)、[IInitializeWithStream t=15171](https://www.voidtools.com/forum/viewtopic.php?t=15171) |
| 服务与安全 | [Everything Service](https://www.voidtools.com/support/everything/everything_service/)、[Options → General](https://www.voidtools.com/support/everything/options/) |
| 1.5 平台能力 | [Index Journal](https://www.voidtools.com/support/everything/index_journal/)、[Result Omissions](https://www.voidtools.com/support/everything/result_omissions/)、[Find Duplicates](https://www.voidtools.com/support/everything/find_duplicates/)、[Properties](https://www.voidtools.com/support/everything/properties/)、[Sessions t=14903](https://www.voidtools.com/forum/viewtopic.php?t=14903)、[Tabs t=13987](https://www.voidtools.com/forum/viewtopic.php?t=13987)、[Plugins](https://www.voidtools.com/support/everything/plugins/)、[Everything Server](https://www.voidtools.com/support/everything/everything_server/) |
| 服务器面 | [HTTP Server](https://www.voidtools.com/support/everything/http/)、[ETP Server](https://www.voidtools.com/support/everything/etp/)、[ES CLI](https://www.voidtools.com/support/everything/command_line_interface/) |
| 性能 | [FAQ](https://www.voidtools.com/faq/)、[t=12371](https://www.voidtools.com/forum/viewtopic.php?t=12371)、[Everything Service](https://www.voidtools.com/support/everything/everything_service/) |
| 许可与生态 | [Enterprise](https://www.voidtools.com/en-us/enterprise)、[商用确认 t=7508](https://www.voidtools.com/forum/viewtopic.php?t=7508)、[voidtools/es](https://github.com/voidtools/es) |
| 可视化空白 | [t=17866（2026-08，无官方回应）](https://www.voidtools.com/forum/viewtopic.php?t=17866) |
