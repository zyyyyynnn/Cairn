# Cairn English locale resources
#
# Written as interface language, not translated token-for-token from Chinese: destinations are
# nouns, commands are verbs, states are adjectives. Short control labels take no terminal
# punctuation; full explanatory and error sentences do. `…` marks a command that opens a dialog or
# needs more input. Shortcuts live in the keymap and in AccessKit keyshortcuts, never in the label.

# The title bar carries the brand alone; the window itself shows what the product does.
app_title = Cairn

sidebar_locations = Locations
sidebar_drives = Drives
sidebar_no_drives = No drives found yet
sidebar_recycle_bin = Recycle Bin
drive_label = Drive { $drive }

# The title-bar menu bar: six top-level menus on the same row as the brand.
menu_file = File
menu_edit = Edit
menu_view = View
menu_search = Search
menu_tools = Tools
menu_help = Help


result_list_label = Search results
table_header_label = Result table column headings
result_table_label = Result table

toggle_case_label = Case sensitive
toggle_whole_word_label = Whole word
toggle_match_path_label = Match location
toggle_regex_label = Regular expression

filter_all = All files
filter_audio = Audio
filter_doc = Document
filter_exe = Executable
filter_image = Picture
filter_video = Video
filter_zip = Compressed
filter_folder = Folder

col_name = Name
col_path = Location
col_original_location = Original location
col_size = Size
col_modified = Date modified
col_deleted = Date deleted
col_created = Date created
col_ext = Extension

query_undo = Undo
query_redo = Redo
query_cut = Cut
query_paste = Paste
query_select_all_text = Select all query text
query_clear = Clear search
sort_reverse = Reverse order

status_objects_count = { $count ->
    [one] { $count } item
   *[other] { $count } items
}

volume_capsule = { $drive }: { $state }
volume_state_synced = Up to date
volume_state_catching_up = Synchronizing { $percent }%
volume_state_catching_up_pending = Synchronizing
volume_state_rebuilding = Rebuilding index
volume_state_offline = Offline
volume_state_snapshot_mode = Snapshot mode

# Search placeholders
search_placeholder = Search files or folders…
recycle_bin_filter_placeholder = Search Recycle Bin…
recycle_bin_filter_label = Search Recycle Bin
search_box_label = Search text

ctx_open = Open
ctx_open_path = Open containing folder
ctx_copy_path = Copy full path
ctx_copy_original_location = Copy original location
ctx_copy_name = Copy file name
ctx_delete = Delete…
ctx_restore = Restore
ctx_delete_permanently = Delete permanently…
ctx_empty_recycle_bin = Empty Recycle Bin…
ctx_properties = Properties
delete_confirm_title = Delete “{ $name }”?
delete_confirm_title_many = Delete { $count } items?
delete_confirm_description = This item moves to the Recycle Bin, where you can restore it later.
delete_confirm_description_many = These items move to the Recycle Bin, where you can restore them later.
delete_confirm_action = Delete
delete_confirm_cancel = Cancel
recycle_bin_delete_confirm_title = Delete “{ $name }” permanently?
recycle_bin_delete_confirm_title_many = Delete { $count } items permanently?
recycle_bin_delete_confirm_description = This item will be permanently deleted and can’t be recovered.
recycle_bin_delete_confirm_description_many = These items will be permanently deleted and can’t be recovered.
recycle_bin_delete_confirm_action = Delete permanently
recycle_bin_empty_confirm_title = Empty Recycle Bin?
recycle_bin_empty_confirm_description = All items in the Recycle Bin will be permanently deleted and can’t be recovered.
recycle_bin_empty_confirm_action = Empty Recycle Bin
delete_failed_title = Could not delete the selected items

service_not_running_title = Indexing is unavailable
service_not_running_desc = Cairn could not reach the background service. If the component is missing, reinstall Cairn and try again.
service_not_installed_title = The local indexer is missing
service_not_installed_desc = CairnService.exe was not found next to the app. Reinstall Cairn, then try again.
service_incompatible_title = Protocol versions don’t match
service_incompatible_desc = Update the client and the service to the same version, then try again.
service_unauthorized_title = This account can’t use the service
service_unauthorized_desc = The Cairn background service refused this request. Use an authorized account, or run the action as an administrator, then try again.
# The recovery lines promise another attempt, so the promise needs a button a pointer and a keyboard
# can both reach.
service_retry_button = Retry connection
# Starting the service is the only runtime elevation, and it happens only on this explicit click.
service_start_button = Start background service
service_start_hint = File search resumes after the service starts.

# Preferences live in the title bar's View and Tools menus (SET-01).
# Choices commit as they are made; the menu checkmark is the current value, and there is no
# separate settings dialog.
settings_theme = Theme
settings_theme_system = System
settings_theme_light = Light
settings_theme_dark = Dark
settings_section_language = Language
# Language options keep the name each language gives itself: someone who can't read the current
# interface language can only recognise a language by its own spelling.
settings_language_system = System
settings_language_zh = 简体中文
settings_language_en = English
settings_section_zoom = Zoom
settings_zoom_compact = Compact
settings_zoom_standard = Standard
settings_zoom_relaxed = Relaxed
settings_autostart = Start with Windows
menu_sort = Sort
menu_startup = Startup
menu_about = About Cairn
menu_about_body = Fast Windows file search
index_clear = Clear content index?
index_clear_description = The existing full-text index will be deleted and must be rebuilt before body search works again.
index_clear_confirm_action = Clear
index_clear_cancel = Cancel

