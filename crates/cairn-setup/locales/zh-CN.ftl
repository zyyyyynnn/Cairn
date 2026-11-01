app_title = Cairn
setup_subtitle = 安装程序
uninstall_subtitle = 卸载程序
window_minimize = 最小化
window_close = 关闭

license_title = 许可协议
license_name = Apache License 2.0
action_view_license = 查看完整许可
error_license_required = 接受许可条款后继续。
error_open_license = 无法打开许可协议。重试。

license_accept = 我接受这些条款

location_title = 安装位置
install_summary = 为这台电脑上的所有用户安装到 Program Files，需要 100 MB 空间。
action_browse = 浏览…

progress_title = 正在安装 Cairn
progress_removing_old = 正在移除旧版本
progress_preparing = 正在准备安装
progress_copying = 正在复制文件
progress_service = 正在注册后台服务
progress_finishing = 正在完成安装
progress_do_not_close = 安装完成前，保持此窗口打开。
progress_do_not_close_uninstall = 卸载完成前，保持此窗口打开。
progress_percent = { $percent }%

finish_title = Cairn 已安装
finish_body = 后台服务会建立索引；首次搜索可能需要几分钟。
finish_restart = 重启电脑以完成安装。

uninstall_missing_title = 未找到 Cairn
uninstall_missing_body = 这台电脑上没有安装 Cairn，因此无需卸载。

error_uninstall_title = 卸载未完成
error_uninstall_msi_unknown = 卸载未完成，Cairn 仍在这台电脑上。查看日志了解原因，然后重试。
error_uninstall_msi_cancelled = 卸载已取消，Cairn 仍在这台电脑上。
error_uninstall_msi_busy = 另一个安装或卸载程序正在运行。等待其完成后重试。
error_uninstall_not_installed = 没有找到 Cairn 的安装记录。
error_uninstall_msi_platform = 这个安装记录不支持当前版本的 Windows。使用支持当前版本的 Cairn 安装程序进行卸载。
error_uninstall_msi_fatal = Windows 安装程序未能完成卸载。查看日志后重试。
error_uninstall_msi_invalid = Windows 安装程序无法读取 Cairn 的安装数据。移除剩余文件会同时移除 Cairn 的设置和注册表项，且无法撤销。
error_uninstall_cleanup_failed = 部分 Cairn 文件未能移除。查看日志了解原因。

error_title = 安装未完成
error_msi_unknown = 安装未完成，现有文件没有被删除。查看日志了解原因，然后重试。
error_no_payload = 安装包缺少必要文件。重新下载 CairnSetup.exe。
error_msi_cancelled = 安装已取消。
error_msi_busy = 另一个安装程序正在运行。等待其完成后重试。
error_msi_invalid = 安装包不完整或已损坏。重新下载安装程序。
error_msi_cleanup_failed = 清理未完成。查看日志了解原因。
error_msi_platform = 这个安装包不支持当前版本的 Windows。下载适用于当前版本的安装程序。
error_msi_fatal = Windows 安装程序未能完成操作。查看日志后重试。
error_legacy_failed = 先在“设置 > 应用”中卸载旧版本，然后重试。
error_not_installed = 没有找到 Cairn 的安装记录。
error_launch = 无法启动 Cairn。从开始菜单打开。
error_open_log = 无法打开日志文件。重试。

maintenance_installed = 版本 { $version }
maintenance_newer = 这台电脑上已有较新版本 { $version }。

uninstall_title = 卸载 Cairn
uninstall_body = 将移除 Cairn 程序、命令行工具、后台服务、索引、设置和注册表项（适用于所有用户）。
uninstall_progress_title = 正在卸载 Cairn
uninstall_progress_working = 正在移除 Cairn 文件和设置
uninstall_restart = 重启电脑以完成 Cairn 的移除。
uninstall_done_title = 卸载完成
uninstall_done_body = Cairn 已从这台电脑移除。
uninstall_files_removed_title = Cairn 文件已移除
uninstall_record_remaining = Windows Installer 中仍保留 Cairn 的安装记录。文件已移除，但之后的安装可能认为 Cairn 已安装。重新运行 Cairn 安装程序，再卸载 Cairn 以清除记录。
uninstall_cleanup_incomplete = 部分 Cairn 数据未能移除。查看日志了解详情。

path_empty = 输入安装文件夹。
path_not_absolute = 输入完整的本地路径，例如 C:\Program Files\Cairn。
path_network = 不能安装到网络位置。选择本地文件夹。
path_root = 选择一个文件夹，而不是磁盘根目录。
path_outside_program_files = 将 Cairn 安装到 Program Files 下。
path_system = 不能直接安装到 Windows 或 Program Files 根目录。选择子文件夹。
path_file = 这个路径指向一个文件。选择文件夹。
path_invalid = 路径包含 Windows 不允许的字符。从文件夹名称中移除“< > : " / \ | ? *”字符。

action_continue = 继续
action_back = 上一步
action_install = 安装
action_close = 关闭
action_retry = 重试
action_uninstall = 卸载
action_remove_remaining = 移除剩余文件
action_open_app = 打开 Cairn
action_finish = 完成
action_view_log = 查看日志
