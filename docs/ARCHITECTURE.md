# OFM Architecture

This document is binding for all agents. Changes go through an ADR in `docs/adr/`.

## 1. Big picture

```
            ┌────────────────────────── ofm-app (GTK 4 / libadwaita, main thread) ─────────────────────────┐
            │  MainWindow ─ PanelView×2 (FileList widget) ─ CommandLine ─ Terminal (VTE) ─ FKeyBar ─ Dialogs │
            │        │ KeyChord                   ▲ UiEvent (async-channel, glib::spawn_future_local)        │
            │        ▼                            │                                                          │
            │  CommandDispatcher ──► Controllers (PanelController, JobsController, ViewerController, …)     │
            └────────┬─────────────────────────────┬───────────────────────────────────────────────────────┘
                     │ calls (non-blocking)        │ events
   ┌─────────────────▼────┐  ┌──────────────┐  ┌───▼──────────┐  ┌────────────┐  ┌────────────┐  ┌──────────┐
   │ ofm-core             │  │ ofm-vfs      │  │ ofm-jobs     │  │ ofm-viewer │  │ ofm-search │  │ofm-config│
   │ types, PanelModel,   │  │ Vfs trait,   │  │ JobEngine,   │  │ mmap doc,  │  │ walk+grep  │  │ TOML,    │
   │ masks, commands,     │  │ local, list, │  │ copy/move/   │  │ line index,│  │ streaming  │  │ keymaps, │
   │ keychords, cancel    │  │ archive (ro) │  │ delete/…     │  │ encodings  │  │ results    │  │ themes   │
   └──────────────────────┘  └──────────────┘  └──────────────┘  └────────────┘  └────────────┘  └──────────┘
         worker threads do all file I/O; results flow back as events
```

Dependency rules: `ofm-core` depends on nothing internal. `ofm-vfs`, `ofm-config`, `ofm-viewer` depend
on `ofm-core`. `ofm-jobs` and `ofm-search` depend on `ofm-core` + `ofm-vfs`. `ofm-app` depends on all.
Only `ofm-app` may depend on `gtk4`, `libadwaita`, `glib`, `gio`, `gdk`, `vte4`.
`ofm-testkit` is a dev-dependency only.

## 2. Crates and key types

Names below are binding; signatures are guidance for the architect's stubs (T1.0).

### ofm-core

```rust
pub enum Location {
    Local(PathBuf),
    Archive { archive: PathBuf, inner: PathBuf }, // archive file on local fs; no nested archives in v1.0
    List { id: ListId },                          // panelized results (search, command output)
}

pub struct Entry {
    pub name: OsString,             // display name (for List entries: path relative to list root or full)
    pub kind: EntryKind,            // File | Dir | Symlink { target: Option<TargetKind> } | Other
    pub size: u64,
    pub mtime_ns: i128,
    pub mode: u32,                  // st_mode
    pub uid: u32, pub gid: u32,
    pub real_path: Option<PathBuf>, // set for List/Archive-extracted entries
}

pub struct CancelToken(Arc<AtomicBool>);   // cheap clone, checked between syscalls / every 1 MiB

pub struct KeyChord { pub key: Key, pub mods: Mods } // parsed from "Ctrl+Shift+F5", "Gray+", "Alt+F7"
pub struct CommandId(&'static str);               // e.g. "file.copy", "panel.cursor_down"
```

- `panel::PanelModel` — entries (`Vec<Entry>`), sorted+filtered view (`Vec<u32>` indices), cursor,
  selection (by name, survives reloads), quick search, `SortSpec { key, reverse, dirs_first }`,
  natural compare, hidden-file toggle. Mutations return `ModelChange` (`Reset`, `Spliced { pos, removed, added }`)
  so the UI can update incrementally. Pure logic: no I/O.
- `mask` — file masks: `*.txt;*.md`, `*.{jpg,png}`, `!*.tmp`, case-insensitive option. Rename masks
  (`*.txt` → `*.bak`, `[N]_[C]`-style reserved for v1.1).
- `commands` — `CommandId` constants for every action in SPEC, plus metadata (title, context,
  description). Single source for menus, F-key bar, keymap validation and docs.
- `cmdline` — command-line helpers (quote paths for shell, parse `cd` targets, OSC 7 URI → path,
  history ring) — pure functions, unit tested.

### ofm-vfs

```rust
pub trait Vfs: Send + Sync {
    fn list(&self, loc: &Location, cancel: &CancelToken, sink: &mut dyn FnMut(ListEvent));
    fn stat(&self, loc: &Location) -> Result<Entry, VfsError>;
    fn open_read(&self, loc: &Location) -> Result<Box<dyn ReadSeek + Send>, VfsError>;
    fn capabilities(&self, loc: &Location) -> Caps; // read, write, rename, watch, trash, real_paths
    fn parent(&self, loc: &Location) -> Option<Location>;
}
pub enum ListEvent { Batch(Vec<Entry>), Done { total: usize }, Error(VfsError) }
```

