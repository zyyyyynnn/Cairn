app_title = Cairn
setup_subtitle = Setup
uninstall_subtitle = Uninstall
window_minimize = Minimize
window_close = Close

license_title = License
license_name = Apache License 2.0
action_view_license = View full license
error_license_required = Accept the license terms to continue.

license_accept = I accept these terms

location_title = Install location
install_summary = Installs for all users under Program Files, using 100 MB.
action_browse = Browse…

progress_title = Installing Cairn
progress_removing_old = Removing the previous version
progress_preparing = Preparing the installation
progress_copying = Copying files
progress_service = Registering the background service
progress_finishing = Finishing the installation
progress_do_not_close = Keep this window open until the installation finishes.
progress_do_not_close_uninstall = Keep this window open until the uninstall finishes.
progress_percent = { $percent }%

finish_title = Cairn is installed
finish_body = The service indexes files; the first search can take a few minutes.
finish_restart = Restart your PC to finish the installation.

uninstall_missing_title = Cairn was not found
uninstall_missing_body = Cairn is not installed on this PC, so there is nothing to remove.

error_uninstall_title = Uninstall did not finish
error_uninstall_msi_unknown = Cairn is still installed. Check the log for the reason, then try again.
error_uninstall_msi_cancelled = The uninstall was cancelled, so Cairn is still installed.
error_uninstall_msi_busy = Another installer or uninstaller is already running. Wait for it to complete and try again.
error_uninstall_not_installed = No Cairn installation was found.
error_uninstall_msi_platform = This installation record does not support this version of Windows. Use a Cairn installer that supports this version to remove Cairn.
error_uninstall_msi_fatal = Windows Installer could not complete the uninstall. Check the log, then try again.
error_uninstall_msi_invalid = Windows Installer could not read Cairn’s installation data. Removing the remaining files also removes Cairn’s settings and registry entries. This cannot be undone.
error_uninstall_cleanup_failed = Some Cairn files could not be removed. Check the log for the reason.

error_title = The installation did not finish
error_msi_unknown = Existing files were not removed. Check the log for the reason, then try again.
error_no_payload = Installer files are missing. Download CairnSetup.exe again.
error_staging = The installer could not prepare its temporary files. Close the installer and try again.
error_uninstall_staging = The uninstaller could not prepare its temporary files. Close this window and try again.
error_msi_cancelled = Setup was cancelled.
error_msi_busy = Another installer is already running. Wait for it to complete and try again.
error_msi_invalid = The installer is damaged. Download it again.
error_msi_cleanup_failed = The cleanup did not finish. Check the log for the reason.
error_msi_platform = This package does not support this version of Windows. Download the installer for this version.
error_msi_fatal = Windows Installer could not complete the operation. Check the log, then try again.
error_legacy_failed = Remove the old version in Settings > Apps, then try again.
error_not_installed = No Cairn installation was found.
error_launch = Cairn could not be started. Open it from the Start menu.
error_open_log = The log file could not be opened. Try again.

maintenance_installed = Version { $version }
maintenance_newer = A newer version, { $version }, is already installed.

uninstall_title = Uninstall Cairn
uninstall_body = Removes Cairn, its command-line tools, service, index, settings, and registry entries for all users.
uninstall_progress_title = Uninstalling Cairn
uninstall_progress_working = Removing Cairn files and settings
uninstall_restart = Restart your PC to finish removing Cairn.
uninstall_done_title = Cairn is uninstalled
uninstall_done_body = Cairn has been removed from this PC.
uninstall_files_removed_title = Cairn files were removed
uninstall_record_remaining = Windows Installer still holds an installation record for Cairn. The files are gone, but a later install may see Cairn as already installed. Run Cairn Setup again, then uninstall Cairn to clear the record.
uninstall_cleanup_incomplete = Some Cairn data could not be removed. Check the log for details.

path_empty = Enter an install folder.
path_not_absolute = Enter a full local path, for example C:\Program Files\Cairn.
path_network = Cairn cannot be installed on a network location. Choose a local folder.
path_root = Choose a folder, not the root of a drive.
path_outside_program_files = Install Cairn under Program Files.
path_system = Cairn cannot be installed directly in Windows or the root of Program Files. Choose a subfolder.
path_file = That path is a file. Choose a folder.
path_invalid = The path contains characters Windows does not allow. Remove characters such as < > : " / \ | ? * from the folder name.

action_continue = Continue
action_back = Back
action_install = Install
action_close = Close
action_cancel = Cancel
action_retry = Retry
action_uninstall = Uninstall
action_remove_remaining = Remove remaining files
action_open_app = Open Cairn
action_finish = Done
action_view_log = View log
log_viewer_title = Installation log
