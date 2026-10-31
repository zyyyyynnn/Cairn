app_title = Cairn
setup_subtitle = Setup
uninstall_subtitle = Uninstall
window_minimize = Minimize
window_close = Close

license_title = License
license_name = Apache License 2.0
action_view_license = View full license
error_license_required = Accept the license terms to continue.
error_open_license = The license could not be opened. Try again.

license_accept = I accept these terms

location_title = Install location
install_path_label = Install folder
install_scope = Installs for all users under Program Files
install_space = About 100 MB required
action_browse = Browse…

progress_title = Installing Cairn
progress_removing_old = Removing the previous version
progress_preparing = Preparing the installation
progress_copying = Copying files
progress_service = Registering the background service
progress_finishing = Finishing setup
progress_do_not_close = Keep this window open until setup finishes.
progress_do_not_close_uninstall = Keep this window open until the uninstall finishes.
progress_percent = { $percent }%

finish_title = Cairn is installed
finish_body = The service indexes files; the first search can take a few minutes.
finish_restart = Restart your PC to finish some changes.

uninstall_missing_title = Cairn was not found
uninstall_missing_body = Cairn is not installed on this PC, so there is nothing to remove.

error_uninstall_title = Uninstall did not finish
error_uninstall_msi_unknown = The uninstall did not finish, so Cairn is still installed. Check the log for the reason, then try again.
error_uninstall_msi_cancelled = The uninstall was cancelled, so Cairn is still installed.
error_uninstall_msi_busy = Another installer or uninstaller is already running. Please wait for it to complete and try again.
error_uninstall_not_installed = No Cairn installation was found.
error_uninstall_msi_platform = This installation record does not support this version of Windows.
error_uninstall_msi_fatal = Windows Installer could not complete the uninstall. Check the log, then try again.
error_uninstall_msi_invalid = Windows Installer could not read the data it saved for Cairn. Removing the remaining files also removes Cairn’s settings and registry entries. This cannot be undone.

error_title = Setup did not finish
error_msi_unknown = The install did not finish, so existing files were not removed. Check the log for the reason, then try again.
error_no_payload = Installer files are missing. Download CairnSetup.exe again.
error_msi_cancelled = Setup was cancelled.
error_msi_busy = Another installer is already running. Please wait for it to complete and try again.
error_msi_invalid = The installer is damaged. Download it again.
error_msi_platform = This package does not support this version of Windows.
error_msi_fatal = Windows Installer could not complete the operation. Check the log, then try again.
error_legacy_failed = Remove the old version in Apps & features, then try again.
error_not_installed = No Cairn installation was found.
error_launch = Cairn could not be started. Open it from the Start menu.
error_open_log = The log file could not be opened. Try again.

maintenance_installed = Version { $version }
maintenance_newer = A newer version, { $version }, is already installed.

uninstall_title = Uninstall Cairn
uninstall_body = Removes Cairn, its CLI, service, index, settings, and registry entries for all users.
uninstall_progress_title = Uninstalling Cairn
uninstall_progress_working = Removing Cairn files and settings…
uninstall_restart = Restart Windows to finish removing Cairn.
uninstall_done_title = Cairn is uninstalled
uninstall_files_removed_title = Cairn files were removed
uninstall_record_remaining = Windows Installer still holds an installation record for Cairn. The files are gone, but a later install may see Cairn as already installed.
uninstall_cleanup_incomplete = Some Cairn data could not be removed. Check the log for details.

path_empty = Enter an install folder.
path_not_absolute = Enter a full local path, for example C:\Program Files\Cairn.
path_network = Cairn cannot be installed on a network location.
path_root = Choose a folder, not the root of a drive.
path_outside_program_files = Install Cairn under Program Files.
path_system = Cairn cannot be installed directly in Windows or the root of Program Files.
path_file = That path is a file. Choose a folder.
path_invalid = The path contains characters Windows does not allow.

action_continue = Continue
action_back = Back
action_cancel = Cancel
action_install = Install
action_update = Update
action_close = Close
action_retry = Try again
action_uninstall = Uninstall
action_remove_remaining = Remove remaining files
action_open_app = Open Cairn
action_finish = Done
action_view_log = View log
