# Cairn 简体中文界面资源
#
# 文案按界面语言规则书写，不做逐词翻译：目的地用名词、命令用动词、状态用形容词。
# 短控件标签与单行状态标题不加句末标点；说明句与安装器正文加句末标点。`…` 只用于
# “会打开对话框或需要更多输入”的命令，以及输入占位符。快捷键写在键表与 AccessKit 的
# keyshortcuts 里，不写进标签文字。

# 顶栏只写品牌名，产品职责由窗口本身说明。
app_title = Cairn

# 侧边导航：目的地是名词，不带"管理/面板"这类外壳词。
sidebar_locations = 位置
sidebar_drives = 驱动器
sidebar_no_drives = 尚未发现驱动器
sidebar_recycle_bin = 回收站
drive_label = 驱动器 { $drive }

# 顶栏菜单：与品牌同行的六个顶层菜单。
menu_file = 文件
menu_edit = 编辑
menu_view = 查看
menu_search = 搜索
menu_tools = 工具
menu_help = 帮助


result_list_label = 搜索结果列表
table_header_label = 结果表列标题
result_table_label = 结果表

toggle_case_label = 区分大小写
toggle_whole_word_label = 全字匹配
toggle_match_path_label = 匹配路径
toggle_regex_label = 正则表达式

filter_all = 所有文件
filter_audio = 音频
filter_doc = 文档
filter_exe = 可执行文件
filter_image = 图片
filter_video = 视频
filter_zip = 压缩包
filter_folder = 文件夹

col_name = 名称
col_path = 位置
col_original_location = 原位置
col_size = 大小
col_modified = 修改日期
col_deleted = 删除日期
col_created = 创建时间
col_ext = 扩展名

# 溢出菜单里的条目：结果、查询、索引三段各自成组。
query_undo = 撤销
query_redo = 重做
query_cut = 剪切
query_paste = 粘贴
edit_select_all = 全选
query_clear = 清空搜索
sort_reverse = 反向排序

status_objects_count = { $count_text } 个项目
status_selected_count = 已选择 { $count_text } 项
selection_limit_title = 已达到选择上限
selection_limit_message = 已选择前 { $count_text } 项。请缩小查询范围后再试。
reveal_limit_title = 未打开全部文件夹
reveal_limit_message = 选中项分布在 { $count } 个文件夹，超过一次最多打开 { $limit } 个文件夹的限制。请缩小选择范围后重试。
status_objects_summary = { $count_text } 个项目 { $size }

volume_capsule = { $drive }：{ $state }
volume_state_synced = 已同步
volume_state_catching_up = 同步中 { $percent }%
volume_state_catching_up_pending = 同步中
volume_state_rebuilding = 正在重建索引
volume_state_offline = 离线
volume_state_snapshot_mode = 快照模式

# 搜索框占位符
search_placeholder = 搜索文件或文件夹…
recycle_bin_filter_placeholder = 搜索回收站…
recycle_bin_filter_label = 搜索回收站
search_box_label = 搜索文本

ctx_open = 打开
ctx_open_path = 打开所在文件夹
ctx_copy_path = 复制完整路径
ctx_copy_original_location = 复制原位置
ctx_copy_name = 复制文件名
ctx_delete = 删除
ctx_restore = 还原
ctx_restore_all = 还原所有项目
ctx_delete_permanently = 永久删除
ctx_empty_recycle_bin = 清空回收站
ctx_properties = 属性
delete_confirm_title = 删除“{ $name }”？
delete_confirm_title_many = 删除 { $count } 个项目？
delete_confirm_description = 此项目会移到回收站，之后可以从回收站还原。
delete_confirm_description_many = 这些项目会移到回收站，之后可以从回收站还原。
delete_confirm_action = 删除
delete_confirm_cancel = 取消
recycle_bin_delete_confirm_title = 永久删除“{ $name }”？
recycle_bin_delete_confirm_title_many = 永久删除 { $count } 个项目？
recycle_bin_delete_confirm_description = 此项目将永久删除，无法还原。
recycle_bin_delete_confirm_description_many = 这些项目将永久删除，无法还原。
recycle_bin_delete_confirm_action = 永久删除
recycle_bin_empty_confirm_title = 清空回收站？
recycle_bin_empty_confirm_description = 回收站中的所有项目都将永久删除，无法还原。
recycle_bin_empty_confirm_action = 清空回收站
recycle_bin_restore_all_confirm_title = 还原所有项目？
recycle_bin_restore_all_confirm_description = 回收站中的所有项目都将还原到其原位置。
recycle_bin_restore_all_confirm_action = 还原所有项目
delete_failed_title = 无法删除所选项目

