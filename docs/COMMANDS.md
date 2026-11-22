# Commands

Every user action is a `CommandId` (see `crates/ofm-core/src/commands.rs`). "Context" is the keymap section
holding the default binding (`global` = always in the main window, `panels` = main window,
`cmdline` = main window while the command line is non-empty (takes priority over `panels`),
`viewer`, `dialog`; see ADR-0005). tc/mc columns show only differences; `=` means same as Far.
Several keys are comma separated. Key syntax: ADR-0005. Presets are generated from this table:
`crates/ofm-config/defaults/keymap-{far,tc,mc}.toml`.

Note: Ctrl+X is Cut (not command-history-next, as in Far); history-next is Ctrl+Shift+E.

| ID | Context | Title | Far (default) | TC | mc |
|---|---|---|---|---|---|
| `app.quit` | global | Quit | `F10` | `Alt+F4` | = |
| `app.menu` | global | Menu bar | `F9` | `F9`, `F10` | = |
| `app.help` | global | Help | `F1` | = | = |
| `app.preferences` | global | Preferences | `Ctrl+,` | = | = |
| `app.jobs` | global | Jobs view | `Ctrl+J` | = | = |
| `app.terminal_toggle` | global | Show/hide panels (full terminal) | `Ctrl+O` | = | = |
| `app.theme_next` | global | Next theme | `Ctrl+Alt+T` | = | = |
| `app.reload_config` | global | Reload configuration | `Ctrl+Alt+R` | = | = |
| `window.fullscreen` | global | Toggle fullscreen | `F11` | = | = |
| `panel.cursor_up` | panels | Cursor up | `Up` | = | = |
| `panel.cursor_down` | panels | Cursor down | `Down` | = | = |
| `panel.cursor_left` | panels | Cursor left (previous column) | `Left` | = | = |
| `panel.cursor_right` | panels | Cursor right (next column) | `Right` | = | = |
| `panel.page_up` | panels | Page up | `PgUp` | = | = |
| `panel.page_down` | panels | Page down | `PgDn` | = | = |
| `panel.home` | panels | First entry | `Home` | = | = |
| `panel.end` | panels | Last entry | `End` | = | = |
| `panel.switch` | panels | Switch active panel | `Tab` | = | = |
| `panel.toggle_other` | panels | Show/hide other panel | `Ctrl+P` | = | = |
| `panel.swap` | panels | Swap panels | `Ctrl+U` | = | = |
| `panel.same_dir` | panels | Other panel to this directory | `Alt+=` | = | `Alt+O`, `Alt+=` |
| `panel.refresh` | panels | Refresh | `Ctrl+R` | = | = |
| `panel.go_up` | panels | Go to parent directory | `Backspace`, `Ctrl+PgUp` | = | = |
| `panel.go_root` | panels | Go to root directory | `Ctrl+\` | = | `Ctrl+Shift+\` |
| `panel.go_home` | panels | Go to home directory | `Ctrl+Home` | = | = |
| `panel.goto_path` | panels | Go to path... | `Ctrl+L` | = | = |
| `panel.open` | panels | Open / enter / run | `Enter` | = | = |
| `panel.enter_dir` | panels | Enter directory or archive (never run) | `Ctrl+PgDn` | = | = |
| `panel.history_back` | panels | History back | `Alt+Left` | = | = |
| `panel.history_forward` | panels | History forward | `Alt+Right` | = | = |
| `panel.history_list` | panels | Directory history list | `Alt+F12` | = | `Alt+H`, `Alt+F12` |
| `panel.hotlist` | panels | Hotlist / bookmarks | `Ctrl+D` | = | `Ctrl+\`, `Ctrl+D` |
| `panel.hotlist_add` | panels | Add directory to hotlist | `Ctrl+Shift+D` | = | = |
| `panel.drive_menu_left` | panels | Drive/mount menu (left panel) | `Alt+F1` | = | = |
| `panel.drive_menu_right` | panels | Drive/mount menu (right panel) | `Alt+F2` | = | = |
| `panel.view_brief` | panels | Brief view | `Ctrl+1` | = | = |
| `panel.view_full` | panels | Full view | `Ctrl+2` | = | = |
| `panel.view_custom` | panels | Custom columns view | `Ctrl+3` | = | = |
| `panel.sort_name` | panels | Sort by name (again = reverse) | `Ctrl+F3` | = | = |
| `panel.sort_ext` | panels | Sort by extension (again = reverse) | `Ctrl+F4` | = | = |
| `panel.sort_mtime` | panels | Sort by modified time (again = reverse) | `Ctrl+F5` | = | = |
| `panel.sort_size` | panels | Sort by size (again = reverse) | `Ctrl+F6` | = | = |
| `panel.sort_reverse` | panels | Reverse sort order | `Ctrl+F12` | = | = |
| `panel.toggle_hidden` | panels | Show/hide hidden files | `Ctrl+H` | = | = |
| `panel.toggle_quickview` | panels | Quick view in other panel | `Ctrl+Q` | = | = |
| `panel.context_menu` | panels | Context menu | `Shift+F10`, `Menu` | = | = |
| `panel.compare_dirs` | panels | Compare directories | `Shift+F2` | = | = |
| `panel.quicksearch` | panels | Quick search | `Ctrl+S` | = | = |
| `panel.quicksearch_next` | panels | Quick search: next match | `Ctrl+Down` | = | = |
| `panel.quicksearch_prev` | panels | Quick search: previous match | `Ctrl+Up` | = | = |
| `panel.filter` | panels | Filter as you type | `Ctrl+Shift+S` | = | = |
| `panel.cancel` | panels | Cancel quick search / filter | `Esc` | = | = |
| `tab.new` | panels | New tab | `Ctrl+T` | = | = |
| `tab.close` | panels | Close tab | `Ctrl+W` | = | = |
| `tab.next` | panels | Next tab | `Ctrl+Tab` | = | = |
| `tab.prev` | panels | Previous tab | `Ctrl+Shift+Tab` | = | = |
| `tab.lock` | panels | Lock/unlock tab | `Ctrl+Shift+L` | = | = |
| `select.toggle` | panels | Toggle selection and move down | `Ins` | `Ins`, `Space` | = |
| `select.extend_up` | panels | Extend selection up | `Shift+Up` | = | = |
| `select.extend_down` | panels | Extend selection down | `Shift+Down` | = | = |
| `select.extend_page_up` | panels | Extend selection page up | `Shift+PgUp` | = | = |
| `select.extend_page_down` | panels | Extend selection page down | `Shift+PgDn` | = | = |
| `select.extend_home` | panels | Extend selection to first | `Shift+Home` | = | = |
| `select.extend_end` | panels | Extend selection to last | `Shift+End` | = | = |
| `select.by_mask` | panels | Select by mask | `Gray+` | = | = |
| `select.unmask` | panels | Deselect by mask | `Gray-` | = | = |
| `select.invert` | panels | Invert selection | `Gray*` | = | = |
| `select.all` | panels | Select all | `Ctrl+Gray+` | `Ctrl+A`, `Ctrl+Gray+` | = |
| `select.none` | panels | Deselect all | `Ctrl+Gray-` | = | = |
| `select.restore` | panels | Restore previous selection | `Ctrl+M` | = | = |
| `file.view` | panels | View (directory: calculate size) | `F3` | = | = |
| `file.edit` | panels | Edit | `F4` | = | = |
| `file.new_file` | panels | New file (edit) | `Shift+F4` | = | = |
| `file.copy` | panels | Copy | `F5` | = | = |
| `file.move` | panels | Move | `F6` | = | = |
| `file.rename` | panels | Rename in place | `Shift+F6` | = | = |
| `file.mkdir` | panels | Make directory | `F7` | = | = |
| `file.delete` | panels | Delete (to Trash) | `F8`, `Del` | = | = |
| `file.delete_permanent` | panels | Delete permanently | `Shift+Del`, `Shift+F8` | = | = |
| `file.link` | panels | Create link | `Alt+F6` | = | = |
| `file.properties` | panels | Properties / attributes | `Alt+Enter` | = | = |
| `file.calc_size` | panels | Calculate directory size | `Alt+Shift+Enter` | = | = |
| `file.batch_rename` | panels | Batch rename in editor | `Ctrl+Shift+M` | = | = |
| `file.batch_rename_undo` | panels | Undo last batch rename | `Ctrl+Alt+Z` | = | = |
| `file.clipboard_copy` | panels | Copy files to clipboard | `Ctrl+C` | = | = |
| `file.clipboard_cut` | panels | Cut files to clipboard | `Ctrl+X` | = | = |
| `file.clipboard_paste` | panels | Paste files from clipboard | `Ctrl+V` | = | = |
| `file.copy_path` | panels | Copy full path to clipboard | `Ctrl+Shift+C` | = | = |
| `file.open_with` | panels | Open with... | `Shift+Enter` | = | = |
| `menu.user` | panels | User menu | `F2` | = | = |
| `menu.edit` | panels | Edit user menu | `Ctrl+Alt+M` | = | = |
| `menu.edit_local` | panels | Edit local user menu (this folder) | `Ctrl+Alt+Shift+M` | = | = |
| `search.find` | panels | Find files | `Alt+F7` | = | = |
| `search.panelize_command` | panels | Panelize command output | `Ctrl+Alt+P` | = | = |
| `cmdline.history_prev` | panels | Command history: previous | `Ctrl+E` | = | = |
| `cmdline.history_next` | panels | Command history: next | `Ctrl+Shift+E` | = | = |
| `cmdline.history_list` | panels | Command history list | `Alt+F8` | = | = |
| `cmdline.insert_name` | panels | Insert file name | `Ctrl+Enter` | = | = |
| `cmdline.insert_fullpath` | panels | Insert full path | `Ctrl+F` | = | = |
| `cmdline.insert_left_path` | panels | Insert left panel path | `Ctrl+[` | = | = |
| `cmdline.insert_right_path` | panels | Insert right panel path | `Ctrl+]` | = | = |
| `cmdline.execute` | cmdline | Execute command line | `Enter` | = | = |
| `cmdline.clear` | cmdline | Clear command line | `Esc` | = | = |
| `cmdline.complete` | cmdline | Complete path | `Ctrl+Space`, `Tab` | = | = |
| `viewer.close` | viewer | Close viewer | `Esc`, `F10` | = | = |
| `viewer.toggle_hex` | viewer | Toggle hex mode | `F4` | = | = |
| `viewer.encoding` | viewer | Choose encoding | `F8` | = | = |
| `viewer.search` | viewer | Search | `F7` | = | = |
| `viewer.search_next` | viewer | Search next | `Shift+F7` | = | = |
| `viewer.search_prev` | viewer | Search previous | `Alt+F7` | = | = |
| `viewer.wrap` | viewer | Toggle line wrap | `F2` | = | = |
| `viewer.goto` | viewer | Go to line / offset | `Alt+F8` | = | = |
| `viewer.line_up` | viewer | Scroll up one line | `Up` | = | = |
| `viewer.line_down` | viewer | Scroll down one line | `Down` | = | = |
| `viewer.page_up` | viewer | Page up | `PgUp` | = | = |
| `viewer.page_down` | viewer | Page down | `PgDn`, `Space` | = | = |
| `viewer.top` | viewer | Go to start | `Home` | = | = |
| `viewer.bottom` | viewer | Go to end | `End` | = | = |
| `viewer.scroll_left` | viewer | Scroll left | `Left` | = | = |
| `viewer.scroll_right` | viewer | Scroll right | `Right` | = | = |
| `viewer.next_file` | viewer | Next file | `Gray+` | = | = |
| `viewer.prev_file` | viewer | Previous file | `Gray-` | = | = |
| `dialog.ok` | dialog | Confirm | `Enter` | = | = |
| `dialog.cancel` | dialog | Cancel | `Esc` | = | = |
| `dialog.queue` | dialog | Add to queue (instead of running in parallel) | `F2` | = | = |
| `dialog.apply_all` | dialog | Apply decision to all | `Ctrl+A` | = | = |
| `search.panelize_results` | dialog | Panelize search results | `F10` | = | = |
| `jobs.pause_resume` | dialog | Pause / resume job | `Space` | = | = |
| `jobs.cancel` | dialog | Cancel job | `Del` | = | = |
| `jobs.clear_finished` | dialog | Clear finished jobs | `Ctrl+L` | = | = |
