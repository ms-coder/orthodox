# G3 walkthrough checklist — please try OFM for a day of normal use

One line per spec feature (F01–F33). Keys = Far preset (the default). Tick or jot notes
next to each line; report anything odd per "How to report" at the bottom.

## Setup

1. Build & run: `cargo run -p ofm-app` (release: `cargo run -p ofm-app --release`).
   Start it once as `cargo run -p ofm-app -- ~/ofm-g3/a ~/ofm-g3/b`.
2. **Never test on the only copy of anything.** Scratch area:
   `mkdir -p ~/ofm-g3/{a,b}` and fill `a/` with *copies* of real data (a photo
   folder, some code, documents).
   **Already prepared for you** (made-up data, 2026-11-27): `~/ofm-g3/a` has odd names, 200 random
   `.jpg` files, a 1 GiB sparse file, a CP1251 text, a script, a symlink and hardlink, `project.zip`,
   `project.tar.gz`, `encrypted-password-is-ofm.zip` (password `ofm`) and `broken.zip`.
   No 7z tool is installed here: make a 7z yourself if you can (`sudo apt install 7zip`), ideally
   one with a password.
3. The list of sample files, if you want to recreate them in `~/ofm-g3/a`:
   - Odd names: `a b.txt`, `-rf`, `new␊line` (`touch "$(printf 'new\nline')"`), a
     non-UTF-8 name (`touch "$(printf 'x\xff.txt')"`), a very long name.
   - `truncate -s 1G sparse.bin` (sparse file)
   - A zip, a tar.gz and a 7z archive (small, made from your scratch files)
   - A text file in CP1251 (e.g. `iconv -f UTF-8 -t CP1251`)
   - A symlink (`ln -s target link`) and a hardlink pair (`ln a.txt a-hard.txt`)
4. Keep a terminal open for verification one-liners
   (`diff -rq`, `rsync -rlcnH --itemize-changes`, `find ~/ofm-g3 -name '*.ofm-part*'`).

## Panels (F01–F05)