service_not_running_title = 后台服务未运行，文件索引和内容搜索暂不可用
service_not_installed_title = 后台服务未安装，重新运行安装程序
service_incompatible_title = 后台服务版本不兼容，请在帮助菜单检查更新并重新安装
service_unauthorized_title = 当前账户无权使用后台服务，改用授权账户
# 故障说明都承诺"再试一次"，所以承诺要有一个指针和键盘都能按的按钮。
service_probe_label = 正在连接后台服务
service_retry_button = 重试连接
# 启动服务是唯一的运行期提权动作，只在用户点击这个明确按钮时发生。
service_start_button = 启动后台服务

# 设置项位于顶栏的“查看”和“工具”菜单里（SET-01）。
# 选择即时生效；菜单项的勾选状态就是当前值，不另设设置弹窗。
settings_theme = 主题
settings_theme_system = 系统
settings_theme_light = 浅色
settings_theme_dark = 深色
settings_section_language = 语言
# 语言选项写该语言自己的名字：读不懂当前界面语言的人，只能靠母语拼法认出要换到哪一种。
settings_language_system = 系统
settings_language_zh = 简体中文
settings_language_en = English
settings_section_zoom = 缩放
settings_zoom_compact = 紧凑
settings_zoom_standard = 标准
settings_zoom_relaxed = 宽松
settings_autostart = 开机自启
menu_sort = 排序
menu_startup = 启动
menu_check_updates = 检查更新
update_up_to_date_title = Cairn 已是最新版本
update_up_to_date_body = 当前最新版本为 { $version }。
update_no_releases_body = 尚无已发布的更新。
update_available_title = Cairn 有可用更新
update_available_body = 新版本 { $version }，当前版本为 { $current }。安装完成后 Cairn 会重启。
update_download_action = 更新
update_later = 稍后
update_check_failed_title = 无法检查更新
update_check_failed_body = 检查网络连接后重试。
update_incomplete_title = 更新包不完整
update_incomplete_body = 发布中缺少安装包或校验文件。稍后重试。
update_retry = 重试
update_close = 关闭
update_installing_title = 正在更新 Cairn
update_installing_body = 正在下载、校验并安装更新。完成后 Cairn 会重启。
update_verify_failed_title = 无法校验更新
update_verify_failed_body = 下载的安装包与校验值不符。重试。
update_download_failed_title = 无法下载更新
update_download_failed_body = 检查网络连接后重试。
update_cancelled_title = 更新已取消
update_cancelled_body = Cairn 未被更改。
update_install_failed_title = 无法启动更新
update_install_failed_body = 重试。
update_success_title = Cairn 已更新
update_success_body = 已安装版本 { $version }。
update_interrupted_title = 更新未完成
update_interrupted_body = 打开“帮助 > 检查更新”重试。
menu_about = 关于 Cairn
menu_about_body = 高性能 Windows 文件搜索
index_clear = 清空内容索引？
index_clear_description = 内容索引会被删除，必须重新建立后才能再次搜索内容。
index_clear_confirm_action = 清空
index_clear_cancel = 取消

sidebar_toggle = 折叠或展开侧边栏
nav_back = 后退
nav_forward = 前进
nav_up = 上一级
window_minimize = 最小化
window_maximize = 最大化
window_restore = 还原
window_close = 关闭

