# Cairn 简体中文界面资源
#
# 文案按界面语言规则书写，不做逐词翻译：目的地用名词、命令用动词、状态用形容词。
# 短控件标签不加句末标点；完整说明与错误句子加句末标点。`…` 只用于"会打开对话框或需要
# 更多输入"的命令。快捷键写在键表与 AccessKit 的 keyshortcuts 里，不写进标签文字。

# 顶栏只写品牌名：产品是什么由窗口本身说明，"本地文件即时搜索"是重复的注解。
app_title = Cairn

# 侧边导航：目的地是名词，不带"管理/面板"这类外壳词。
sidebar_locations = 位置
sidebar_drives = 驱动器
sidebar_no_drives = 尚未发现驱动器
sidebar_recycle_bin = 回收站
drive_label = 驱动器 { $drive }

# 顶栏菜单：与品牌同行的六个顶层菜单，对齐 Everything 1.5 的菜单栏。
menu_file = 文件
menu_edit = 编辑
menu_view = 查看
menu_search = 搜索
menu_tools = 工具
menu_help = 帮助

breadcrumb_label = 当前位置

result_list_label = 搜索结果列表
table_header_label = 结果表列标题
result_table_label = 结果表

toggle_case_label = 区分大小写
toggle_whole_word_label = 全字匹配
toggle_match_path_label = 匹配位置
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
query_select_all_text = 全选搜索文本
query_clear = 清空搜索
sort_reverse = 反向排序

status_objects_count = { $count } 个项目

volume_capsule = { $drive }: { $state }
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
ctx_delete = 删除…
ctx_restore = 还原
ctx_delete_permanently = 永久删除…
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
recycle_bin_empty_confirm_description = 确定要永久删除回收站中的所有项目吗？此操作无法撤销。
recycle_bin_empty_confirm_action = 清空回收站
delete_failed_title = 无法删除所选项目

service_not_running_title = 索引暂不可用
service_not_running_desc = Cairn 未能连接后台服务。若服务组件缺失，请重新安装 Cairn 后重试。
service_not_installed_title = 本地索引组件缺失
service_not_installed_desc = 应用所在目录中找不到 cairnd.exe。请重新安装 Cairn，然后重试。
service_incompatible_title = 协议版本不兼容
service_incompatible_desc = 请升级客户端与服务端组件到同一版本后重试。
service_unauthorized_title = 当前账户没有访问该服务的权限
service_unauthorized_desc = Cairn 后台服务拒绝了此请求。请改用已授权的账户，或以管理员身份重新发起操作。
# 故障说明都承诺"再试一次"，所以承诺要有一个指针和键盘都能按的按钮。
service_retry_button = 重试连接
# 启动服务是唯一的运行期提权动作，只在用户点击这个明确按钮时发生。
service_start_button = 启动后台服务
service_start_hint = 启动后即可恢复文件搜索。

# 内容索引配置通过管道直接提交给服务，主窗口不申请提权。
config_submit_in_progress = 正在应用内容索引设置

# 设置项现在住在顶栏的“查看”和“工具”菜单里（SET-01）。
# 选择即时生效；菜单项的勾选状态就是当前值，不再有独立的设置弹窗。
settings_theme = 主题
settings_theme_system = 系统
settings_theme_light = 浅色
settings_theme_dark = 深色
settings_section_language = 语言
# 语言选项写该语言自己的名字：读不懂当前界面语言的人，只能靠母语拼法认出要换到哪一种。
settings_language_system = 系统
settings_language_zh = 简中
settings_language_en = EN
settings_section_zoom = 缩放
settings_zoom_compact = 紧凑
settings_zoom_standard = 标准
settings_zoom_relaxed = 宽松
settings_autostart = 开机自启
settings_section_index_source = 索引来源
settings_content_index = 内容索引
menu_sort = 排序
menu_startup = 启动
menu_index = 索引
menu_about = 关于 Cairn
menu_about_body = 高性能 Windows 文件搜索
menu_index_source_service = 索引来源：后台服务运行中
menu_index_source_local = 索引来源：本地索引（后台服务未运行）
menu_index_source_local_only = 索引来源：本地索引（未安装后台服务）
menu_index_source_unavailable = 索引来源：后台服务不可用
menu_content_index_service_unavailable = 内容索引（后台服务不可用）
menu_content_index_loading = 内容索引（正在读取配置…）
menu_content_index_unavailable = 内容索引（无法读取状态）
menu_content_index_unavailable_reason = 内容索引（无法读取状态：{ $reason }）
menu_content_index_disabled = 内容索引（已关闭）
menu_content_index_backfilling = 内容索引（回填中 { $processed } / { $total }）
menu_content_index_ready = 内容索引（已就绪）
menu_content_index_error = 内容索引（失败）
menu_content_index_error_reason = 内容索引（失败：{ $reason }）
menu_content_index_enable = 启用内容索引
menu_content_index_disable = 关闭内容索引
menu_content_index_action_failed = 内容索引设置未生效
menu_content_index_action_failed_reason = 内容索引设置未生效：{ $reason }
menu_content_index_cancel = 取消回填
menu_content_index_retry = 重试内容索引
menu_content_index_reload = 重新读取状态
menu_content_index_clear = 清空内容索引…
index_clear_description = 已建立的正文索引会被删除，需要重新回填才能再次检索。
index_clear_confirm_action = 清空
index_clear_cancel = 取消
menu_content_index_local = 内容索引（本地索引不支持）