sidebar_toggle = Toggle sidebar
nav_back = Back
nav_forward = Forward
window_minimize = Minimize
window_maximize = Maximize
window_restore = Restore
window_close = Close

empty_index_empty_title = The index is empty
empty_index_empty_hint = The index has no files to show yet. Check that the background service or local index has finished scanning.
recycle_bin_empty_title = The Recycle Bin is empty
recycle_bin_empty_hint = Deleted files and folders arrive here first, where they can be restored or deleted permanently.
recycle_bin_no_matches_title = No items match in the Recycle Bin
recycle_bin_no_matches_hint = Try another search, or clear it to see every item.
recycle_bin_restore_failed = Could not restore the selected items
recycle_bin_delete_failed = Could not permanently delete the selected items
recycle_bin_empty_failed = Could not empty the Recycle Bin
recycle_bin_unavailable_title = The Recycle Bin is unavailable
recycle_bin_unavailable_hint = The Recycle Bin list is unavailable. Try again.
recycle_bin_retry = Try again
status_recycle_bin_unavailable = Recycle Bin unavailable
status_loading = Loading
status_objects_summary = { $count } items { $size }
status_refresh = Refresh
toast_status = Notification
toast_dismiss = Dismiss notification
bin_error_access_denied = Access denied
bin_error_read_only = The original location is read-only
bin_error_conflict = An item with that name already exists
bin_error_missing_item = The item is no longer in the Recycle Bin
bin_error_original_location = The original location is unavailable
bin_error_path_too_long = The original location path is too long
bin_error_path_too_deep = The original location is nested too deeply
bin_error_no_items = No item could be processed
bin_error_generic = Windows could not complete the operation
bin_error_items = { $reason }: { $items }
bin_error_more_items = and { $count } more
bin_list_separator = ", "
empty_no_matches_hint = Try a different keyword or path.
empty_no_matches = No matching files or folders
empty_content_local_title = Local index does not search file contents
empty_content_local_hint = Local fallback indexes file names only. Start the background service and enable the content index in Tools to search bodies with content:.
empty_content_loading_title = Reading the content index status
empty_content_loading_hint = Try again in a moment, or check that the content index is enabled in Tools.
empty_content_disabled_title = Content index is off
empty_content_disabled_hint = Enable the content index in Tools. content: returns body matches after the first backfill finishes.
empty_content_backfilling_title = Content index is backfilling
empty_content_backfilling_hint = content: cannot search bodies until the backfill finishes. Progress and cancel are in Tools.
empty_content_error_title = Content index is unavailable
empty_content_error_hint = The last content-index run failed: { $reason }. Retry the content index in Tools.
empty_content_unreadable_title = Content index status is unavailable
empty_content_unreadable_hint = Reload the content index status in Tools, then search again.
empty_content_scope_hint = Body search skips system folders, program folders, and development tool folders by default; file names remain searchable.
result_content_match = Body
row_loading = Loading this part of the results
row_not_loaded = Result not loaded yet

tray_show = Show Cairn
tray_exit = Exit

view_in_table = View in Table
treemap_empty_directory = This folder is empty
treemap_unreadable = Cannot read this folder
treemap_aggregate = { $count } items
treemap_status_selected = { $name } · { $path } · { $size } ({ $percent }%) · { $date }
treemap_status_summary = { $count } items { $size }
treemap_zoom_reset = Reset chart zoom
treemap_zoom_reset_tooltip = Reset chart zoom (currently { $percent }%)
treemap_filter_all = Show all categories
treemap_category_folder = Folder
treemap_category_system = System
treemap_category_applications = Applications
treemap_category_document = Document
treemap_category_archive = Archive
treemap_category_image = Image
treemap_category_code = Code
treemap_category_audio = Audio
treemap_category_video = Video
treemap_category_other = Other
treemap_filter_hide = Hide the { $category } category
treemap_filter_show = Show the { $category } category
menu_search_scope = Search scope
menu_search_scope_all_files = All files
menu_search_scope_readable_files = Readable files
menu_search_scope_cancelled = The switch was cancelled; the search scope is unchanged.
menu_search_scope_not_installed = The background service is not installed, so “All files” is unavailable.
menu_search_scope_unreadable = Cairn could not read the background service registration, so the search scope is unchanged.
menu_search_scope_change_failed = Cairn could not switch the search scope, so it is unchanged.
menu_search_scope_not_in_effect = The switch did not change the service, so the search scope is unchanged.
menu_search_scope_restricted = An administrator restricted the search scope. Contact the administrator or use an authorized account.
menu_search_scope_all_files_unavailable = All files (the background service is not installed, so it cannot be selected)
menu_content_search = Search file contents
menu_content_search_enable = Search file contents
menu_content_search_disable = Stop searching file contents
menu_content_search_details = Content search details
menu_content_search_off = Content search is off
menu_content_search_loading = Reading the content index status…
menu_content_search_unavailable = The content index status is unavailable
menu_content_search_unavailable_reason = The content index status is unavailable: { $reason }
menu_content_search_backfilling = Building the content index { $processed } / { $total }
menu_content_search_ready = The content index is ready
menu_content_search_failed = The content index failed
menu_content_search_failed_reason = The content index failed: { $reason }
menu_content_search_retry = Rebuild the content index
menu_content_search_clear = Clear the content index…
menu_content_search_state = Content search status
menu_content_search_usage = Size: { $size }
menu_content_search_updated = Last updated: { $time }
menu_content_search_usage_unknown = Size: unknown
menu_content_search_never_updated = Last updated: never
