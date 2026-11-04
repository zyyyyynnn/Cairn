# Cairn English locale resources
#
# Written as interface language, not translated token-for-token from Chinese: destinations are
# nouns, commands are verbs, states are adjectives. Short control labels and one-line state titles
# take no terminal punctuation; explanatory sentences and installer body text do. `…` marks a
# command that opens a dialog or needs more input, and an input placeholder. Shortcuts live in the
# keymap and in AccessKit keyshortcuts, never in the label.

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
toggle_match_path_label = Match path
toggle_regex_label = Regular expression

filter_all = All files
filter_audio = Audio
filter_doc = Document
filter_exe = Executable
filter_image = Image
filter_video = Video
filter_zip = Archive
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
edit_select_all = Select all
query_clear = Clear search
sort_reverse = Reverse order

status_selected_count = { $count_text } items selected
selection_limit_title = Selection limit reached
selection_limit_message = The first { $count_text } items are selected. Narrow the results and try again.
reveal_limit_title = Folders not opened
reveal_limit_message = The selection spans { $count } folders; at most { $limit } can be opened at once. Narrow the selection and try again.
status_objects_count = { $count ->
    [one] { $count_text } item
   *[other] { $count_text } items
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
ctx_delete = Delete
ctx_restore = Restore
ctx_restore_all = Restore all items
ctx_delete_permanently = Delete permanently
ctx_empty_recycle_bin = Empty Recycle Bin
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
recycle_bin_restore_all_confirm_title = Restore all items?
recycle_bin_restore_all_confirm_description = All items in the Recycle Bin will be restored to their original locations.
recycle_bin_restore_all_confirm_action = Restore all items
delete_failed_title = Could not delete the selection

service_not_running_title = The background service is not running; search is unavailable
service_not_installed_title = The background service is not installed; run the Cairn installer again
service_incompatible_title = The background service version is incompatible; reinstall Cairn
service_unauthorized_title = This account cannot use the background service; use an authorized account
# The recovery lines promise another attempt, so the promise needs a button a pointer and a keyboard
# can both reach.
service_probe_label = Connecting to the background service
service_retry_button = Retry connection
# Starting the service is the only runtime elevation, and it happens only on this explicit click.
service_start_button = Start background service

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
menu_check_updates = Check for updates
update_up_to_date_title = Cairn is up to date
update_up_to_date_body = Version { $version } is the latest available.
update_no_releases_body = No published updates are available yet.
update_available_title = A Cairn update is available
update_available_body = Version { $version } is available. You have { $current }. Cairn restarts when the installation finishes.
update_download_action = Update
update_later = Later
update_check_failed_title = Could not check for updates
update_check_failed_body = Check your connection, then try again.
update_incomplete_title = The update package is incomplete
update_incomplete_body = The release does not include the installer and checksum. Try again later.
update_retry = Retry
update_close = Close
update_installing_title = Updating Cairn
update_installing_body = Downloading, verifying, and installing the update. Cairn restarts when it finishes.
update_verify_failed_title = Could not verify the update
update_verify_failed_body = The downloaded package did not match its checksum. Try again.
update_download_failed_title = Could not download the update
update_download_failed_body = Check your connection, then try again.
update_cancelled_title = Update cancelled
update_cancelled_body = Cairn was not changed.
update_install_failed_title = Could not start the update
update_install_failed_body = Try again.
update_success_title = Cairn updated
update_success_body = Version { $version } is installed.
update_interrupted_title = The update did not finish
update_interrupted_body = Open Help > Check for updates to try again.
menu_about = About Cairn
menu_about_body = High-performance Windows file search
index_clear = Clear content index?
index_clear_description = The content index will be deleted and must be rebuilt before content search works again.
index_clear_confirm_action = Clear
index_clear_cancel = Cancel

sidebar_toggle = Toggle sidebar
nav_back = Back
nav_forward = Forward
nav_up = Up one level
window_minimize = Minimize
window_maximize = Maximize
window_restore = Restore
window_close = Close

empty_index_empty_title = The index is empty; check that the background service has finished scanning
recycle_bin_empty_title = The Recycle Bin is empty; deleted files arrive here first
recycle_bin_no_matches_title = No items match; clear the search to see the whole Recycle Bin
recycle_bin_restore_failed = Could not restore the selection
recycle_bin_delete_failed = Could not permanently delete the selection
recycle_bin_empty_failed = Could not empty the Recycle Bin
recycle_bin_unavailable_title = Could not read the Recycle Bin; check access and try again
recycle_bin_retry = Retry
status_recycle_bin_unavailable = Recycle Bin unavailable
status_loading = Loading
status_objects_summary = { $count ->
    [one] { $count_text } item { $size }
   *[other] { $count_text } items { $size }
}
status_refreshing = Refreshing
status_refresh = Refresh
toast_status = Notification
toast_dismiss = Dismiss notification
toast_retry = Retry
toast_accessible_name = { $title }. { $message }
bin_error_access_denied = Access denied; check the item's permissions, then retry
bin_error_read_only = The original location is read-only; clear the read-only attribute, then retry
bin_error_conflict = An item with that name already exists; rename or remove it, then retry
bin_error_missing_item = The item is no longer in the Recycle Bin; close this message and refresh the list
bin_error_original_location = The original location is unavailable; reconnect the drive, then retry
bin_error_path_too_long = The original location path is too long; shorten the path, then retry
bin_error_path_too_deep = The original location is nested too deeply; shorten the path, then retry
bin_error_no_items = No item could be processed; close this message and refresh the list
bin_error_generic = Windows could not complete the operation; try again
bin_error_items = { $items }: { $reason }
bin_error_more_items = , and { $count } more
bin_list_separator = ", "
empty_no_matches = No files or folders match; try another keyword or path
empty_content_loading_title = Reading the content search status
empty_content_disabled_title = Content search is off; turn it on to search file contents
empty_content_backfilling_title = Building the content index; content search starts when it finishes
empty_content_error_title = Content search is unavailable; filename search still works
empty_content_unreadable_title = Content search status is unavailable; read it again to retry
result_content_match = Content
row_loading = Loading this part of the results
row_not_loaded = Result not loaded yet

tray_show = Show Cairn
tray_exit = Exit

view_in_table = View in table
treemap_retry = Retry
treemap_path_placeholder = C:\Users
treemap_path_label = Path
treemap_path_clear = Clear path
treemap_path_menu_for = Show folders under { $label }
treemap_path_menu_empty = No subfolders to switch to
treemap_path_menu_truncated = Only the first { $count } folders are listed
treemap_crumb_label = Go to { $label } ({ $path })
treemap_path_invalid_title = Path format not recognized
treemap_path_invalid_hint = Enter a drive path such as C:\Users, or a path relative to this folder.
treemap_unavailable_service = Could not reach the background service
treemap_unavailable_too_large = This folder is too large for the Treemap; open a subfolder or go up one level
treemap_unavailable_not_directory = That path is a file; open a folder or go up one level
treemap_unavailable_not_indexed = This path is not in the index; check the path or retry later
treemap_empty_directory = This folder is empty
treemap_aggregate = { $count ->
    [one] { $count_text } item
   *[other] { $count_text } items
}
treemap_status_selected = { $name }, { $path }, { $size } ({ $percent }%), { $date }
treemap_status_summary = { $count ->
    [one] { $count_text } item { $size }
   *[other] { $count_text } items { $size }
}
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
menu_content_search = Search file contents
menu_content_search_details = Content search details
menu_content_search_state = Content search status
menu_content_search_off = Content search is off
menu_content_search_loading = Reading the content search status
menu_content_search_unavailable = Content search status is unavailable
menu_content_search_backfilling = Building the content index { $processed } / { $total }
menu_content_search_ready = The content index is ready
menu_content_search_failed = The content index failed
menu_content_search_retry = Rebuild the content index
menu_content_search_clear = Clear the content index
index_enable_action = Turn on content search
index_status_retry = Retry
menu_content_search_usage = Size: { $size }
menu_content_search_updated = Last updated: { $time }
menu_content_search_usage_unknown = Size: unknown
menu_content_search_never_updated = Last updated: never
empty_content_scope_line = No files or folders match; content search skips system and developer-tool folders