sidebar_toggle = 折叠或展开侧边栏
nav_back = 后退
nav_forward = 前进
window_minimize = 最小化
window_maximize = 最大化
window_restore = 还原
window_close = 关闭

empty_index_empty_title = 索引为空
empty_index_empty_hint = 当前索引没有可显示的文件。请确认后台服务或本地索引已完成扫描。
recycle_bin_empty_title = 回收站为空
recycle_bin_empty_hint = 删除的文件和文件夹会先移到这里，可在此还原或永久删除。
recycle_bin_no_matches_title = 回收站中没有匹配的项目
recycle_bin_no_matches_hint = 请尝试其他关键词，或清空搜索查看全部项目。
recycle_bin_restore_failed = 无法还原所选项目
recycle_bin_delete_failed = 无法永久删除所选项目
recycle_bin_empty_failed = 无法清空回收站
recycle_bin_unavailable_title = 无法读取回收站
recycle_bin_unavailable_hint = 回收站列表暂时不可用，请重试。
recycle_bin_retry = 重试
status_recycle_bin_unavailable = 回收站不可用
status_loading = 正在加载
toast_status = 通知
toast_dismiss = 关闭通知
bin_error_access_denied = 拒绝访问
bin_error_read_only = 原位置为只读
bin_error_conflict = 原位置已有同名项目
bin_error_missing_item = 项目已不在回收站中
bin_error_original_location = 原位置不可用
bin_error_path_too_long = 原位置路径过长
bin_error_path_too_deep = 原位置层级过深
bin_error_no_items = 没有可操作的项目
bin_error_generic = Windows 未能完成操作
bin_error_items = { $reason }：{ $items }
bin_error_more_items = 还有 { $count } 个项目
bin_list_separator = 、
empty_no_matches_hint = 请尝试其他关键词或路径。
empty_no_matches = 没有匹配的文件或文件夹
empty_content_local_title = 本地索引不搜索文件内容
empty_content_local_hint = 本地降级只索引文件名。启动后台服务并在工具菜单开启内容索引后，才能用 content: 搜索正文。
empty_content_loading_title = 正在读取内容索引状态
empty_content_loading_hint = 稍候重试，或在工具菜单中确认内容索引已开启。
empty_content_disabled_title = 内容索引未启用
empty_content_disabled_hint = 在工具菜单开启内容索引；首次回填完成后，content: 才能返回正文结果。
empty_content_backfilling_title = 内容索引正在回填
empty_content_backfilling_hint = 回填完成前，content: 不能搜索正文。进度和取消操作在工具菜单中。
empty_content_error_title = 内容索引不可用
empty_content_error_hint = 上次内容索引任务失败：{ $reason }。请在工具菜单重试内容索引。
empty_content_unreadable_title = 无法读取内容索引状态
empty_content_unreadable_hint = 在工具菜单重新读取内容索引状态后再搜索。
empty_content_scope_hint = 正文搜索默认跳过系统目录、程序安装目录和开发工具目录；文件名仍可搜索。
result_content_match = 正文
row_loading = 正在加载该段结果
row_not_loaded = 尚未加载的结果

tray_show = 显示 Cairn
tray_exit = 退出

view_in_table = 在表格中查看
treemap_empty_directory = 此文件夹为空
treemap_aggregate = { $count } 项
treemap_status_selected = { $name } · { $path } · { $size } ({ $percent }%) · { $date }
treemap_status_summary = { $count } 个项目 { $size }
treemap_zoom_reset = 重置图表缩放
treemap_zoom_reset_tooltip = 重置图表缩放（当前 { $percent }%）
treemap_filter_all = 显示全部类别
treemap_category_folder = 文件夹
treemap_category_system = 系统
treemap_category_applications = 应用
treemap_category_document = 文档
treemap_category_archive = 压缩包
treemap_category_image = 图片
treemap_category_code = 代码
treemap_category_audio = 音频
treemap_category_video = 视频
treemap_category_other = 其他
treemap_filter_hide = 隐藏“{ $category }”类别
treemap_filter_show = 显示“{ $category }”类别