- `local` — `read_dir` + `statx` (rustix) with `symlink_metadata` fallback; batches of 512 entries or
  every 16 ms, whichever first. Symlink target kind via a second `stat`. Free space via `statvfs`.
  Watching: `notify` (inotify), debounce 100 ms, emits changed names; the panel re-stats only those.
- `list` — in-memory lists for panelize; entries carry `real_path`; capabilities = those of real files.
- `archive` — read-only. On first entry, the whole archive is listed in a worker into an `ArchiveIndex`
  (tree). Formats: zip (`zip`, AES feature), tar + gz/bz2/xz/zst (`tar`, `flate2`, `bzip2`, `xz2`/`liblzma`,
  `zstd`), 7z (`sevenz-rust2`). Format detection by magic bytes, not extension. Rejects entries with
  absolute paths or `..` (zip-slip). Password via callback (`PasswordProvider`).
- `VfsRegistry` maps a `Location` to its provider. Remote providers (SFTP, …) in v1.1+ add
  implementations and a `VfsWrite` trait — nothing in v1.0 may assume "everything is local" outside
  `Location::Local` handling.

### ofm-jobs (data-safety critical)

```rust
pub struct JobEngine { /* worker threads, queues */ }
impl JobEngine {
    pub fn submit(&self, spec: JobSpec, mode: RunMode /* Queued | Parallel */) -> JobId;
    pub fn pause(&self, id: JobId); pub fn resume(&self, id: JobId); pub fn cancel(&self, id: JobId);
    pub fn events(&self) -> async_channel::Receiver<JobEvent>;
}
pub enum JobSpec {
    Copy { sources: Vec<Location>, dest_dir: PathBuf, opts: CopyOpts },  // rename mask, follow symlinks, …
    Move { sources: Vec<Location>, dest_dir: PathBuf, opts: CopyOpts },
    Delete { paths: Vec<PathBuf>, to_trash: bool },
    Mkdir { path: PathBuf }, Link { target: PathBuf, link: PathBuf, kind: LinkKind },
    SetAttrs { paths: Vec<PathBuf>, mode: Option<ModeChange>, owner: Option<OwnerChange>, recursive: bool },
    CalcSize { paths: Vec<PathBuf> },
    Extract { archive: PathBuf, entries: Vec<PathBuf>, dest_dir: PathBuf },
    Rename { pairs: Vec<(PathBuf, PathBuf)> },                               // batch rename (F20)
}
pub enum JobEvent {
    Started { id, title }, Progress { id, progress: Progress }, // throttled to ≤ 10 Hz per job
    Ask { id, question: Question, reply: Responder<Answer> },  // conflicts, errors (retry/skip/abort)
    Finished { id, outcome: Outcome },                         // Ok | Cancelled | Failed(summary)
}
```

- All file system calls go through an internal `trait Fs` (real implementation + `FaultyFs` for tests,
  which injects `ENOSPC`, `EIO`, `EACCES`, short writes at the N-th call).
- Two phases per job: **plan** (scan sources: count files/bytes, detect dest-inside-source and
  same-file by dev+ino, collect conflicts lazily) → **execute**.
- Worker panics are caught (`catch_unwind`) and reported as `Failed`.

**Copy algorithm (per file) — ADR-0004:**

1. Open source with `O_RDONLY | O_NOFOLLOW | O_CLOEXEC`; `fstat` it.
2. Create `dest_dir/.<name>.ofm-part-<random>` with `O_WRONLY | O_CREAT | O_EXCL`, mode 0600.
3. Data: try `ioctl(FICLONE)` (reflink, Btrfs/XFS) → else `copy_file_range` loop → else read/write with
   1 MiB buffer. If the source is sparse (`st_blocks * 512 < st_size`), use `SEEK_DATA`/`SEEK_HOLE`
   to skip holes. Check `CancelToken` every 1 MiB. Do not set `FS_NOCOW` or any attribute that disables
   Btrfs compression.
4. Metadata: `fchmod`, `futimens` (atime+mtime ns), xattrs (`user.*`, plus `security.*`/`trusted.*`
   only when permitted; failures are warnings), `fchown` only when running as root.
5. For **move** (cross-device) and when `opts.sync` is on: `fsync` the file, then `fsync` the dir after rename.
6. Rename temp → final: `renameat2(RENAME_NOREPLACE)`; if the user chose overwrite, plain `renameat`
   (atomic replace). Never truncate an existing destination in place.
7. On error/cancel: unlink the temp file. The destination name is never left half-written.

