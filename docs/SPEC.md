# OFM v1.0 — Product Specification

Status: approved for implementation. Source: MVP feature spec (research of Far, Double Commander,
Total Commander, Midnight Commander, Krusader, user discussions), 2026-11-14.

v1.0 is a keyboard-first, two-panel GTK 4 file manager with a built-in command line and terminal,
a background job queue, F3 viewer, search with results in a panel, read-only archive browsing,
a user menu, and two signature themes: **Far Blue** and **Commander Light** (white/orange).

## Design rules

1. Never block the UI. 2. Never lose data. 3. Every action reachable from the keyboard and rebindable.
4. Far key preset by default; Total/Double Commander and mc presets selectable.
5. Fit into GNOME (portals, trash, clipboard, drag & drop) without looking like a GNOME Files clone.

## Features (F01–F33)

Default keys = Far preset. "Done when" = acceptance criteria, verified by tests where possible and by
Igor at the gates.

| ID | Area | Feature | Default keys | Done when |
|---|---|---|---|---|
| F01 | Panels | Two panels, active one highlighted, resizable split, one-panel mode (hide other) | Tab, Ctrl+P | Split ratio and layout survive restart |
| F02 | Panels | View modes: Brief (multi-column names), Full (name/size/date/permissions), Custom (configurable columns) | Ctrl+1 / Ctrl+2 / Ctrl+3 | 100k-entry directory scrolls smoothly; no visible stutter |
| F03 | Panels | Sort by name/extension/size/modified time; directories first; natural sort (file2 < file10); reverse | Ctrl+F3 name, Ctrl+F4 extension, Ctrl+F5 modified, Ctrl+F6 size; same key again = reverse | Sort indicator in header; sort survives restart per panel |
| F04 | Panels | Status line: current file info, selected count + total size, free space of the panel's filesystem | — | Updates live while selecting |
| F05 | Panels | Tabs per panel; locked tabs (can't navigate away) | Ctrl+T, Ctrl+W, Ctrl+Tab / Ctrl+Shift+Tab | Tabs restored per session |
| F06 | Navigation | Quick search: typing jumps to first match (prefix, then substring); optional filter-as-you-type mode | letters (Far: Alt+letters, both supported) | No lag in 100k-entry dirs; Esc cancels |
| F07 | Navigation | Directory history (per panel, back/forward) and hotlist/bookmarks with editor | Alt+F12, Ctrl+D, Alt+Left/Right | Stored in config; editable in a dialog |
| F08 | Navigation | Drive/mount menu: Home, /, mounted volumes, removable drives, GVfs mounts (SMB/SFTP mounted by GNOME), Trash, bookmarks | Alt+F1 (left), Alt+F2 (right) | Shows GVfs mounts made in GNOME Files; mounting a listed unmounted volume works |
| F09 | Navigation | Go up, go to root, swap panels, other panel → same dir, refresh | Backspace / Ctrl+PgUp, Ctrl+\\, Ctrl+U, Alt+=, Ctrl+R | Cursor lands on the directory we came from when going up |
| F10 | Selection | Toggle selection, range with Shift, select/deselect by mask, invert, restore last selection | Ins / Space, Shift+arrows, Gray+ / Gray−, Gray*, Ctrl+M (restore) | Masks like `*.{jpg,png}`, `!*.tmp`; mask history |
| F11 | Command line | Command line below panels; executes in the active panel's dir via the user's $SHELL; history; completion of paths | Ctrl+E / Ctrl+X history, Ctrl+Enter insert name, Ctrl+F insert full path, Ctrl+[ / Ctrl+] insert left/right panel path | `cd x` typed there changes the panel's directory |
| F12 | Command line | Embedded terminal (VTE) runs one persistent shell; command output visible; toggle panels on/off to see terminal full size | Ctrl+O | Shell cwd and panel dir stay in sync (OSC 7); Tab completion works in terminal view |
| F13 | File ops | Copy, move, rename in place, make dir, delete | F5, F6, Shift+F6, F7, F8 | Target path editable in dialog; rename masks (`*.txt` → `*.bak`); copying to same dir asks for new name |
| F14 | File ops | Delete moves to Trash (freedesktop spec) by default; permanent delete separately | F8, Shift+Del | Deleted items appear in GNOME Files trash and can be restored there |
| F15 | File ops | Background job queue with progress (bytes, files, speed, ETA), pause/resume/cancel, queue vs run-parallel | Ctrl+J jobs view | UI never blocks; quitting with running jobs asks |
| F16 | File ops | Conflict dialog: overwrite / skip / rename new / rename existing / overwrite if newer / compare; "apply to all" | — | Decision remembered for the rest of the job |
| F17 | File ops | Correct copies: permissions, timestamps (incl. ns), symlinks (copy as link by default), xattrs, ownership when root; reflink (FICLONE) / copy_file_range fast paths; sparse files; no forced flags that defeat Btrfs compression | — | Test suite: metadata preserved, byte-equal content, zero data-loss cases |
| F18 | File ops | Create symlink (absolute or relative) / hardlink | Alt+F6 | — |
| F19 | File ops | Properties: chmod (octal + checkboxes), chown/chgrp (if permitted), directory size calculation | Alt+Enter; F3 on directory = calculate size | Size calculation async and cancellable; recursive chmod optional |
| F20 | File ops | Batch rename via editor: selected names open in the configured editor as text, saving applies renames | Ctrl+Shift+M | Preview of changes before apply; conflicts/cycles detected; undo of last batch |
| F21 | View | F3 viewer: text with encoding switch (UTF-8, UTF-16, CP1251, CP866, KOI8-R, Latin-1), wrap/unwrap, hex mode, search (text/hex/regex), go to line/offset | F3; in viewer: F4 hex, F8 encoding, F7 search, Shift+F7 next, Esc close | Opens a 10 GB file instantly (mmap, lazy line index) |
| F22 | View | Quick view: the other panel shows a preview of the file under the cursor (text, image, directory summary) | Ctrl+Q | Follows the cursor with ≤ 100 ms debounce, never blocks |
| F23 | Edit | F4 opens the file in an external editor ($VISUAL / $EDITOR, else GNOME Text Editor), configurable per mask | F4, Shift+F4 = new file | Terminal editors (vim, nano, mcedit) run inside the embedded terminal |
| F24 | Search | Find files: name masks, content (text or regex, case option, encoding), size and date filters, follow symlinks option, search inside archives option | Alt+F7 | Results stream in while searching; cancellable; F3/F4 on results |
| F25 | Search | Panelize: send search results or output of a shell command to a panel; all file ops work there | F10 in results; "Panelize command" in menu | F5/F6/F8 on panelized items operate on the real files |
| F26 | Archives | Enter zip, tar, tar.gz, tar.bz2, tar.xz, tar.zst, 7z as folders (read-only); copy out = extract | Enter / Ctrl+PgDn, F5 | Password prompt for encrypted zip/7z; corrupt archives show an error, never crash |
| F27 | Compare | Compare directories: mark files that are missing or different on the other side | Shift+F2 (Far: menu) | Modes: name+size+mtime (default), content (async) |
| F28 | Customization | User menu: list of named commands with placeholders, global and per-directory (`.ofm-menu.toml`) | F2 | Placeholders: `%f` file, `%s` selection, `%d` dir, `%D` other dir, `%F` other file, `%n` name w/o ext, `%{input:Prompt}` |
| F29 | Customization | Keymap in TOML; presets: Far (default), Total/Double Commander, Midnight Commander | — | Every command has an ID and can be rebound; conflicts reported on load |
| F30 | Customization | Themes + file highlighting rules (colour/icon by mask or attribute) | — | Far Blue, Commander Light, Adwaita ship; switching is instant; live reload of theme files |
| F31 | Desktop | Enter opens with default app (GTK FileLauncher / portal), executable scripts run in terminal, drag & drop with other apps, file clipboard compatible with GNOME Files | Enter, Ctrl+C / Ctrl+X / Ctrl+V | Copy in GNOME Files → paste in OFM works and vice versa |
| F32 | Desktop | F-key bar (clickable, shows modifier variants while Shift/Ctrl/Alt held), F9 menu bar, full mouse support (click, double-click, right-click menu, Shift/Ctrl click selection) | F9 | All functions reachable without mouse; all common ones without keyboard |
| F33 | Desktop | Session restore: panels, tabs, paths, cursor positions, view modes, window size | — | Restart returns to the same state |

## Themes (F30)

Three built-in themes, all in the same TOML format (`ofm-config` defines the schema):

| Role | Far Blue | Commander Light |
|---|---|---|
| Panel background | `#000080` | `#FFFFFF`, alternate rows `#F7F7F7` |
| Regular file text | `#00FFFF` | `#1E1E1E` |
| Directory text | `#FFFFFF` bold | `#000000` bold, folder icon |
| Cursor bar (active panel) | `#008080` background, black text | `#F28C28` background, white text |
| Cursor (inactive panel) | hidden or outline | `#F28C28` outline |
| Marked files | `#FFFF00` | `#C2410C` bold |
| Panel frame / header | cyan double line; path black on `#008080` | `#E5E5E5` hairline; header `#F2F2F2` |
| Command line | light grey on black | black on `#FAFAFA` |
| F-key bar | white digit, black label on `#008080` | grey digit, black label, orange hover |
| Font | monospace (configurable) | system UI font |

Far Blue values approximate the classic 16-colour console palette; tune by eye against Far/far2l
screenshots at gate G1. **Adwaita** follows the GNOME light/dark preference and accent colour.

Theme format requirements: semantic roles (above + viewer, dialogs, progress, errors, highlighting);
highlighting rules evaluated in order, first match wins (hidden, executable, symlink, broken symlink,
archive, image, source code, by glob); retro options (monospace grid, box-drawing frames, no rounded
corners, no animations); honour GNOME high-contrast and text scaling.

## Quality bar for 1.0

| Metric | Target |
|---|---|
| First paint of a 100k-entry local directory | < 1 s (entries stream in; cursor usable immediately) |
| Cold start to interactive window | < 300 ms on a recent laptop SSD (release build) |
| Main-thread file I/O | none (enforced by review + `tracing` span check in debug builds) |
| Copy engine | zero data-loss cases in the torture suite; byte-compare after copy in tests |
| Memory | < 150 MB RSS with two 100k-entry panels open |
| Crashes | no panics reachable from user input; archive/viewer parsers fuzzed for 10 min each without crash |

## Out of scope for v1.0 (planned later)

| Feature | Target |
|---|---|
| Full multi-rename tool (counters, regex, case, EXIF/date placeholders) | v1.1 |
| Directory synchronization (one/two-way, profiles) | v1.1 |
| Built-in editor (GtkSourceView) | v1.1 |
| Native SFTP VFS | v1.1 |
| Create / update archives | v1.1 |
| Checksums (create/verify) | v1.1 |
| FTP/FTPS, WebDAV, SMB, S3, cloud VFS | v1.2 |
| Branch view, duplicate finder, disk usage map, split/combine | v1.2 |
| Root mode / polkit elevation, thumbnails view | v1.2 |
| Plugins (WASM), Lua macros, key recording, TC plugin shim | v2.0 |
| Windows/macOS/TUI frontends | v2.0 |

The architecture must not make any of these harder (see ARCHITECTURE.md "Extension points"),
but no code for them is written in v1.0.
