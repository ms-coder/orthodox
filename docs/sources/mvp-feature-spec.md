# Orthodox File Manager — MVP Feature Spec

Nov 14, 2026 · @Igor

## Summary

v1.0 is a keyboard-first, two-panel GTK 4 file manager with a built-in command line, a background copy queue, F3 viewer, file search with results in a panel, read-only archive browsing, a user menu, and two shipped themes: **Far Blue** and **Commander Light** (white/orange).

The MVP takes the core loop every orthodox manager shares (panels + F-keys + command line, from Norton Commander via mc and Far) and adds three things users repeatedly praise in the bigger tools: tabs (Total/Double Commander), panelized search results (mc), and a user menu (mc/Far). Heavier power tools like the full multi-rename, directory sync, the built-in editor, native remote file systems and plugins come in v1.1 and later, on top of the same core.

**Design rules for v1.0:** never block the UI, never lose data, every action reachable from the keyboard, Far/TC key presets out of the box.

## Landscape: what users praise and complain about

Users are loyal to the keyboard-and-panels workflow and to the power tools (rename, sync, VFS). Their complaints are about dated looks, stability, settings that are hard to change, and missing modern copy features. That gap is ours to fill.

| Tool | Platform / UI | What users praise | What users complain about |
| --- | --- | --- | --- |
| [Norton Commander](https://fman.io/blog/dual-pane-file-manager-history/) | DOS, text | Set the template: two panels, F5 copy, F6 move; fast because the design fit the hardware | Historic only |
| [Midnight Commander](https://hn.svelte.dev/item/45271481) | Linux, terminal | Ctrl+O to the shell and back, Ctrl+X T to put selected names on the command line, + / - / \* selection, VFS (FTP, SFTP, archives), panelize, F2 user menu, swappable keymaps, works over SSH | Tab blocks shell completion; colours and formats are painful to change; dated blue look; F10 clashes with GNOME Terminal; copy features feel old |
| [Far Manager](https://en.wikipedia.org/wiki/Far_Manager) / [far2l](https://www.itechguides.com/compare/far2l-vs-rtfm/) | Windows; far2l on Linux/macOS, TTY or GUI | Panels + command prompt combined; viewer, editor, user menus, compare; Lua macros and a huge plugin ecosystem; far2l reaches SFTP, SMB, NFS, WebDAV, S3 | Windows-only original; far2l is a port of the old v2 codebase; plugins are native binaries tied to one architecture |
| [Total Commander](https://www.ghisler.com/featurel.htm) | Windows (paid) | Multi-rename tool (incl. editing names in a text editor), directory sync, compare by content, search inside archives, custom columns, branch view, operation queue | Not on Linux, not open source; UI looks old; scripting is limited |
| [Double Commander](https://doublecmd.sourceforge.io/) | Linux/Win/macOS, Lazarus (GTK/Qt) | Free TC clone; background operations, tabs, multi-rename, custom columns, F3/F4 viewer/editor, TC plugin support; 1.2 added GTK 3, high-DPI, cloud, checksums ([news](https://alternativeto.net/news/2026/7/double-commander-1-2-brings-support-for-high-dpi-cloud-providers-jpeg-xl-gtk-3-and-more/)) | Looks "like the 90's"; high-DPI called a hack; past crashes and freezes; file list called "grotesque"; weak accessibility ([forum](https://ghisler.ch/board/viewtopic.php?p=345921)); copying disables Btrfs compression |
| [Krusader](https://www.linuxlinks.com/krusader/) | Linux, KDE/Qt | Remote via KIO (SFTP, SMB, WebDAV), sync dirs, disk usage, mount manager, root mode, user actions | Tied to KDE; some find it cluttered |
| Newcomers ([Carelo](https://www.linuxlinks.com/carelo-dual-pane-file-manager-linux/), Yazi, COSMIC Files) | Rust; Tauri / TUI / iced | Modern look, async ops with progress and cancel, previews, folder compare | Thin on power features; Carelo is webview-based |

## Most wanted features, ranked

The ranking reflects how often a feature comes up as a reason people use, or miss, a tool across the sources below. It is a judgement from the research, not a survey.

| Rank | Feature | Loved in | In v1.0? |
| --- | --- | --- | --- |
| 1 | Two panels + F-key operations (F3–F8), Tab to switch | All | Yes |
| 2 | Keyboard-first: quick search, Ins/+/-/\* selection, configurable keymap | NC, mc, Far, TC | Yes |
| 3 | Command line under the panels + Ctrl+O to the shell output | mc, Far | Yes |
| 4 | Background operations with a queue, progress, pause and cancel | DC, TC, Yazi | Yes |
| 5 | Multi-rename / batch rename | TC, DC, Krusader | Basic (edit names in editor); full tool in v1.1 |
| 6 | Archives as folders (browse, extract, search inside) | Far, mc, TC, Krusader | Read-only browse + extract |
| 7 | Remote file systems (SFTP, SMB, WebDAV, FTP, S3) | far2l, mc, Krusader | GNOME-mounted locations only; native SFTP in v1.1 |
| 8 | Tabs per panel | TC, DC, Krusader | Yes |
| 9 | Find files by name/content, results as a panel (panelize) | mc, TC | Yes |
| 10 | Directory compare / sync | TC, DC, Krusader | Compare only; sync in v1.1 |
| 11 | User menu (F2) / user actions with placeholders | mc, Far, Krusader | Yes |
| 12 | Built-in viewer (text/hex) and quick view panel | All | Yes |
| 13 | Built-in editor with syntax highlighting | mc, Far, DC | No: external editor in v1.0, built-in in v1.1 |
| 14 | Themes and colour by file type | mc skins, Far highlighting | Yes (2 themes + highlighting rules) |
| 15 | Plugins, macros, scripting | Far, TC, DC | v2.0 |
| 16 | Checksums, duplicates, disk usage, split/combine | TC, DC 1.2, Krusader | v1.2 |

## MVP scope (v1.0)

33 features in 8 areas. Default keys follow Far. A Total Commander / Double Commander preset ships alongside it.

| # | Area | Feature | Default keys | Done when |
| --- | --- | --- | --- | --- |
| 1 | Panels | Two panels, active one highlighted, resizable split, one-panel mode | Tab, Ctrl+P (hide other) | Split ratio and layout survive restart |
| 2 | Panels | View modes: Brief (multi-column names), Full (name/size/date/perm), one custom column set | Ctrl+1/2/3 | 100k-entry directory scrolls smoothly at 60 fps |
| 3 | Panels | Sort by name/ext/size/date, dirs first, natural sort | Ctrl+F3…F6 | Sort indicator shown in panel header |
| 4 | Panels | Status line: current file, selected count + total size, free space | — | Updates live while selecting |
| 5 | Panels | Tabs per panel, with lock | Ctrl+T, Ctrl+W, Ctrl+Tab | Tabs restored per session |
| 6 | Navigation | Quick search: type to jump, optional filter-as-you-type | letters / Alt+letters | Works in 100k dirs with no lag |
| 7 | Navigation | Directory history and hotlist/bookmarks | Alt+F12, Ctrl+D | Stored in config, editable |
| 8 | Navigation | Mount/drive menu: home, root, mounts, removable drives, GNOME (GVfs) mounted SMB/SFTP, trash | Alt+F1 / Alt+F2 | Lists GVfs mounts that GNOME Files mounted |
| 9 | Navigation | Go up, root, swap panels, other panel to same dir | Backspace, Ctrl+, Ctrl+U, Alt+= | — |
| 10 | Selection | Ins/Space toggle, Shift+arrows, select/deselect by mask, invert | Ins, Gray +, Gray −, Gray \* | Masks support `*.{jpg,png}` and history |
| 11 | Command line | Command line under the panels, runs in the active panel's dir with $SHELL; history; completion | Ctrl+E/X (history), Ctrl+Enter (insert name), Ctrl+F (insert path) | `cd` typed there changes the panel |
| 12 | Command line | Embedded terminal (VTE) showing command output; toggle panels on/off | Ctrl+O | Tab completion works in terminal view (fixes mc's complaint) |
| 13 | File ops | Copy / move / rename / mkdir / delete | F5, F6, Shift+F6, F7, F8 | Target path editable, rename masks (`*.txt` → `*.bak`) |
| 14 | File ops | Delete to trash by default (freedesktop spec), permanent delete | F8, Shift+Del | Items restorable from GNOME Files trash |
| 15 | File ops | Background job queue: progress, speed, ETA, pause/resume/cancel, queue vs parallel | Ctrl+J (jobs) | UI never blocks; closing the window asks about running jobs |
| 16 | File ops | Conflict dialog: overwrite / skip / rename / newer only / compare, “for all” | — | Remembered per job |
| 17 | File ops | Correct copies: permissions, timestamps, symlinks, xattrs; reflink/copy\_file\_range; sparse files; keep Btrfs compression | — | Test suite: zero data-loss cases, byte-compare after copy |
| 18 | File ops | Create symlink / hardlink | Alt+F6 | — |
| 19 | File ops | Properties: chmod / chown, size of selected dirs | Alt+Enter, F3 on dir | Size calc is async and cancellable |
| 20 | File ops | Batch rename via editor: names of selected files open in an editor, save = rename | Ctrl+M | Dry-run preview, undo of last batch |
| 21 | View/edit | F3 viewer: text with encodings (UTF-8, CP1251, CP866, KOI8-R), hex mode, wrap, search; mmap for large files | F3, F4 inside viewer = hex | Opens a 10 GB log instantly |
| 22 | View/edit | Quick view panel (other panel shows preview: text, image, dir info) | Ctrl+Q | Follows the cursor |
| 23 | View/edit | F4 opens external editor ($EDITOR or GNOME Text Editor), configurable per mask | F4 | — |
| 24 | Search | Find files: name masks, content (text/regex), size/date filters, inside archives (optional) | Alt+F7 | Results stream in while searching |
| 25 | Search | Panelize: send search results (or any command output) to a panel and work on them | F10 in results | F5/F6/F8 work on panelized items |
| 26 | Archives | Enter zip / tar / tar.gz / tar.xz / tar.zst / 7z as folders, read-only; F5 extracts | Enter, F5 | Encrypted zip asks for password |
| 27 | Compare | Compare directories: mark files that differ or are missing on the other side | Shift+F2 (or menu) | By name+size+date; by content optional |
| 28 | Customization | User menu with placeholders (%f file, %s selection, %d dir, %D other dir) | F2 | Example entries: tar selection, rsync to other panel |
| 29 | Customization | Keymap in TOML + presets: Far, Total/Double Commander, mc | — | Every action has an id and can be rebound |
| 30 | Customization | Themes + file highlighting rules (colour by mask/attributes: dirs, executables, archives, hidden, symlinks) | — | Far Blue and Commander Light ship by default |
| 31 | Desktop | Enter opens with the default app (portal), drag & drop with other apps, file clipboard compatible with GNOME Files | Enter, Ctrl+C/X/V | Copy in Nautilus, paste in panel works |
| 32 | Desktop | F-key bar (clickable), F9 menu bar, full mouse support | F9 | All functions reachable without mouse and without keyboard |
| 33 | Desktop | Session restore: panels, tabs, paths, cursor positions | — | Restart returns to the same state |

**Quality bar for 1.0:** first paint of a 100k-file directory under 1 s, cold start under 300 ms, no main-thread file I/O, the copy engine covered by tests against a temp dir before release.

## Theming

v1.0 ships three themes: **Far Blue** (retro console look), **Commander Light** (white with an orange cursor), and **Adwaita**, which follows the GNOME light/dark setting. All three use one semantic theme format, so new themes need no code.

| Role | Far Blue | Commander Light |
| --- | --- | --- |
| Panel background | `#000080` navy | `#FFFFFF` white, alternate rows `#F7F7F7` |
| Regular file text | `#00FFFF` cyan | `#1E1E1E` near-black |
| Directory text | `#FFFFFF` white, bold | `#000000` black, bold, folder icon |
| Cursor bar (active panel) | `#008080` teal, black text | `#F28C28` orange, white text |
| Cursor (inactive panel) | hidden or outline only | `#F28C28` outline only |
| Marked (selected) files | `#FFFF00` yellow | `#C2410C` dark orange-red, bold |
| Panel frame and header | cyan double line; path in black-on-teal | `#E5E5E5` hairline; header `#F2F2F2` |
| Command line | light grey on black | black on `#FAFAFA` |
| F-key bar | white number, black label on teal | grey number, black label, orange on hover |
| Font | monospace (e.g. JetBrains Mono, or a pixel font for full retro) | system UI font |

Far Blue colours are the classic 16-colour console palette, approximately. They should be tuned by eye against Far/far2l screenshots.

**Theme format requirements**

- TOML file with semantic roles (the rows above + viewer, dialogs, progress, error), not widget CSS.
- Highlighting rules: colour + optional icon per mask or attribute (hidden, executable, symlink, broken link, archive, image, source code), first match wins, like Far's highlighting groups.
- Live reload when the file changes, so editing a theme is instant.
- Retro options per theme: monospace grid, box-drawing frames, no rounded corners, no animations.
- High contrast and font scaling are honoured (a Double Commander user complained about missing accessibility).

## Explicitly out of v1.0

These features are wanted, but each is big or risky enough to slip the first stable release. All of them plug into the core without redesign.

| Feature | Why not in v1.0 | Target |
| --- | --- | --- |
| Full multi-rename tool (counters, regex, case, date/EXIF placeholders, preview) | Large UI; the editor-based rename covers the basics | v1.1 |
| Directory synchronization (one-way / two-way, profiles) | Data-loss risk; needs its own test matrix | v1.1 |
| Built-in editor (GtkSourceView) | External editor is good enough at first | v1.1 |
| Native SFTP panel (russh), key auth, agent | GVfs mounts cover it for now | v1.1 |
| Create / update archives, encrypted 7z/zip | Read-only browse first | v1.1 |
| Checksums (create/verify, XXH3, BLAKE3, SHA-256) | Nice to have | v1.1 |
| FTP/FTPS, WebDAV, SMB, S3, cloud drives as VFS | Each backend needs auth UI and edge cases | v1.2 |
| Branch view (flat view of all files in subdirs) | Needs panel model changes | v1.2 |
| Duplicate finder, disk usage map, split/combine | Separate tools | v1.2 |
| Root mode / elevated operations (polkit) | Security review needed | v1.2 |
| Thumbnails view | Not core to the keyboard workflow | v1.2 |
| Plugins (WASM components), Lua macros, key recording | API must be stable first | v2.0 |
| Total Commander WCX/WFX plugin shim | Native-code ABI; only if users ask | v2.0 |
| Windows / macOS frontends, TUI frontend | Core-first architecture makes them additive | v2.0 |

## Roadmap after the MVP

&#91;embedded content: release roadmap · 4 phases, 3 gates\]

Each release gets through its gate only when the named test bar is met. The core API freezes before plugins and the Windows/macOS frontends, so those build on a stable base. There are no dates yet; phases are ordered, not to scale.

## Sources

- [Hacker News thread on Norton/Midnight Commander (mirror)](https://hn.svelte.dev/item/45271481): user praise and complaints about mc
- [Double Commander vs Total Commander, ghisler.ch forum](https://ghisler.ch/board/viewtopic.php?p=345921)
- [Double Commander 1.2 release news, AlternativeTo](https://alternativeto.net/news/2026/7/double-commander-1-2-brings-support-for-high-dpi-cloud-providers-jpeg-xl-gtk-3-and-more/)
- [Double Commander homepage](https://doublecmd.sourceforge.io/)
- [Double Commander theme complaint, Universal Blue forum](https://universal-blue.discourse.group/t/howto-install-double-commander-in-distrobox-container-and-auto-upgrade/8709/2.md)
- [Double Commander and Btrfs compression, linux.org](https://www.linux.org/threads/double-commander-not-doing-file-compression-in-btrfs-partitions.68421/latest)
- [Far Manager, Wikipedia](https://en.wikipedia.org/wiki/Far_Manager)
- [far2l review, itechguides](https://www.itechguides.com/compare/far2l-vs-rtfm/)
- [Total Commander feature list](https://www.ghisler.com/featurel.htm)
- [Midnight Commander, LinuxLinks](https://www.linuxlinks.com/midnightcommander/)
- [Krusader, LinuxLinks](https://www.linuxlinks.com/krusader/)
- [Carelo, LinuxLinks](https://www.linuxlinks.com/carelo-dual-pane-file-manager-linux/)
- [Dual pane file manager history, fman](https://fman.io/blog/dual-pane-file-manager-history/)
- [r/linux thread on Total Commander bulk rename](https://axebps-redlib.hf.space/r/linux/comments/17p1ufs/what_is_a_piece_of_software_that_linux/k82ef44)
