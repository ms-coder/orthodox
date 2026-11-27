# OFM user guide

OFM is a two-panel file manager that you drive from the keyboard.
This guide uses the default **Far** keys. Press `F1` in OFM to see all keys.

Contents: [Concepts](#concepts) | [Everyday tasks](#everyday-tasks) | [Themes](#themes-and-file-highlighting) |
[User menu](#user-menu) | [Configuration](#configuration-files) | [Known limitations](#known-limitations) |
[Key reference](#key-reference)

## Concepts

### Two panels

- One panel is **active**. The other one is the default target for copy and move.
- `Tab` switches panels. `Ctrl+P` hides or shows the other panel.
- Each panel has tabs: `Ctrl+T` new, `Ctrl+W` close, `Ctrl+Shift+L` lock.
- A panel shows a folder, the inside of an archive, or a result list.

### Cursor and selection

- The **cursor** is the highlighted row.
- The **selection** is the set of marked files. `Ins` marks a file and moves down.
- A command works on the selection. Without a selection it works on the cursor file.
- Select by mask: `Alt+M` or keypad `+`. Examples: `*.txt`, `*.{jpg,png}`, `!*.tmp`.

### Command line and terminal

- Type, and the text goes to the command line under the panels. `Enter` runs it in the folder of the active panel.
- `Ctrl+O` hides the panels and shows the full terminal.
- With bash, zsh and fish the terminal and the active panel follow each other when you change folder.
  Other shells run, but they do not follow.
- Quick search: `Alt+letter` (Far and mc keys) or a plain letter (Total Commander keys).

### Jobs

Copy, move, delete and extract run in the background. `Ctrl+J` opens the jobs view.
There, `Space` pauses or resumes a job and `Del` cancels it.
OFM asks before it quits while jobs run.

## Everyday tasks

### Start OFM

```
ofm [LEFT_DIR [RIGHT_DIR]]
```

Without folders, OFM restores your last session: tabs, folders, cursor, views and window size.
With folders, it shows these instead of the stored ones.
If OFM is already running, a second `ofm` only shows the existing window and ignores the folders.

### Copy, move, rename, delete

| Key | Action |
|---|---|
| `F5` | Copy to the other panel (you can edit the target) |
| `F6` | Move |
| `Shift+F6` | Rename in place |
| `F7` | Make a folder |
| `F8` or `Del` | Move to Trash |
| `Shift+Del` | Delete permanently |
| `Alt+F6` | Make a symbolic or hard link |
| `Alt+Enter` | Properties: permissions and owner |

- A target such as `*.bak` renames while copying.
- In the copy dialog you can queue the job (`F2`) or run it in parallel.
- When a file exists you can overwrite, skip, rename, overwrite if newer or if the size differs,
  or compare the contents. You can apply the answer to all further conflicts.
- Copies keep permissions, times, symbolic links, extended attributes and ACLs where the target
  file system can. Ownership is kept when OFM runs as root. Sparse files stay sparse.
- Files are written under a temporary name and renamed when complete.
  A cancelled copy leaves no half file under the final name.

### Batch rename

Select files and press `Ctrl+Shift+M`. The names open in your editor, one per line.
Change them, save and close. OFM shows a preview before it renames.
Keep the number and order of the lines. `Ctrl+Alt+Z` undoes the last batch.

### View and edit

- `F3` opens the viewer. In it: `F2` wrap, `F4` hex, `F7` search, `Shift+F7` next, `F8` encoding,
  `Alt+F8` go to line or offset, `Esc` close.
- Encodings: UTF-8, UTF-16, CP1251, CP866, KOI8-R, Latin-1.
- `F3` on a folder calculates its size.
- `Ctrl+Q` shows a quick view of the cursor file in the other panel.
- `F4` edits, `Shift+F4` creates a new file. OFM uses the editor from `config.toml`, else `$VISUAL`,
  else `$EDITOR`, else GNOME Text Editor. Terminal editors run in the built-in terminal.
- `Enter` opens a file with its default application. A script or program with the execute bit runs
  in the terminal.

### Find files

`Alt+F7` searches by name mask, content (text or regular expression), size and date.
It can follow links and look inside archives. Results appear while the search runs.
You can view or edit a result, go to it, or send all results to a panel (`F10`).

A result panel (also `Ctrl+Alt+P`, which lists the output of a shell command) holds the real files:
copy, move and delete work on them. `Backspace` goes back.

### Archives

Press `Enter` on a zip, tar, tar.gz, tar.bz2, tar.xz, tar.zst or 7z file to look inside.
Archives are read-only. `F5` extracts. OFM asks for a password when needed.
Cancelling the password prompt extracts nothing.

### Compare folders

`Shift+F2` compares the two panels and marks what differs.
Compare by name, size and time (default), by name, by name and size, or by content.

### Mouse, clipboard and other programs

- Click, double-click, right-click menu, `Shift` and `Ctrl` click work. `F9` opens the menu bar.
- `Ctrl+C`, `Ctrl+X`, `Ctrl+V` exchange files with GNOME Files. Drag and drop works.
- `Alt+F1` and `Alt+F2` open the drive menu for the left and right panel.
- `Ctrl+D` opens the hotlist. `Ctrl+Shift+D` adds the current folder.

## Themes and file highlighting

Three themes ship: **Far Blue** (default), **Commander Light** and **Adwaita**
(follows the GNOME light or dark setting). `Ctrl+Alt+T` switches to the next one and OFM remembers it.

### Your own theme

1. Copy a file from `/usr/share/doc/ofm/examples/themes/` to `~/.config/ofm/themes/mine.toml`.
2. Change `id` and `name`. (A theme with the `id` of a built-in theme replaces it.)
3. Edit the colours. Colours are `"#RRGGBB"` or `"#RRGGBBAA"`.
4. Select it in `config.toml`: `[theme]` then `name = "mine"`, or press `Ctrl+Alt+T`.

Start from a complete file, not an empty one.
OFM reloads theme files when you save them (`live_reload = true`).

### Highlighting rules

Rules are checked from top to bottom. The first match wins.

```toml
[[highlight]]
attr = "image"          # dir, hidden, executable, symlink, broken_symlink, archive, image, source
fg = "#FFFF55"
icon = "image-x-generic"

[[highlight]]
mask = "*.log;*.bak"    # glob list, separated by ';'
fg = "#808080"
```

A rule can set `fg`, `bg`, `bold` and `icon`. If a rule has both `attr` and `mask`, both must match.

### Icons

`icon` is the name of an icon in your icon theme, not a file path.
If any rule of a theme names an icon, every row gets an icon. Rows without a rule icon get a folder or
a plain file icon. Commander Light has icons. Far Blue is text only.

## User menu

`F2` opens a menu of your own commands. Press the item's letter or `Enter`.
`Ctrl+Alt+M` edits the global menu, `~/.config/ofm/menu.toml`.

```toml
version = 1

[[item]]
key = "T"                       # letter inside the menu
title = "tar.gz selection"
command = "tar -czf %{input:Archive name}.tar.gz %s"
mask = ""                       # show only when the cursor file matches
terminal = true                 # false = run in the background, output hidden
confirm = false                 # ask before running
```

| Placeholder | Meaning |
|---|---|
| `%f` | File under the cursor |
| `%s` | Selected names, or the cursor file |
| `%d` | Folder of the active panel |
| `%D` | Folder of the other panel |
| `%F` | File under the cursor in the other panel |
| `%n` | Cursor file name without extension |
| `%{input:Prompt}` | OFM asks you |
| `%%` | A literal `%` |

OFM quotes the names for the shell. Do not add your own quotes around placeholders.

### Menu per folder

A file `.ofm-menu.toml` in a folder adds entries, listed first. `Ctrl+Alt+Shift+M` creates or edits it.
Because it can run any command, OFM shows it and asks you to trust it first, and again after every change.
Do not trust a menu you do not understand.

## Configuration files

| File | Content |
|---|---|
| `~/.config/ofm/config.toml` | Settings |
| `~/.config/ofm/keymap.toml` | Your key bindings |
| `~/.config/ofm/menu.toml` | User menu |
| `~/.config/ofm/themes/*.toml` | Your themes |
| `~/.local/state/ofm/` | Session, history, hotlist |

`XDG_CONFIG_HOME` and `XDG_STATE_HOME` change these places.

Every setting is optional. A commented copy with all defaults is in
`/usr/share/doc/ofm/examples/config.toml`.
`Ctrl+,` opens Preferences, where you can open the files.
After you edit `config.toml` or `keymap.toml`, press `Ctrl+Alt+R`.
A mistake in a file does not stop OFM: it uses defaults and tells you.

Common settings:

```toml
[keymap]
preset = "far"          # far, tc or mc

[theme]
name = "far-blue"       # far-blue, commander-light, adwaita or your own

[panels]
default_view = "brief"  # brief, full or custom
show_hidden = false     # Ctrl+H toggles in the panel

[jobs]
trash = true            # false: F8 deletes permanently (asks first)

[editor]
command = ""            # for example "gnome-text-editor %f"

[terminal]
shell = ""              # empty = $SHELL
```

### Rebind keys

`keymap.toml` is applied on top of the preset. An empty command removes a binding.

```toml
version = 1

[panels]
"F3" = "file.edit"      # change a key
"Ctrl+Q" = ""           # unbind a key
```

Sections: `[global]`, `[panels]`, `[cmdline]`, `[viewer]`, `[dialog]`.
Modifiers: `Ctrl+`, `Alt+`, `Shift+`, `Super+`. Keypad keys: `Gray+`, `Gray-`, `Gray*`, `Gray/`.
The command IDs are in the last column of the tables below.

## Known limitations

- Archives cannot be created or changed.
- `F3` and `F4` on a file inside an archive stop at 64 MiB. Extract larger files with `F5`.
- Extraction, delete and permission changes need `/proc` to be mounted.
- Batch rename undo matches names, not files. If you delete a renamed file and create a new one
  with the same name, undo renames the new one. Nothing is overwritten.
- A crash or power loss can leave hidden temporary files named `.NAME.ofm-part-...`.
  OFM never removes them. Find them with the mask `.*.ofm-part-*` and delete them yourself.
- Hard links among copied files become separate copies. Sockets, pipes and device files are skipped.
- The viewer cannot select or copy text, and ignores case only for ASCII letters when searching.
- A second `ofm` command does not open its folders in the running window.
- GNOME may take keys such as `Alt+F2` before OFM sees them.
- The interface is in English only.
- `mc` key sequences such as `Ctrl+X Q` do not exist; those actions use single keys.

## Key reference

These tables come from `ofm --dump-keys far`, `tc` and `mc`. Your own `keymap.toml` is not included.

<!-- keys -->

### Far Manager (default) preset

Plain letter keys are appended to the command line; Alt+letter starts a quick search.

#### global (far)

Always available in the main window.

| Key | Action | Command ID |
|---|---|---|
| Ctrl+, | Preferences | `app.preferences` |
| Ctrl+J | Jobs view | `app.jobs` |
| Ctrl+O | Show/hide panels (full terminal) | `app.terminal_toggle` |
| Ctrl+Alt+R | Reload configuration | `app.reload_config` |
| Ctrl+Alt+T | Next theme | `app.theme_next` |
| F1 | Help | `app.help` |
| F9 | Menu bar | `app.menu` |
| F10 | Quit | `app.quit` |
| F11 | Toggle fullscreen | `window.fullscreen` |

#### panels (far)

Main window with the file panels (the normal state).

| Key | Action | Command ID |
|---|---|---|
| Ctrl+1 | Brief view | `panel.view_brief` |
| Ctrl+2 | Full view | `panel.view_full` |
| Ctrl+3 | Custom columns view | `panel.view_custom` |
| Alt+= | Other panel to this directory | `panel.same_dir` |
| Ctrl+A | Select all | `select.all` |
| Ctrl+Shift+A | Deselect all | `select.none` |
| Ctrl+C | Copy files to clipboard | `file.clipboard_copy` |
| Ctrl+Shift+C | Copy full path to clipboard | `file.copy_path` |
| Ctrl+D | Hotlist / bookmarks | `panel.hotlist` |
| Ctrl+Shift+D | Add directory to hotlist | `panel.hotlist_add` |
| Ctrl+E | Command history: previous | `cmdline.history_prev` |
| Ctrl+Shift+E | Command history: next | `cmdline.history_next` |
| Ctrl+F | Insert full path | `cmdline.insert_fullpath` |
| Ctrl+H | Show/hide hidden files | `panel.toggle_hidden` |
| Ctrl+Shift+I | Invert selection | `select.invert` |
| Ctrl+L | Go to path... | `panel.goto_path` |
| Ctrl+Shift+L | Lock/unlock tab | `tab.lock` |
| Alt+M | Select by mask | `select.by_mask` |
| Alt+Shift+M | Deselect by mask | `select.unmask` |
| Ctrl+M | Restore previous selection | `select.restore` |
| Ctrl+Shift+M | Batch rename in editor | `file.batch_rename` |
| Ctrl+Alt+M | Edit user menu | `menu.edit` |
| Ctrl+Alt+Shift+M | Edit local user menu (this folder) | `menu.edit_local` |
| Ctrl+P | Show/hide other panel | `panel.toggle_other` |
| Ctrl+Alt+P | Panelize command output | `search.panelize_command` |
| Ctrl+Q | Quick view in other panel | `panel.toggle_quickview` |
| Ctrl+R | Refresh | `panel.refresh` |
| Ctrl+S | Quick search | `panel.quicksearch` |
| Ctrl+Shift+S | Filter as you type | `panel.filter` |
| Ctrl+T | New tab | `tab.new` |
| Ctrl+U | Swap panels | `panel.swap` |
| Ctrl+V | Paste files from clipboard | `file.clipboard_paste` |
| Ctrl+W | Close tab | `tab.close` |
| Ctrl+X | Cut files to clipboard | `file.clipboard_cut` |
| Ctrl+Alt+Z | Undo last batch rename | `file.batch_rename_undo` |
| Ctrl+[ | Insert left panel path | `cmdline.insert_left_path` |
| Ctrl+\ | Go to root directory | `panel.go_root` |
| Ctrl+] | Insert right panel path | `cmdline.insert_right_path` |
| Alt+F1 | Drive/mount menu (left panel) | `panel.drive_menu_left` |
| F2 | User menu | `menu.user` |
| Shift+F2 | Compare directories | `panel.compare_dirs` |
| Alt+F2 | Drive/mount menu (right panel) | `panel.drive_menu_right` |
| F3 | View (directory: calculate size) | `file.view` |
| Ctrl+F3 | Sort by name (again = reverse) | `panel.sort_name` |
| F4 | Edit | `file.edit` |
| Shift+F4 | New file (edit) | `file.new_file` |
| Ctrl+F4 | Sort by extension (again = reverse) | `panel.sort_ext` |
| F5 | Copy | `file.copy` |
| Ctrl+F5 | Sort by modified time (again = reverse) | `panel.sort_mtime` |
| F6 | Move | `file.move` |
| Shift+F6 | Rename in place | `file.rename` |
| Alt+F6 | Create link | `file.link` |
| Ctrl+F6 | Sort by size (again = reverse) | `panel.sort_size` |
| F7 | Make directory | `file.mkdir` |
| Alt+F7 | Find files | `search.find` |
| F8 | Delete (to Trash) | `file.delete` |
| Shift+F8 | Delete permanently | `file.delete_permanent` |
| Alt+F8 | Command history list | `cmdline.history_list` |
| Shift+F10 | Context menu | `panel.context_menu` |
| Alt+F12 | Directory history list | `panel.history_list` |
| Ctrl+F12 | Reverse sort order | `panel.sort_reverse` |
| Up | Cursor up | `panel.cursor_up` |
| Shift+Up | Extend selection up | `select.extend_up` |
| Ctrl+Up | Quick search: previous match | `panel.quicksearch_prev` |
| Down | Cursor down | `panel.cursor_down` |
| Shift+Down | Extend selection down | `select.extend_down` |
| Ctrl+Down | Quick search: next match | `panel.quicksearch_next` |
| Left | Cursor left (previous column) | `panel.cursor_left` |
| Alt+Left | History back | `panel.history_back` |
| Right | Cursor right (next column) | `panel.cursor_right` |
| Alt+Right | History forward | `panel.history_forward` |
| PgUp | Page up | `panel.page_up` |
| Shift+PgUp | Extend selection page up | `select.extend_page_up` |
| Ctrl+PgUp | Go to parent directory | `panel.go_up` |
| PgDn | Page down | `panel.page_down` |
| Shift+PgDn | Extend selection page down | `select.extend_page_down` |
| Ctrl+PgDn | Enter directory or archive (never run) | `panel.enter_dir` |
| Home | First entry | `panel.home` |
| Shift+Home | Extend selection to first | `select.extend_home` |
| Ctrl+Home | Go to home directory | `panel.go_home` |
| End | Last entry | `panel.end` |
| Shift+End | Extend selection to last | `select.extend_end` |
| Enter | Open / enter / run | `panel.open` |
| Shift+Enter | Open with... | `file.open_with` |
| Alt+Enter | Properties / attributes | `file.properties` |
| Alt+Shift+Enter | Calculate directory size | `file.calc_size` |
| Ctrl+Enter | Insert file name | `cmdline.insert_name` |
| Esc | Cancel quick search / filter | `panel.cancel` |
| Tab | Switch active panel | `panel.switch` |
| Ctrl+Tab | Next tab | `tab.next` |
| Ctrl+Shift+Tab | Previous tab | `tab.prev` |
| Backspace | Go to parent directory | `panel.go_up` |
| Del | Delete (to Trash) | `file.delete` |
| Shift+Del | Delete permanently | `file.delete_permanent` |
| Ins | Toggle selection and move down | `select.toggle` |
| Menu | Context menu | `panel.context_menu` |
| Gray+ | Select by mask | `select.by_mask` |
| Ctrl+Gray+ | Select all | `select.all` |
| Gray- | Deselect by mask | `select.unmask` |
| Ctrl+Gray- | Deselect all | `select.none` |
| Gray* | Invert selection | `select.invert` |

#### cmdline (far)

While the command line under the panels contains text.

| Key | Action | Command ID |
|---|---|---|
| Enter | Execute command line | `cmdline.execute` |
| Esc | Clear command line | `cmdline.clear` |
| Tab | Complete path | `cmdline.complete` |
| Ctrl+Space | Complete path | `cmdline.complete` |

#### viewer (far)

Inside the F3 viewer window.

| Key | Action | Command ID |
|---|---|---|
| F2 | Toggle line wrap | `viewer.wrap` |
| F4 | Toggle hex mode | `viewer.toggle_hex` |
| F7 | Search | `viewer.search` |
| Shift+F7 | Search next | `viewer.search_next` |
| Alt+F7 | Search previous | `viewer.search_prev` |
| F8 | Choose encoding | `viewer.encoding` |
| Alt+F8 | Go to line / offset | `viewer.goto` |
| F10 | Close viewer | `viewer.close` |
| Up | Scroll up one line | `viewer.line_up` |
| Down | Scroll down one line | `viewer.line_down` |
| Left | Scroll left | `viewer.scroll_left` |
| Right | Scroll right | `viewer.scroll_right` |
| PgUp | Page up | `viewer.page_up` |
| PgDn | Page down | `viewer.page_down` |
| Home | Go to start | `viewer.top` |
| End | Go to end | `viewer.bottom` |
| Esc | Close viewer | `viewer.close` |
| Space | Page down | `viewer.page_down` |
| Gray+ | Next file | `viewer.next_file` |
| Gray- | Previous file | `viewer.prev_file` |

#### dialog (far)

Inside modal dialogs and the jobs view.

| Key | Action | Command ID |
|---|---|---|
| Ctrl+A | Apply decision to all | `dialog.apply_all` |
| Ctrl+L | Clear finished jobs | `jobs.clear_finished` |
| F2 | Add to queue (instead of running in parallel) | `dialog.queue` |
| F10 | Panelize search results | `search.panelize_results` |
| Enter | Confirm | `dialog.ok` |
| Esc | Cancel | `dialog.cancel` |
| Space | Pause / resume job | `jobs.pause_resume` |
| Del | Cancel job | `jobs.cancel` |

### Total / Double Commander preset

Plain letter keys start a quick search in the active panel.

#### global (tc)

Always available in the main window.

| Key | Action | Command ID |
|---|---|---|
| Ctrl+, | Preferences | `app.preferences` |
| Ctrl+J | Jobs view | `app.jobs` |
| Ctrl+O | Show/hide panels (full terminal) | `app.terminal_toggle` |
| Ctrl+Alt+R | Reload configuration | `app.reload_config` |
| Ctrl+Alt+T | Next theme | `app.theme_next` |
| F1 | Help | `app.help` |
| Alt+F4 | Quit | `app.quit` |
| F9 | Menu bar | `app.menu` |
| F10 | Menu bar | `app.menu` |
| F11 | Toggle fullscreen | `window.fullscreen` |

#### panels (tc)

Main window with the file panels (the normal state).

| Key | Action | Command ID |
|---|---|---|
| Ctrl+1 | Brief view | `panel.view_brief` |
| Ctrl+2 | Full view | `panel.view_full` |
| Ctrl+3 | Custom columns view | `panel.view_custom` |
| Alt+= | Other panel to this directory | `panel.same_dir` |
| Ctrl+A | Select all | `select.all` |
| Ctrl+Shift+A | Deselect all | `select.none` |
| Ctrl+C | Copy files to clipboard | `file.clipboard_copy` |
| Ctrl+Shift+C | Copy full path to clipboard | `file.copy_path` |
| Ctrl+D | Hotlist / bookmarks | `panel.hotlist` |
| Ctrl+Shift+D | Add directory to hotlist | `panel.hotlist_add` |
| Ctrl+E | Command history: previous | `cmdline.history_prev` |
| Ctrl+Shift+E | Command history: next | `cmdline.history_next` |
| Ctrl+F | Insert full path | `cmdline.insert_fullpath` |
| Ctrl+H | Show/hide hidden files | `panel.toggle_hidden` |
| Ctrl+Shift+I | Invert selection | `select.invert` |
| Ctrl+L | Go to path... | `panel.goto_path` |
| Ctrl+Shift+L | Lock/unlock tab | `tab.lock` |
| Alt+M | Select by mask | `select.by_mask` |
| Alt+Shift+M | Deselect by mask | `select.unmask` |
| Ctrl+M | Batch rename in editor | `file.batch_rename` |
| Ctrl+Shift+M | Batch rename in editor | `file.batch_rename` |
| Ctrl+Alt+M | Edit user menu | `menu.edit` |
| Ctrl+Alt+Shift+M | Edit local user menu (this folder) | `menu.edit_local` |
| Ctrl+P | Show/hide other panel | `panel.toggle_other` |
| Ctrl+Alt+P | Panelize command output | `search.panelize_command` |
| Ctrl+Q | Quick view in other panel | `panel.toggle_quickview` |
| Ctrl+R | Refresh | `panel.refresh` |
| Ctrl+Shift+R | Restore previous selection | `select.restore` |
| Ctrl+S | Quick search | `panel.quicksearch` |
| Ctrl+Shift+S | Filter as you type | `panel.filter` |
| Ctrl+T | New tab | `tab.new` |
| Ctrl+U | Swap panels | `panel.swap` |
| Ctrl+V | Paste files from clipboard | `file.clipboard_paste` |
| Ctrl+W | Close tab | `tab.close` |
| Ctrl+X | Cut files to clipboard | `file.clipboard_cut` |
| Ctrl+Alt+Z | Undo last batch rename | `file.batch_rename_undo` |
| Ctrl+[ | Insert left panel path | `cmdline.insert_left_path` |
| Ctrl+\ | Go to root directory | `panel.go_root` |
| Ctrl+] | Insert right panel path | `cmdline.insert_right_path` |
| Alt+F1 | Drive/mount menu (left panel) | `panel.drive_menu_left` |
| F2 | User menu | `menu.user` |
| Shift+F2 | Compare directories | `panel.compare_dirs` |
| Alt+F2 | Drive/mount menu (right panel) | `panel.drive_menu_right` |
| F3 | View (directory: calculate size) | `file.view` |
| Ctrl+F3 | Sort by name (again = reverse) | `panel.sort_name` |
| F4 | Edit | `file.edit` |
| Shift+F4 | New file (edit) | `file.new_file` |
| Ctrl+F4 | Sort by extension (again = reverse) | `panel.sort_ext` |
| F5 | Copy | `file.copy` |
| Ctrl+F5 | Sort by modified time (again = reverse) | `panel.sort_mtime` |
| F6 | Move | `file.move` |
| Shift+F6 | Rename in place | `file.rename` |
| Alt+F6 | Create link | `file.link` |
| Ctrl+F6 | Sort by size (again = reverse) | `panel.sort_size` |
| F7 | Make directory | `file.mkdir` |
| Alt+F7 | Find files | `search.find` |
| F8 | Delete (to Trash) | `file.delete` |
| Shift+F8 | Delete permanently | `file.delete_permanent` |
| Alt+F8 | Command history list | `cmdline.history_list` |
| Shift+F10 | Context menu | `panel.context_menu` |
| Alt+F12 | Directory history list | `panel.history_list` |
| Ctrl+F12 | Reverse sort order | `panel.sort_reverse` |
| Up | Cursor up | `panel.cursor_up` |
| Shift+Up | Extend selection up | `select.extend_up` |
| Ctrl+Up | Quick search: previous match | `panel.quicksearch_prev` |
| Down | Cursor down | `panel.cursor_down` |
| Shift+Down | Extend selection down | `select.extend_down` |
| Ctrl+Down | Quick search: next match | `panel.quicksearch_next` |
| Left | Cursor left (previous column) | `panel.cursor_left` |
| Alt+Left | History back | `panel.history_back` |
| Right | Cursor right (next column) | `panel.cursor_right` |
| Alt+Right | History forward | `panel.history_forward` |
| PgUp | Page up | `panel.page_up` |
| Shift+PgUp | Extend selection page up | `select.extend_page_up` |
| Ctrl+PgUp | Go to parent directory | `panel.go_up` |
| PgDn | Page down | `panel.page_down` |
| Shift+PgDn | Extend selection page down | `select.extend_page_down` |
| Ctrl+PgDn | Enter directory or archive (never run) | `panel.enter_dir` |
| Home | First entry | `panel.home` |
| Shift+Home | Extend selection to first | `select.extend_home` |
| Ctrl+Home | Go to home directory | `panel.go_home` |
| End | Last entry | `panel.end` |
| Shift+End | Extend selection to last | `select.extend_end` |
| Enter | Open / enter / run | `panel.open` |
| Shift+Enter | Open with... | `file.open_with` |
| Alt+Enter | Properties / attributes | `file.properties` |
| Alt+Shift+Enter | Calculate directory size | `file.calc_size` |
| Ctrl+Enter | Insert file name | `cmdline.insert_name` |
| Esc | Cancel quick search / filter | `panel.cancel` |
| Tab | Switch active panel | `panel.switch` |
| Ctrl+Tab | Next tab | `tab.next` |
| Ctrl+Shift+Tab | Previous tab | `tab.prev` |
| Space | Toggle selection and move down | `select.toggle` |
| Ctrl+Space | Calculate directory size | `file.calc_size` |
| Backspace | Go to parent directory | `panel.go_up` |
| Del | Delete (to Trash) | `file.delete` |
| Shift+Del | Delete permanently | `file.delete_permanent` |
| Ins | Toggle selection and move down | `select.toggle` |
| Menu | Context menu | `panel.context_menu` |
| Gray+ | Select by mask | `select.by_mask` |
| Ctrl+Gray+ | Select all | `select.all` |
| Gray- | Deselect by mask | `select.unmask` |
| Ctrl+Gray- | Deselect all | `select.none` |
| Gray* | Invert selection | `select.invert` |

#### cmdline (tc)

While the command line under the panels contains text.

| Key | Action | Command ID |
|---|---|---|
| Enter | Execute command line | `cmdline.execute` |
| Esc | Clear command line | `cmdline.clear` |
| Tab | Complete path | `cmdline.complete` |
| Ctrl+Space | Complete path | `cmdline.complete` |

#### viewer (tc)

Inside the F3 viewer window.

| Key | Action | Command ID |
|---|---|---|
| F2 | Toggle line wrap | `viewer.wrap` |
| F4 | Toggle hex mode | `viewer.toggle_hex` |
| F7 | Search | `viewer.search` |
| Shift+F7 | Search next | `viewer.search_next` |
| Alt+F7 | Search previous | `viewer.search_prev` |
| F8 | Choose encoding | `viewer.encoding` |
| Alt+F8 | Go to line / offset | `viewer.goto` |
| F10 | Close viewer | `viewer.close` |
| Up | Scroll up one line | `viewer.line_up` |
| Down | Scroll down one line | `viewer.line_down` |
| Left | Scroll left | `viewer.scroll_left` |
| Right | Scroll right | `viewer.scroll_right` |
| PgUp | Page up | `viewer.page_up` |
| PgDn | Page down | `viewer.page_down` |
| Home | Go to start | `viewer.top` |
| End | Go to end | `viewer.bottom` |
| Esc | Close viewer | `viewer.close` |
| Space | Page down | `viewer.page_down` |
| Gray+ | Next file | `viewer.next_file` |
| Gray- | Previous file | `viewer.prev_file` |

#### dialog (tc)

Inside modal dialogs and the jobs view.

| Key | Action | Command ID |
|---|---|---|
| Ctrl+A | Apply decision to all | `dialog.apply_all` |
| Ctrl+L | Clear finished jobs | `jobs.clear_finished` |
| F2 | Add to queue (instead of running in parallel) | `dialog.queue` |
| F10 | Panelize search results | `search.panelize_results` |
| Enter | Confirm | `dialog.ok` |
| Esc | Cancel | `dialog.cancel` |
| Space | Pause / resume job | `jobs.pause_resume` |
| Del | Cancel job | `jobs.cancel` |

### Midnight Commander preset

Plain letter keys are appended to the command line; Alt+letter starts a quick search.

#### global (mc)

Always available in the main window.

| Key | Action | Command ID |
|---|---|---|
| Ctrl+, | Preferences | `app.preferences` |
| Ctrl+J | Jobs view | `app.jobs` |
| Ctrl+O | Show/hide panels (full terminal) | `app.terminal_toggle` |
| Ctrl+Alt+R | Reload configuration | `app.reload_config` |
| Ctrl+Alt+T | Next theme | `app.theme_next` |
| F1 | Help | `app.help` |
| F9 | Menu bar | `app.menu` |
| F10 | Quit | `app.quit` |
| F11 | Toggle fullscreen | `window.fullscreen` |

#### panels (mc)

Main window with the file panels (the normal state).

| Key | Action | Command ID |
|---|---|---|
| Alt+. | Show/hide hidden files | `panel.toggle_hidden` |
| Ctrl+1 | Brief view | `panel.view_brief` |
| Ctrl+2 | Full view | `panel.view_full` |
| Ctrl+3 | Custom columns view | `panel.view_custom` |
| Alt+= | Other panel to this directory | `panel.same_dir` |
| Ctrl+A | Select all | `select.all` |
| Ctrl+Shift+A | Deselect all | `select.none` |
| Ctrl+C | Copy files to clipboard | `file.clipboard_copy` |
| Ctrl+Shift+C | Copy full path to clipboard | `file.copy_path` |
| Ctrl+D | Hotlist / bookmarks | `panel.hotlist` |
| Ctrl+Shift+D | Add directory to hotlist | `panel.hotlist_add` |
| Ctrl+E | Command history: previous | `cmdline.history_prev` |
| Ctrl+Shift+E | Command history: next | `cmdline.history_next` |
| Ctrl+F | Insert full path | `cmdline.insert_fullpath` |
| Alt+H | Directory history list | `panel.history_list` |
| Ctrl+H | Show/hide hidden files | `panel.toggle_hidden` |
| Ctrl+Shift+I | Invert selection | `select.invert` |
| Ctrl+L | Go to path... | `panel.goto_path` |
| Ctrl+Shift+L | Lock/unlock tab | `tab.lock` |
| Alt+M | Select by mask | `select.by_mask` |
| Alt+Shift+M | Deselect by mask | `select.unmask` |
| Ctrl+M | Restore previous selection | `select.restore` |
| Ctrl+Shift+M | Batch rename in editor | `file.batch_rename` |
| Ctrl+Alt+M | Edit user menu | `menu.edit` |
| Ctrl+Alt+Shift+M | Edit local user menu (this folder) | `menu.edit_local` |
| Alt+O | Other panel to this directory | `panel.same_dir` |
| Ctrl+P | Show/hide other panel | `panel.toggle_other` |
| Ctrl+Alt+P | Panelize command output | `search.panelize_command` |
| Ctrl+Q | Quick view in other panel | `panel.toggle_quickview` |
| Ctrl+R | Refresh | `panel.refresh` |
| Ctrl+S | Quick search | `panel.quicksearch` |
| Ctrl+Shift+S | Filter as you type | `panel.filter` |
| Ctrl+T | Toggle selection and move down | `select.toggle` |
| Ctrl+Shift+T | New tab | `tab.new` |
| Ctrl+U | Swap panels | `panel.swap` |
| Ctrl+V | Paste files from clipboard | `file.clipboard_paste` |
| Ctrl+W | Close tab | `tab.close` |
| Ctrl+X | Cut files to clipboard | `file.clipboard_cut` |
| Ctrl+Alt+Z | Undo last batch rename | `file.batch_rename_undo` |
| Ctrl+[ | Insert left panel path | `cmdline.insert_left_path` |
| Ctrl+\ | Hotlist / bookmarks | `panel.hotlist` |
| Ctrl+Shift+\ | Go to root directory | `panel.go_root` |
| Ctrl+] | Insert right panel path | `cmdline.insert_right_path` |
| Alt+F1 | Drive/mount menu (left panel) | `panel.drive_menu_left` |
| F2 | User menu | `menu.user` |
| Shift+F2 | Compare directories | `panel.compare_dirs` |
| Alt+F2 | Drive/mount menu (right panel) | `panel.drive_menu_right` |
| F3 | View (directory: calculate size) | `file.view` |
| Ctrl+F3 | Sort by name (again = reverse) | `panel.sort_name` |
| F4 | Edit | `file.edit` |
| Shift+F4 | New file (edit) | `file.new_file` |
| Ctrl+F4 | Sort by extension (again = reverse) | `panel.sort_ext` |
| F5 | Copy | `file.copy` |
| Ctrl+F5 | Sort by modified time (again = reverse) | `panel.sort_mtime` |
| F6 | Move | `file.move` |
| Shift+F6 | Rename in place | `file.rename` |
| Alt+F6 | Create link | `file.link` |
| Ctrl+F6 | Sort by size (again = reverse) | `panel.sort_size` |
| F7 | Make directory | `file.mkdir` |
| Alt+F7 | Find files | `search.find` |
| F8 | Delete (to Trash) | `file.delete` |
| Shift+F8 | Delete permanently | `file.delete_permanent` |
| Alt+F8 | Command history list | `cmdline.history_list` |
| Shift+F10 | Context menu | `panel.context_menu` |
| Alt+F12 | Directory history list | `panel.history_list` |
| Ctrl+F12 | Reverse sort order | `panel.sort_reverse` |
| Up | Cursor up | `panel.cursor_up` |
| Shift+Up | Extend selection up | `select.extend_up` |
| Ctrl+Up | Quick search: previous match | `panel.quicksearch_prev` |
| Down | Cursor down | `panel.cursor_down` |
| Shift+Down | Extend selection down | `select.extend_down` |
| Ctrl+Down | Quick search: next match | `panel.quicksearch_next` |
| Left | Cursor left (previous column) | `panel.cursor_left` |
| Alt+Left | History back | `panel.history_back` |
| Right | Cursor right (next column) | `panel.cursor_right` |
| Alt+Right | History forward | `panel.history_forward` |
| PgUp | Page up | `panel.page_up` |
| Shift+PgUp | Extend selection page up | `select.extend_page_up` |
| Ctrl+PgUp | Go to parent directory | `panel.go_up` |
| PgDn | Page down | `panel.page_down` |
| Shift+PgDn | Extend selection page down | `select.extend_page_down` |
| Ctrl+PgDn | Enter directory or archive (never run) | `panel.enter_dir` |
| Home | First entry | `panel.home` |
| Shift+Home | Extend selection to first | `select.extend_home` |
| Ctrl+Home | Go to home directory | `panel.go_home` |
| End | Last entry | `panel.end` |
| Shift+End | Extend selection to last | `select.extend_end` |
| Enter | Open / enter / run | `panel.open` |
| Shift+Enter | Open with... | `file.open_with` |
| Alt+Enter | Properties / attributes | `file.properties` |
| Alt+Shift+Enter | Calculate directory size | `file.calc_size` |
| Ctrl+Enter | Insert file name | `cmdline.insert_name` |
| Esc | Cancel quick search / filter | `panel.cancel` |
| Tab | Switch active panel | `panel.switch` |
| Ctrl+Tab | Next tab | `tab.next` |
| Ctrl+Shift+Tab | Previous tab | `tab.prev` |
| Space | Toggle selection and move down | `select.toggle` |
| Backspace | Go to parent directory | `panel.go_up` |
| Del | Delete (to Trash) | `file.delete` |
| Shift+Del | Delete permanently | `file.delete_permanent` |
| Ins | Toggle selection and move down | `select.toggle` |
| Menu | Context menu | `panel.context_menu` |
| Gray+ | Select by mask | `select.by_mask` |
| Ctrl+Gray+ | Select all | `select.all` |
| Gray- | Deselect by mask | `select.unmask` |
| Ctrl+Gray- | Deselect all | `select.none` |
| Gray* | Invert selection | `select.invert` |

#### cmdline (mc)

While the command line under the panels contains text.

| Key | Action | Command ID |
|---|---|---|
| Enter | Execute command line | `cmdline.execute` |
| Esc | Clear command line | `cmdline.clear` |
| Tab | Complete path | `cmdline.complete` |
| Ctrl+Space | Complete path | `cmdline.complete` |

#### viewer (mc)

Inside the F3 viewer window.

| Key | Action | Command ID |
|---|---|---|
| F2 | Toggle line wrap | `viewer.wrap` |
| F4 | Toggle hex mode | `viewer.toggle_hex` |
| F7 | Search | `viewer.search` |
| Shift+F7 | Search next | `viewer.search_next` |
| Alt+F7 | Search previous | `viewer.search_prev` |
| F8 | Choose encoding | `viewer.encoding` |
| Alt+F8 | Go to line / offset | `viewer.goto` |
| F10 | Close viewer | `viewer.close` |
| Up | Scroll up one line | `viewer.line_up` |
| Down | Scroll down one line | `viewer.line_down` |
| Left | Scroll left | `viewer.scroll_left` |
| Right | Scroll right | `viewer.scroll_right` |
| PgUp | Page up | `viewer.page_up` |
| PgDn | Page down | `viewer.page_down` |
| Home | Go to start | `viewer.top` |
| End | Go to end | `viewer.bottom` |
| Esc | Close viewer | `viewer.close` |
| Space | Page down | `viewer.page_down` |
| Gray+ | Next file | `viewer.next_file` |
| Gray- | Previous file | `viewer.prev_file` |

#### dialog (mc)

Inside modal dialogs and the jobs view.

| Key | Action | Command ID |
|---|---|---|
| Ctrl+A | Apply decision to all | `dialog.apply_all` |
| Ctrl+L | Clear finished jobs | `jobs.clear_finished` |
| F2 | Add to queue (instead of running in parallel) | `dialog.queue` |
| F10 | Panelize search results | `search.panelize_results` |
| Enter | Confirm | `dialog.ok` |
| Esc | Cancel | `dialog.cancel` |
| Space | Pause / resume job | `jobs.pause_resume` |
| Del | Cancel job | `jobs.cancel` |