Directories: create (mode 0700 while copying), recurse, then set final mode + times post-order.
Symlinks: recreated (`readlinkat` + `symlinkat`) unless "follow symlinks" is on. FIFOs/devices/sockets:
skipped with a warning in v1.0. Hardlinks inside the copy set: copied as separate files (documented).

**Move:** same filesystem (`st_dev` equal) → `renameat2(RENAME_NOREPLACE)` per top-level item (atomic,
no data copied). Cross-device → copy (with fsync) → verify size and mtime → delete each source item
only after its copy succeeded; partial failures leave sources intact.

**Delete:** to trash via `trash` crate (freedesktop spec incl. per-mount `.Trash-$UID`); if the trash
is unavailable for that mount, ask before deleting permanently. Permanent delete: depth-first,
`unlinkat`/`AT_REMOVEDIR`, never follows symlinks, reports per-entry errors with retry/skip/abort.

### ofm-config

- XDG paths (`$XDG_CONFIG_HOME/ofm/`, `$XDG_STATE_HOME/ofm/`). Files: `config.toml`, `keymap.toml`,
  `themes/*.toml`, `menu.toml`; state: `session.toml`, `history.toml`.
- Built-in defaults embedded with `include_str!` (`defaults/`), user files override; schema `version = 1`.
- All writes atomic (temp + rename). Invalid user files → defaults + a visible warning, never a crash.
- Keymap TOML: sections per context (`[panels]`, `[cmdline]`, `[viewer]`, `[dialog]`), `"Ctrl+F5" = "panel.sort_mtime"`.
  Presets `far` (default), `tc`, `mc` embedded. Setting `typing = "command_line" | "quick_search"`
  (Far: printable keys go to the command line, Alt+letter = quick search; TC: letters = quick search).
- Theme TOML: semantic roles (see SPEC), `[[highlight]]` rules (`mask`, `attrs`, `fg`, `bg`, `bold`, `icon`),
  `[retro]` options. Live reload via `notify` on the themes directory.

### ofm-viewer

- `Document`: `memmap2` for regular files; buffered fallback for non-mappable sources (pipes, /proc)
  capped at 64 MiB; archive entries are extracted to a temp file first.
- `LineIndex`: built lazily in a worker in 4 MiB chunks with `memchr`; the UI can show the first screen
  before indexing finishes.
- Encodings: `encoding_rs` (UTF-8, UTF-16LE/BE, windows-1251, IBM866, KOI8-R, ISO-8859-1); detection:
  BOM → valid UTF-8 in first 64 KiB → `chardetng`.
- Search: `memchr::memmem` on bytes of the needle encoded in the current encoding; `regex::bytes` with
  chunk overlap; forward/backward; results as byte offsets.
- Hex rows: 16 bytes per row, offset + hex + ASCII/encoding column.

### ofm-search

- Walker: `ignore::WalkBuilder` with standard filters off (`hidden(false)`, `git_ignore(false)` …),
  parallel; options: follow symlinks, same-filesystem.
- Content: `grep-searcher` + `grep-regex` (binary detection, mmap, encoding via `encoding_rs_io`).
- Results stream as `SearchEvent::Found(Entry with real_path)` batches; cancellable; archive search via
  `ofm-vfs::archive` when enabled.

### ofm-app

- `app.rs` — `adw::Application`, startup (load config, theme, keymap), shutdown (save session).
- `dispatch.rs` — `CommandDispatcher`: one `EventControllerKey` on the window in **capture** phase
  converts GDK events to `KeyChord`, resolves context (panels / cmdline / viewer / dialog), looks up the
  keymap, executes the command. Text-entry keys pass through when an entry has focus.
- `widgets/file_list/` — **custom-drawn `FileList` widget** (ADR-0002): subclass of `gtk::Widget`,
  renders visible rows in `snapshot()` with cached `pango::Layout`s, implements `gtk::Scrollable`.
  Brief mode is column-major (fill down, then next column) like Far. Draws frames (box-drawing look in
  Far Blue), header with sort indicator, cursor, marked files, highlight colours from the theme.
  Handles mouse (click, double-click, Shift/Ctrl click, right-click menu, wheel, drag source).
  Accessibility: accessible role `list`, label updated with the item under the cursor.
- `panel/` — `PanelController` owns a `PanelModel`, a load generation counter (stale batches dropped),
  watcher subscription, tabs, history.
- `cmdline/` + `terminal/` — command line entry and VTE terminal (ADR-0003): one persistent shell per
  window spawned with an injected rc snippet that emits OSC 7 on every prompt (bash: `--rcfile`
  wrapper that sources `~/.bashrc`; zsh: `ZDOTDIR` wrapper; fish emits OSC 7 natively). The panel follows
  the shell's cwd; when the panel changes dir and the shell is idle, OFM sends ` cd -- '<path>'`
  (leading space keeps it out of history). Command line text is sent to the shell with `feed_child`.
