# Ferrous Commander

A fast, light file manager in Rust (egui): two browsers side by side, tabs, folder columns
(press <kbd>`</kbd>), preview pane, favourites, drives, history, search, colour tags, themes.

Folder reads and refreshes run on a bounded background worker. Sidebar metadata and
Open with application discovery also run outside the render thread, so sleeping network
shares do not stall window movement or resizing. Background refresh checks only visible folders. Inactive folder listings expire after
30 seconds and share a cache budget of 24 folders / 50,000 entries; visible folders
remain available even above that budget. Preview reads run one at a time and skip
superseded selections. Closing or replacing a search cancels it, and timed-out
Windows drive probes cannot accumulate on subsequent refreshes.

The expanded file tree reuses its flattened rows and filtered view until folder data,
expanded branches, or visibility settings change. Drawing temporarily transfers the
row buffer instead of cloning the entire tree; ordinary repaints only paint visible rows.

## Download

Ready-made builds are on the [Releases](../../releases) page: the Windows setup or
portable zip, and a Linux archive. `SHA256SUMS.txt` there lists their checksums.
Windows may warn about an unknown publisher, because the builds are not signed.

## Install

**Windows** – run `ferrous-commander.exe --install` (or Settings → *Install for this user*).
It copies itself to `%LOCALAPPDATA%\Programs\Ferrous Commander`, adds a Start-menu entry and an
entry in *Installed apps* (uninstall from there). No admin rights needed.
Uninstalling from Settings inside the installed copy closes the app so its folder can be removed.
For a classic setup `.exe`: install Inno Setup, then `powershell -File packaging\build-windows.ps1`.
The Windows packaging script uses the versions in `Cargo.lock` and Cargo's actual
output directory (including `CARGO_TARGET_DIR`) for both the portable ZIP and setup.
The icon is embedded in the executable, so the portable build is a single file.
Windows builds use the GUI subsystem in both debug and release mode, so launching
or installing either build does not create a terminal window. Packaging rejects
executables with a console subsystem before creating the ZIP or setup.

**Linux** – from the `app` folder, run `sh packaging/build-linux.sh`. It builds
`dist/ferrous-commander-linux-<architecture>.tar.gz` (for example `x86_64` or `aarch64`)
and a `.deb` when `cargo-deb` is installed. Extract the archive and run
`./ferrous-commander` to open the application, or `./install.sh` to add it to your
application menu for the current user. No administrator access is needed to install.
You can also use Settings → **Install for this user**, or run
`./ferrous-commander --install` directly. Uninstall with
`~/.local/bin/ferrous-commander --uninstall`; your settings are kept.
Installation adds the executable to `~/.local/bin` and the menu launcher/icon to
`$XDG_DATA_HOME` (default `~/.local/share`). Reinstalling updates the executable
even while an existing instance is running; restart it to use the new version.
The build uses the dependency versions in `Cargo.lock` and honours `CARGO_TARGET_DIR`.

When continuing development on Linux after using another device, rebuild from
source rather than launching the Windows `.exe` or an older bundled binary.
Install the Rust toolchain and Linux build dependencies first (Debian/Ubuntu:
`build-essential libxkbcommon-dev libgl1-mesa-dev libwayland-dev`), then run:

```sh
cd app
sh packaging/build-linux.sh
./dist/ferrous-commander/ferrous-commander
```

The Linux packaging script rejects other operating systems. Its bundled installer
can be run from any working directory and returns installation failures to the caller.

To verify the Linux installer after building, run
`python3 packaging/check-linux-install.py`. This uses disposable HOME/XDG folders,
checks paths with spaces, Unicode and shell/desktop special characters, validates
and launches the menu entry through GIO, updates a running executable, and checks
uninstallation preserves settings. Requires `gio` and `desktop-file-validate`;
no graphical session or administrator access is needed for this installer test.

## Configuration

Preferences and the panel session are stored in a readable Lua data table in
`~/.config/ferrous/settings.lua` on Linux (or `$XDG_CONFIG_HOME/ferrous/settings.lua`)
and `%APPDATA%\ferrous\settings.lua` on Windows. Settings → **Open configuration folder**
opens that directory and shows the exact path. Close the app before editing;
edits take effect at the next start. Existing `ferrous/settings.json` or
`ferrous-commander/settings.json` settings are migrated automatically when the Lua
file does not exist; the old files are kept. The current JSON location takes priority.

```lua
-- Omitted fields use their defaults.
return {
  theme = "Dark",
  font_size = 12,
  dual = true,
  show_hidden = false,
  favorites = {
    { name = "Work", paths = { "C:\\Work" } },
  },
}
```

The file supports Lua data tables, quoted strings (escape Windows backslashes as
`\\`), numbers, booleans, `nil`, and `--` line or `--[[...]]` block comments.
Configuration scripts, functions and expressions are not executed. Saving from
the application rewrites the table and removes comments.

Besides the default **Dark**, there are Tokyo Night and Gruvbox (dark), and
Catppuccin Latte and Rosé Pine (light). These bring their own accent colour; the
one chosen in Settings applies to Dark. Saved `Midnight` and `Nord` settings open
as Tokyo Night, and `Light` as Catppuccin Latte.

On [Omarchy](https://omarchy.org/), Settings and the theme button also offer an
**Omarchy** theme. It takes its colours and accent from the current desktop theme
and follows `omarchy theme set` within a second, without a restart.

## Keys
Programs on removable drives and network locations ask before they run. Added drives and
pendrives have an eject button in the sidebar; right click offers the same.

Ctrl+C/X/V copy/cut/paste · Del delete (Recycle Bin) · F2 rename · Ctrl+Shift+N new folder ·
Ctrl+N new file · Ctrl+D duplicate · Ctrl+T/W/Tab tabs · Ctrl+Shift+T reopen tab ·
Alt+←/→ back/forward · Backspace parent · \ or Alt+D edit path · type to filter ·
Space preview · Ctrl+H hidden files · Ctrl+F search · F5 refresh · F10 terminal ·
Tab switch browser · drag to move, Ctrl+drag to copy.

While dragging inside the window, a wide tooltip separates the action and item
count, destination, and Ctrl hint. It flips away from window edges and shortens
long destination names to keep the text on single lines.

On **Windows**, Ctrl+C and Ctrl+X put actual files and folders on the system
clipboard: switch to Explorer (or another file manager) and press Ctrl+V to copy
or move them. Multiple selections and Unicode paths are supported; copying stays
available after closing Ferrous Commander. Ctrl+V also accepts files copied or cut
in Explorer. **Copy as path** continues to copy text only.
Drag a selected file or folder outside the window to pass the entire selection
to another application using Windows drag and drop. Windows shows the destination's
action; Ctrl requests a copy, Shift a move and Ctrl+Shift a shortcut where supported.
Escape cancels without removing the source. Drops within the window retain the
existing panel/folder/favorite behavior.

Click selects a file; Ctrl+click toggles a file and Shift+click selects a range.
Ctrl+Shift+Enter opens another panel, Ctrl+Shift+Q closes the active panel,
and Ctrl+Shift+J flips its split. Drag a divider to resize panels, or double-click
it to divide the space equally. Network drives appear under Drives and local
OneDrive and other sync folders under Cloud. Right-click a file for Open with: a submenu lists the applications registered for its
type (including Windows Store apps), with a system chooser for another application.
Windows lists only recommended handlers for that extension and refreshes its short-lived
cache as associations change. Discovery and invocation use separate STA helper processes;
handler failures and timeouts report an error without closing or blocking the file manager.
Traditional executables receive the file as one argument; packaged apps receive a Shell
IDataObject selection. Both preserve paths with spaces and Unicode characters.
Linux suggestions require a matching declared MIME type; unknown types have no fallback editors.
**Choose another app…** opens a searchable list of installed applications, including
Flatpak and Snap launchers, without changing file associations. Discovery honours XDG
application directories, desktop visibility, and user overrides. Launching uses `gio`
(from GLib, typically `libglib2.0-bin` on Debian/Ubuntu) to handle launcher arguments,
terminal applications and D-Bus activation correctly. Missing launchers report an error.
On Linux, going up from `/` opens the device overview; the column strip includes it too.
F10 supports Ghostty and Foot as well as the common GNOME, KDE and X11 terminals.
Copying on Linux preserves symbolic links (including broken links) and directory permissions.

Drag on empty list space or an unselected row to select several files with a rectangle.
Ctrl+drag adds to the selection; drag an already selected row to move files.
File operations have their own toolbar row; narrow toolbars scroll horizontally.
New panels split the last tile on the right/bottom, along its longer side.
Drag a tab to an edge to create a panel, or onto another panel to join its tabs.
Drop on the tab strip to reorder tabs; empty panels close automatically.
Each tab has its own × button, including the last tab in a panel. Closing the
last tab in the last panel closes the window. Controls use nearly square corners.
The highlighted area shows where the tab will land. Dividers resize panels.
Drag sidebar group headings to reorder groups. Drag a shortcut within its group to
reorder it, or onto another group to move it there. Right-click a shortcut and choose
**Rename…** to give it a sidebar name; the folder on disk keeps its name, its full
path stays in the tooltip, and an empty name restores the folder's own. Use
**+ → Add location…** in a
favorite group to save a local or UNC network path, even when offline. Cloud
automatically lists local sync folders; its tooltip explains the source and Settings
controls its visibility. Use **Settings → Restore groups…** to restore History, default
folders or a deleted group with its saved shortcuts. Closing History hides it
without discarding its entries.

Windows filesystem audit, tested behavior and remaining limits:
[WINDOWS_FILESYSTEM_AUDIT.md](WINDOWS_FILESYSTEM_AUDIT.md).

## Linux file support

Right-click an item → **Properties / Permissions** (or **Alt+Enter**) to inspect
its type, byte and allocated size, inode, device, hard-link count, timestamps and
symbolic-link target. Edit owner/group/other read, write and execute permissions,
octal mode, setuid, setgid, sticky bit, and numeric owner UID/group GID. Changes
apply to one item; Linux enforces ownership and privilege rules. Link targets are
never modified implicitly. Stale edits are rejected when the selected inode or its
permissions/ownership changed. There is no automatic privilege escalation.
Folder properties are also available from the empty-space context menu.
The file menu can create symbolic links and hard links in the other panel;
hard links require regular files on the same filesystem.

Linux copying requires GNU coreutils `cp`. Archive copying preserves symbolic
links (including broken/cyclic links), hard links within each selected tree,
permissions, ACLs, timestamps, extended attributes and sparse files where the
source/destination filesystems support them. New copies belong to the current
user; ownership is retained on same-filesystem moves. Cross-filesystem moves
copy first and delete the source only after success. Other rename errors never
trigger a copy-and-delete fallback. Copies are staged in a private directory,
then published without replacing any existing entry. Failures/cancellation retain
the source and report cleanup failures with the temporary directory's path.
Raw non-UTF-8 filename bytes are retained by copy/move/link operations; UI labels
use replacement characters for invalid UTF-8. Creation never truncates an existing
file, including through a dangling symbolic link. Renaming preserves spaces and
never replaces an existing Linux entry. Pipes and device files are not previewed.

ACL/xattr/flag inspection is read-only and uses optional `getfacl` (`acl`),
`getfattr` (`attr`), `lsattr` (`e2fsprogs`) and `timeout` (`coreutils`). Missing
utilities and unsupported/denied filesystem features are reported in Properties.
This is **not yet complete Linux parity**: recursive permission/ownership edits,
ACL/xattr/immutable-flag editing, SELinux/capability administration, mount management,
and archive editing/extraction do not have dedicated controls. Sockets cannot be
copied; device-node creation may require privileges. Copies are not snapshots of
concurrently modified files. Cross-filesystem moves cannot retain another user's
ownership, and hard links across separately selected roots are copied independently.
The editable name/configuration UI uses UTF-8, so arbitrary byte names cannot be
entered or round-tripped through saved sessions. Network access uses mounted paths.

## Tree files

Use the **Tree files** button (branch icon) in the bottom toolbar to toggle the tree. Click a folder’s arrow
to expand or collapse it in place; clicking its name or icon only selects it; double-click (or Enter) to open it. Right arrow
expands a branch and left arrow collapses it or selects its parent. Nested files
support the same selection, preview, rename and file operations as the regular list.
Expanded branches load in the background and refresh while visible. The view
preference is saved; navigating to another folder clears that panel’s branches.

## Folder colors

Right-click a folder in the file list or a shortcut in Favorites, Cloud or History
and choose **Folder color**. The same saved path color appears in every panel,
the sidebar, folder columns, tabs and preview. Choose **Clear color** to restore
the default folder icon. File-list color changes apply to the selected items;
sidebar changes apply to that shortcut's folder, independently of file selection.
Colors are saved in the existing `tags` table in settings.lua.

On Linux, Devices shows **System disk**, **Home**, and mounted volumes by their
filesystem label (or mount-folder name/model when no label is available). USB
storage is marked **USB**; other removable media is marked **Removable**. Hover
for the full mount path, device, model and filesystem. Repeated Btrfs system
subvolumes are hidden while Home remains accessible. Mounted media refreshes
automatically every ten seconds; unmounted devices are not listed or mounted.

## Drag files to other Linux apps

Drag selected files or folders out of the window into a file manager or a browser
upload area. The app offers real file URIs (native Wayland drag-and-drop, or Xdnd
on X11), including multiple files,
spaces and Unicode names. External drops copy; the destination handles the copy
or upload and source files stay in place. Escape or dropping on an unsupported
area cancels. Moving/copying between this app's panels keeps its existing behavior.

On Wayland desktops the app runs natively and starts the drag through the
compositor, so XWayland is not needed. The destination must accept file drops; browser security
rules still apply. This does not add a shared Linux file clipboard.