empty_index_empty_title = 索引为空，确认后台服务已完成扫描
recycle_bin_empty_title = 回收站为空，删除的文件会先到这里
recycle_bin_no_matches_title = 回收站中没有匹配的项目，清空搜索查看全部
recycle_bin_restore_failed = 无法还原所选项目
recycle_bin_delete_failed = 无法永久删除所选项目
recycle_bin_empty_failed = 无法清空回收站
recycle_bin_unavailable_title = 无法读取回收站，检查访问权限后重试
recycle_bin_retry = 重试
status_recycle_bin_unavailable = 回收站不可用
status_loading = 正在加载
status_refreshing = 正在刷新
status_refresh = 刷新
toast_status = 通知
toast_dismiss = 关闭通知
toast_retry = 重试
toast_accessible_name = { $title }。{ $message }
bin_error_access_denied = 拒绝访问；检查项目权限后重试
bin_error_read_only = 原位置为只读；清除只读属性后重试
bin_error_conflict = 原位置已有同名项目；重命名或移除后重试
bin_error_missing_item = 项目已不在回收站中；关闭提示后刷新列表
bin_error_original_location = 原位置不可用；重新连接驱动器后重试
bin_error_path_too_long = 原位置路径过长；缩短原位置路径后重试
bin_error_path_too_deep = 原位置层级过深；缩短原位置路径后重试
bin_error_no_items = 没有可操作的项目；关闭提示后刷新列表
bin_error_generic = Windows 未能完成操作；重试
bin_error_items = { $items }：{ $reason }
bin_error_more_items = ，还有 { $count } 个项目
bin_list_separator = 、
empty_no_matches = 没有匹配的文件或文件夹，尝试其他关键词或路径
empty_content_loading_title = 正在读取内容搜索状态
empty_content_disabled_title = 内容搜索未开启，开启后可搜索文件正文
empty_content_backfilling_title = 正在建立内容索引；完成后才能搜索内容
empty_content_error_title = 内容搜索不可用，文件名搜索仍可使用
empty_content_unreadable_title = 内容搜索状态不可用，重新读取后再试
query_too_complex_title = 内容检索词超过 { $limit } 个，减少后重新搜索
result_content_match = 内容
row_loading = 正在加载该段结果
row_not_loaded = 尚未加载的结果

tray_show = 显示 Cairn
tray_exit = 退出

view_in_table = 在表格中查看
treemap_retry = 重试
treemap_path_placeholder = C:\Users
treemap_path_label = 路径
treemap_path_clear = 清除路径
treemap_path_menu_for = 显示“{ $label }”下的文件夹
treemap_path_menu_row = 转到 “{ $label }”
treemap_path_menu_empty = 没有可切换的子文件夹
treemap_path_menu_truncated = 仅列出前 { $count } 个文件夹
treemap_crumb_label = 转到 { $label }（{ $path }）
treemap_path_invalid_title = 路径格式无法识别
treemap_path_invalid_hint = 输入驱动器绝对路径，或相对当前文件夹的路径。
treemap_unavailable_service = 无法连接后台服务
treemap_unavailable_too_large = 该目录的直接子项过多，无法一次载入；打开子文件夹或上一级
treemap_unavailable_not_directory = 该路径是文件，不是文件夹；打开文件夹或上一级
treemap_unavailable_not_indexed = 此路径不在索引中；检查路径，或稍后重试
treemap_empty_directory = 此文件夹为空
treemap_aggregate = 其余 { $count_text } 项
treemap_status_selected = { $name }，{ $path }，{ $size }（{ $percent }%），{ $date }
treemap_status_summary = { $count_text } 个项目 { $size }
treemap_zoom_reset = 重置图表缩放
treemap_zoom_reset_tooltip = 重置图表缩放（当前 { $percent }%）
treemap_filter_all = 显示全部类别
treemap_category_system = 系统
treemap_category_programs = 程序
treemap_category_documents = 文档
treemap_category_image = 图片
treemap_category_video = 视频
treemap_category_audio = 音频
treemap_category_archive = 压缩包
treemap_category_other = 其他
treemap_filter_hide = 隐藏“{ $category }”类别
treemap_filter_show = 显示“{ $category }”类别
menu_content_search = 搜索文件内容
menu_content_search_details = 内容搜索详情
menu_content_search_state = 内容搜索状态
menu_content_search_off = 内容搜索未开启
menu_content_search_loading = 正在读取内容搜索状态
menu_content_search_unavailable = 内容搜索状态不可用
menu_content_search_backfilling = 正在建立内容索引 { $processed } / { $total }
menu_content_search_ready = 内容索引已就绪
menu_content_search_failed = 内容索引失败
menu_content_search_retry = 重建内容索引
menu_content_search_clear = 清空内容索引
index_enable_action = 开启内容搜索
index_status_retry = 重试
menu_content_search_usage = 占用：{ $size }
menu_content_search_updated = 最近更新：{ $time }
menu_content_search_usage_unknown = 占用：未知
menu_content_search_never_updated = 最近更新：尚无记录
empty_content_scope_line = 没有匹配的文件或文件夹；内容搜索会跳过系统与开发工具文件夹
