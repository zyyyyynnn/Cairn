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
sidebar_recycle_bin = Recycle Bin
sidebar_folder_sources = Folder sources
sidebar_manage_folder_sources = Manage folder sources…
source_state_synced = synced
source_state_catching_up = catching up
source_state_rebuilding = rebuilding
source_state_offline = offline
source_state_snapshot = snapshot
folder_source_row_label = { $path }, { $state }
drive_label = Drive { $drive }
volume_state_catching_up = syncing
volume_state_rebuilding = rebuilding
volume_state_snapshot_mode = snapshot mode
volume_state_offline = offline
sidebar_index_state = Index status: { $volume } { $state }
sidebar_index_state_more =
    { $count ->
        [one] Index status: { $volume } { $state }, plus one more volume
       *[other] Index status: { $volume } { $state }, plus { $count } more volumes
    }

# The title-bar menu bar: seven top-level menus on the same row as the brand.
menu_file = File
menu_edit = Edit
menu_view = View
menu_search = Search
menu_index = Index
menu_tools = Tools
menu_help = Help

result_list_label = Search results
table_header_label = Result table column headings
result_table_label = Result table

toggle_case_label = Case sensitive
toggle_whole_word_label = Whole word
toggle_match_path_label = Match path
toggle_regex_label = Regular expression
toggle_exclude_hidden_label = Exclude hidden files
toggle_exclude_system_label = Exclude system files

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
col_length = Length
col_author = Author
col_tags = Tags
col_dimensions = Dimensions
col_framerate = Framerate
menu_columns = Show columns
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

# Search placeholders
search_placeholder = Search files or folders…
recycle_bin_filter_placeholder = Search Recycle Bin…
recycle_bin_filter_label = Search Recycle Bin
search_box_label = Search text

ctx_open = Open
ctx_rename = Rename
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
rename_label = Rename file
rename_failed_title = Rename failed
rename_error_empty = The name cannot be empty.
rename_error_unchanged = The name is unchanged.
rename_error_invalid_char = The name cannot contain “{ $char }”.
rename_error_control_char = The name contains a control character.
rename_error_trailing_dot_or_space = The name cannot end with a dot or a space.
rename_error_reserved = That name is reserved by Windows.
rename_error_too_long = The name is too long for Windows.
rename_error_target_exists = An item with that name already exists in this folder; choose another name.
rename_error_access_denied = Windows denied the rename; check the item's permissions, then try again
rename_error_read_only = The item is read-only; clear the read-only attribute, then try again
rename_error_path_too_long = The path is too long for Windows; shorten it, then try again
rename_error_shell = Windows could not complete the rename; try again
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
service_timeout_title = The background service is not responding; try again
service_not_installed_title = The background service is not installed; run the Cairn installer again
service_incompatible_title = The background service is an incompatible build; check for updates in the Help menu and reinstall.
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
menu_check_updates = Check for updates
update_checking_title = Checking for updates
update_up_to_date_title = Cairn is up to date
update_up_to_date_body = Version { $version } is the latest available.
update_no_releases_body = No published updates are available yet.
update_available_title = A Cairn update is available
update_available_body = Version { $version } is available. You have { $current }. Cairn restarts when the installation finishes.
update_download_action = Update
update_check_failed_title = Could not check for updates
update_check_failed_body = Check your connection, then try again.
update_incomplete_title = The update package is incomplete
update_incomplete_body = The release does not include the installer and checksum. Try again later.
update_retry = Retry
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
index_clear = Clear content index?
index_clear_description = The content index will be deleted and must be rebuilt before content search works again.
index_clear_confirm_action = Clear
index_clear_cancel = Cancel

sidebar_toggle = Toggle sidebar
nav_back = Back
nav_forward = Forward
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
search_cancelled_title = Search cancelled
hotkey_unavailable_title = Global hotkey unavailable
hotkey_unavailable_message = Ctrl+Alt+Space is taken by another application; open the window from the tray
empty_content_loading_title = Reading the content search status
empty_content_disabled_title = Content search is off; turn it on to search file contents
empty_content_backfilling_title = Building the content index; content search starts when it finishes
empty_content_error_title = Content search is unavailable; filename search still works
empty_content_unreadable_title = Content search status is unavailable; read it again to retry
query_too_complex_title = More than { $limit } content terms; remove some and search again
query_degraded_property_index = Property index is off; queries with property filters return no results
result_content_match = Content
row_loading = Loading this part of the results
row_not_loaded = Result not loaded yet

tray_show = Show Cairn
tray_exit = Exit

menu_index_journal_open = Index journal…
menu_folder_sources = Folder sources…
menu_rescan = Rescan
menu_rescan_all = Rescan all locations
menu_content_index = Content index
menu_content_search = Search file contents
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
menu_property_index_menu = Extended properties
menu_property_index = Index file properties
menu_property_index_status = Property index status
menu_property_index_off = Property index is off
menu_property_index_loading = Reading the property index status
menu_property_index_unavailable = Property index status is unavailable
menu_property_index_backfilling = Indexing file properties { $processed } / { $total }
menu_property_index_ready = Property index is ready ({ $count } items)
menu_property_index_failed = Property index failed
menu_property_index_usage = Size: { $size }
menu_property_index_usage_unknown = Size: unknown
journal_dialog_title = Index Journal
journal_dialog_record_toggle = Record index changes
journal_dialog_status_unavailable = Background service is unavailable
journal_dialog_status_error = Could not read the index journal
journal_dialog_error_body = Could not read the index journal. Use Refresh to try again.
journal_dialog_status_loading = Reading the index journal
journal_dialog_status_off = Recording is off
journal_dialog_status_summary = { $count } entries ({ $size })
journal_dialog_refresh = Refresh
journal_dialog_close = Close
journal_dialog_col_time = Time
journal_dialog_col_kind = Type
journal_dialog_col_path = Path
journal_dialog_renamed_from = From: { $path }
journal_dialog_empty_unavailable = Connect to the background service to view the index journal.
journal_dialog_empty_off = Turn on "Record index changes" to log creations, modifications, renames, deletions, and rescans.
journal_dialog_empty = No index changes recorded yet.
journal_kind_created = Created
journal_kind_modified = Modified
journal_kind_renamed = Renamed
journal_kind_deleted = Deleted
journal_kind_attribute_changed = Attribute
journal_kind_overflow_rescan = Rescan
toast_rescan_started_title = Index rescan started
toast_rescan_started_body = The background service is rescanning all indexed sources.
toast_rescan_source_started_body = The background service is rescanning { $source }.
toast_rescan_failed_title = Could not start index rescan
toast_rescan_failed_body = Could not start the index rescan. Try again shortly.
folder_sources_title = Folder sources
folder_sources_path_placeholder = Enter a folder path
folder_sources_add = Add
folder_sources_remove = Remove
folder_sources_rescan = Rescan
folder_sources_empty = No folder sources yet.
folder_sources_close = Close
toast_folder_source_add_failed_title = Could not add folder source
toast_folder_source_add_failed_body = Check that the path exists and the service can read it, and that the folder-source limit is not reached.
toast_folder_source_remove_failed_title = Could not remove folder source
toast_folder_source_remove_failed_body = Try again shortly.
folder_sources_unavailable = The background service is not connected, so folder sources cannot be managed now.
toast_property_index_failed_title = Could not update property index
toast_property_index_action_failed_body = Could not update the property index. Try again shortly.
toast_index_journal_failed_title = Could not update index journal
toast_index_journal_action_failed_body = Could not update the index journal setting. Try again shortly.