- [ ] **F01** Tab switches the active panel (highlight moves); Ctrl+P hides/shows the other panel. Drag the splitter; quit and restart — split ratio and one/two-panel layout survive.
- [ ] **F02** Ctrl+1 Brief, Ctrl+2 Full, Ctrl+3 Custom columns. In a big directory scrolling is smooth, no stutter.
- [ ] **F03** Ctrl+F3 name, Ctrl+F4 extension, Ctrl+F5 modified, Ctrl+F6 size; same key again reverses. Header shows the sort indicator; `file2` sorts before `file10`; dirs first; sort survives restart.
- [ ] **F04** Status line shows the current file's info, selected count + total size, and free space of the panel's filesystem; it updates live while you select files.
- [ ] **F05** Ctrl+T opens a tab, Ctrl+W closes, Ctrl+Tab / Ctrl+Shift+Tab cycle, Ctrl+Shift+L locks a tab (it can't navigate away). Restart — tabs come back.

## Navigation (F06–F09)

- [ ] **F06** Type letters (or Alt+letters) to quick-search — cursor jumps to prefix then substring match; Ctrl+S toggles the same, Ctrl+Down/Up next/previous match, Esc cancels. No lag in a 100k-entry dir.
- [ ] **F07** Alt+F12 shows directory history, Alt+Left/Right go back/forward. Ctrl+D opens the hotlist; add a folder with Ctrl+Shift+D and reopen it from the dialog.
- [ ] **F08** Alt+F1 / Alt+F2 open the drive menu (Home, /, volumes, USB sticks, GVfs mounts, Trash, bookmarks). A volume mounted earlier in GNOME Files appears; mounting an unmounted volume from the menu works.
- [ ] **F09** Backspace (or Ctrl+PgUp) goes up — cursor lands on the directory you came from. Ctrl+\ goes to /, Ctrl+U swaps panels, Alt+= makes the other panel show this dir, Ctrl+R refreshes.

## Selection (F10)

- [ ] **F10** Ins toggles selection (and moves down), Shift+arrows select a range, Gray+ (or Alt+M) / Gray− (Alt+Shift+M) select/deselect by mask, Gray* (Ctrl+Shift+I) inverts, Ctrl+A / Ctrl+Shift+A select all / none, Ctrl+M restores the previous selection. Try masks `*.txt`, `*.{jpg,png}`, `!*.tmp`.

## Command line & terminal (F11–F12)

- [ ] **F11** Type a command in the command line (below the panels) and press Enter — it runs in the active panel's dir via your shell; `cd subdir` actually changes the panel. Ctrl+E / Ctrl+Shift+E walk history, Alt+F8 lists it; Ctrl+Enter / Ctrl+F insert the cursor file's name / full path; Ctrl+[ / Ctrl+] insert the left/right panel path; Tab completes paths.
- [ ] **F12** Ctrl+O hides the panels and shows the full embedded terminal (Ctrl+O again brings panels back). Shell cwd and panel dir stay in sync — `cd` in the shell moves the panel and vice versa. Tab completion works.

## File operations (F13–F20)

- [ ] **F13** F5 copies, F6 moves, Shift+F6 renames in place, F7 makes a dir, F8 deletes. The dialog's target path is editable; rename masks work (`*.txt` → `*.bak`); copying into the same dir asks for a new name.
- [ ] **F14** F8 moves files to Trash — they appear in GNOME Files > Trash and can be restored with content intact. Shift+Del deletes permanently (scratch area only!).
- [ ] **F15** During a big copy the UI stays responsive; the jobs view (Ctrl+J) shows bytes, files, speed, ETA; Space pauses/resumes, Del cancels. Quitting (F10) while jobs run asks first.
- [ ] **F16** Copy over existing files → conflict dialog with overwrite / skip / rename new / rename existing / overwrite-if-newer; Ctrl+A applies the decision to the rest of the job. The choice sticks for the whole job.
- [ ] **F17** After a copy: permissions, timestamps (incl. ns), symlinks (copied as links), xattrs preserved; sparse file stays sparse (`du` shows ~0); content byte-identical (`rsync -rlcnH --itemize-changes` prints nothing).
- [ ] **F18** Alt+F6 creates a symlink (absolute or relative — check both) and a hardlink; both work afterwards.
- [ ] **F19** Alt+Enter shows Properties: chmod via octal or checkboxes, chown/chgrp when permitted, directory size calculation. F3 (or Alt+Shift+Enter) on a directory calculates size async; it stays cancellable.
- [ ] **F20** Ctrl+Shift+M batch-renames the selected files via your editor: save the edited name list → preview shows the changes → apply. Conflicts/cycles are detected; Ctrl+Alt+Z undoes the last batch.

## Viewer & edit (F21–F23)

- [ ] **F21** F3 opens the viewer; a 10 GB file opens instantly. Inside: F2 wrap/unwrap, F4 hex mode, F7 search (text/hex/regex), Shift+F7 next match, Alt+F8 go to line/offset, F8 switches encoding (UTF-8, UTF-16, CP1251, CP866, KOI8-R, Latin-1 — try the CP1251 sample), Esc closes.
- [ ] **F22** Ctrl+Q turns the other panel into a quick view of the file under the cursor (text, image, or directory summary); it follows the cursor without blocking.
- [ ] **F23** F4 opens the file in your editor ($VISUAL/$EDITOR, else GNOME Text Editor); a terminal editor (vim/nano) runs inside the embedded terminal. Shift+F4 creates a new file.

## Search, panelize, archives (F24–F26)

- [ ] **F24** Alt+F7 finds files: name masks, content (text or regex, case option, encoding), size/date filters, follow-symlinks option. Results stream in while searching, the job is cancellable, F3/F4 work on a result, including hits inside archives (opened read-only from a temporary copy, up to 64 MiB).
- [ ] **F25** In the results dialog press F10 to panelize the results (or Ctrl+Alt+P to panelize any shell command's output). F5/F6/F8 on panelized items operate on the real files.
- [ ] **F26** Enter (or Ctrl+PgDn) opens a zip/tar/tar.gz/tar.bz2/tar.xz/tar.zst/7z as a read-only folder; F5 on entries extracts them. An encrypted zip or 7z asks for a password and asks again after a typo; cancelling the prompt extracts nothing. A corrupt archive shows an error, never a crash. F3/F4 on an entry works up to 64 MiB (larger: extract with F5). Try F5 on a whole folder inside an archive.

## Compare & customization (F27–F30)

- [ ] **F27** Shift+F2 compares the two panels' directories and marks files missing or differing on the other side (name+size+mtime by default; content compare runs async).
- [ ] **F28** F2 opens the user menu; run a user command with placeholders (`%f` file, `%s` selection, `%d` dir, `%D` other dir, `%{input:Prompt}`). A `.ofm-menu.toml` in a folder overrides/extends the menu there. Ctrl+Alt+M edits the menu.
- [ ] **F29** Every action above works from the keyboard. F1 shows help with all keys. Ctrl+, opens Preferences (links to the config files, reload). In config.toml set `[keymap] preset = "tc"` or `"mc"`; rebind a key in keymap.toml, reload, and it takes effect. Put a typo in config.toml: a warning must be visible in the window, with details under Help > Config warnings.
- [ ] **F30** Ctrl+Alt+T cycles themes: Far Blue, Commander Light, Adwaita — switch is instant. Edit a theme TOML on disk and it reloads live; file highlighting rules (colours by mask/attribute) apply.

## Desktop integration (F31–F33)

- [ ] **F31** Enter opens a file with its default app (GTK FileLauncher); an executable script runs in the terminal. Copy files in GNOME Files, paste in OFM with Ctrl+V (and vice versa with OFM's Ctrl+C / Ctrl+X). Drag & drop between OFM and other apps works.
- [ ] **F32** Hold Shift — the F-key bar shows the shifted variants; F9 opens the menu bar. Mouse: click, double-click open, right-click menu (keyboard: Shift+F10 or the Menu key), Shift/Ctrl-click selection. In the F9 menu, Left/Right switch menus and Esc returns to the panel. Try doing everything with mouse only, then keyboard only.
- [ ] **F33** Set panels, tabs, paths, cursor positions, view modes and window size to your liking; quit and restart — everything returns to the same state.

## Try to break it

- **Cancels:** start a big copy, cancel in the jobs view (Ctrl+J, Del) mid-file; then `find ~/ofm-g3 -name '*.ofm-part*'` must print nothing and no file under its final name may be truncated.
- **Full disk:** create a tiny tmpfs (`sudo mount -t tmpfs -o size=16M tmpfs /mnt/ofm-full`) and copy a folder bigger than 16 MB into it. Expect a clean ENOSPC error — no crash, no partial file under a final name, no temp files left.
- **USB stick:** copy a large folder to a throwaway stick, pull it out mid-copy (only the stick!). Then run `find /mnt -name '*.ofm-part*'` after unmount/remount. Note the stick's filesystem (`findmnt -no FSTYPE,TARGET /media/$USER/*`).
- **Wayland vs X11:** everything above on your normal Wayland session, then once with `GDK_BACKEND=x11 cargo run -p ofm-app` — note any difference (Alt key handling is Wayland-sensitive).
- **Odd names:** F5/F6 the odd-name files; names must stay byte-identical (`diff -rq` the result).
- **Quit during job:** F10 during a running copy; it asks, then waits up to ~15 s for cleanup — afterwards no `.ofm-part` files anywhere.

## Known gaps (do not report)

These are already tracked; skip them in your feedback:

- No file icons yet: highlight rules apply colours only (the `icon` setting is ignored).
- The tc and mc keymap presets differ only a little from Far. Say if you care.
- A themes folder created while OFM is running is not watched until restart.
- Viewing a very large encrypted archive entry is slow (it is checked completely before it is shown).
- F4 on a search hit inside an archive opens the read-only viewer without saying so.
- Stray dots / tiny specks in empty panel or quick-view areas on some sessions (known, not reproduced on Xvfb).
- Light theme: white text on orange active path bar is low contrast; some dialogs render in default libadwaita colours instead of the theme.
- Own-output collisions in the copy dialog say "source and destination are the same file" (wording).
- On GNOME/Wayland, Alt+F2 etc. may be grabbed by GNOME (run dialog); OFM sets `inhibit_system_shortcuts` but this needs real-session verification.
- Viewer: no file name in the status line, no text selection/copy, search case-folding is ASCII-only.
- Quick view keeps a stale "Up one level" header line on the preview side; directory summaries at `/` cross into /proc, /sys (slow until cursor moves).
- Properties: group field editable only as root; size-walk warnings for unreadable dirs don't reach the UI.
- Cancelled/paused jobs show no "Cancelling…" state until the worker stops.

## Questions for you

1. **Batch-rename undo matches names, not files.** Rename `a` to `x`, delete `x`, create a new `x`, undo: the new file is likely renamed to `a` (nothing is overwritten). Accept for 1.0? Recommended: yes; the stricter check is unreliable on FAT/exFAT sticks.
2. **64 MiB limit for F3/F4 inside archives.** It stops a compressed bomb from filling the disk. Is the limit too low for your files?
3. Do the Total Commander / Midnight Commander presets matter to you for 1.0?

## Still open from earlier gates (please check)

- USB stick copy/move results, and the stick's filesystem type.
- Ctrl+J, Del really cancels the running job (fixed after your G2 report).
- Alt+F1 / Alt+F2 / Alt+F7 reach OFM on your Wayland session.
- F10 during a copy leaves no `.ofm-part` files.
- In the terminal (Ctrl+O), run `ls` twice in the same folder, then change folder in a panel: the shell must still follow.

## How to report

For each finding include:

1. Feature id (F05) and the exact key sequence (e.g. "Ctrl+J, Del").
2. What you expected vs what happened, plus a screenshot (`PrtScr` or GNOME's tool).
3. Log: `RUST_LOG=ofm=debug cargo run -p ofm-app 2>ofm.log` and attach `ofm.log`.
4. For anything on an external drive: the filesystem type (`findmnt -no FSTYPE,TARGET`).
5. Whether you were on Wayland or X11 (`GDK_BACKEND=x11` run or plain).