- `viewer/` — viewer window + custom text/hex widget; `quickview/` with `PreviewProvider` trait
  (text, image, directory summary).
- `dialogs/` — copy/move, conflict, delete, mkdir, link, properties, mask, find, user menu, hotlist,
  batch-rename preview, password.
- `theme/` — converts `ofm-config::Theme` into a render palette for custom widgets and generated CSS
  (`gtk::CssProvider`) for standard widgets; Adwaita theme reads `adw::StyleManager`.
- `desktop/` — open with default app (`gtk::FileLauncher`), clipboard (`text/uri-list`,
  `x-special/gnome-copied-files`, text), drag & drop (`gdk::FileList`), mounts (`gio::VolumeMonitor`,
  `gtk::MountOperation`).

## 3. Threading model

- **Main thread:** GTK only. Never blocks; no file I/O (enforced: `crates/ofm-app/clippy.toml`
  disallows `std::fs::*` and `File::open/create`; exceptions need `#[allow]` + comment, review required).
- **I/O pool:** a small fixed pool (4 threads) in `ofm-vfs` for listing/stat/preview work.
- **Job engine:** one thread for the queued lane + one thread per parallel job.
- **Search:** its own walker threads per search.
- Communication: `async-channel`. UI side: `glib::spawn_future_local(async move { while let Ok(ev) = rx.recv().await { … } })`.
- Questions from workers (conflicts, errors, passwords) use a `Responder<T>` (oneshot) the UI answers;
  the worker blocks on it, the UI never blocks.
- Staleness: every panel load carries a generation number; results of old generations are dropped.

## 4. Error handling and logging

- Library crates: `thiserror` enums with path context. `ofm-app`: `anyhow` at the top level only.
- Errors visible to the user get a short message + details expander; nothing is swallowed silently.
- `tracing` everywhere; `RUST_LOG=ofm=debug cargo run -p ofm-app`.

## 5. Testing strategy

| Layer | How |
|---|---|
| ofm-core | unit + `proptest` (sorting, selection, masks, keychords, cmdline quoting) |
| ofm-vfs | integration tests on temp trees from `ofm-testkit`; 100k-entry listing benchmark (`criterion`) |
| ofm-jobs | torture suite (`ofm-testkit` trees: unicode + invalid UTF-8 names, 255-byte names, 200-level nesting, 10k small files, 2 GB sparse file, symlinks abs/rel/broken/loop, unreadable files, read-only dest), fault injection via `FaultyFs`, cancel at random points, cross-device move when two filesystems are available (else skipped with a log line), `assert_trees_equal` (content hash + metadata) |
| ofm-viewer | encoding fixtures (CP866, KOI8-R, CP1251, UTF-16), huge sparse file, binary data, search edge cases at chunk boundaries |
| ofm-search / archive | generated fixtures; malformed archives; zip-slip entries |
| ofm-app | smoke tests under Xvfb (`GDK_BACKEND=x11`): start app, drive it through `CommandDispatcher::execute(CommandId)` and assert controller state; screenshots via `scripts/screenshot.sh` for visual review |
| Fuzzing | `proptest`-based random inputs for archive listing and decoders (10 min budget in M9); `cargo fuzz` optional if nightly is installed |

## 6. Extension points (keep these clean; no v1.1 code in v1.0)

- `Vfs` / future `VfsWrite` — remote file systems (SFTP v1.1, FTP/WebDAV/SMB/S3 v1.2).
- `JobSpec` variants — sync, checksums, archive creation (v1.1).
- `PreviewProvider` — more quick-view types.
- `commands` registry — plugins (v2.0) register commands and keymap entries.
- Frontend separation — everything except `ofm-app` is UI-agnostic, so a TUI or other frontends (v2.0)
  reuse it unchanged.

## 7. Dependencies (initial; add others via `cargo add` and note them in the PR/commit)

gtk4, libadwaita, vte4, glib, gio (app only) · async-channel · rustix (fs, process) · memmap2 · memchr ·
encoding_rs, chardetng, encoding_rs_io · regex · globset · notify · trash · uzers · xattr · filetime ·
zip, tar, flate2, bzip2, xz2 (or liblzma), zstd, sevenz-rust2 · ignore, grep-searcher, grep-regex ·
serde, toml · directories (or etcetera) · thiserror, anyhow · tracing, tracing-subscriber · shell-escape ·
dev: tempfile, proptest, criterion.

Use the latest versions compatible with the minimum system libraries (GTK 4.14, libadwaita 1.5, VTE 0.76):
choose gtk4-rs feature flags accordingly (e.g. `gtk4 = { features = ["v4_14"] }`, `libadwaita = { features = ["v1_5"] }`).
