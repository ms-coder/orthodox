# ADR-0009: Search engine, list provider and panelize (F24, F25)

Status: accepted · 2026-11-22 · frozen by T7.0 for T7.1 (engine), T7.2 (list + panelize), T7.3 (UI)

## Context

Alt+F7 searches by name, content, size and date, optionally inside archives, with results that stream
in and can be cancelled. Results (or the output of a shell command) can be "panelized": shown as a
panel in which F3/F4/F5/F6/F8 work on the real files. Engine, list provider and UI are separate tasks.

## Decision

**Search API (`ofm-search`).** `SearchQuery` is plain data (roots, `NameFilter` glob list or regex,
optional `ContentQuery` text/regex + case + whole words + `SearchEncoding`, `SizeFilter`, `TimeFilter`
in absolute ns, `follow_symlinks`, `same_filesystem`, `include_hidden` (default true; ignore files are
never honoured), `search_archives`, `max_depth`, `max_hits`, `threads`). `query.validate()` checks it
without I/O so the dialog can name the bad field (`SearchError::InvalidQuery { field, .. }`); T7.1 adds
regex compilation. `SearchHandle::start(query, CancelToken)` returns at once and spawns workers;
`handle.events()` is a bounded `async_channel::Receiver<SearchEvent>` (capacity 64: a stalled UI
throttles workers; a dropped receiver or handle cancels). Events: `Found(Vec<Hit>)` (<= 256, flushed
at least every 50 ms), `Progress` (<= 10 Hz), `Warning` (permission denied, symlink loop, encrypted
archive skipped), then exactly one `Done(SearchSummary { cancelled, truncated, .. })` or
`Failed(SearchError)`. Cancel uses `ofm_core::CancelToken`, like jobs and the viewer. One `Hit` per
file: `Location` (local or archive), an `Entry` (`real_path` set for local hits) and an optional
`ContentHit { first: ContentMatch { line, byte_offset, snippet, snippet_match }, count }`, so F3 can
open the viewer at the byte offset. Encodings are `ofm_viewer::Encoding` (`Auto` = viewer detection),
which keeps search and viewer consistent; `ofm-search` therefore depends on `ofm-viewer`.

**Engine rules (T7.1).** Walker: `ignore::WalkBuilder::build_parallel` with all standard filters off;
symlink loops detected by (dev, ino); `same_filesystem` compares `st_dev` with the root. Content:
`grep-searcher` + `grep-regex`, literal text escaped into a regex, line-oriented (no multi-line
matches), binary files skipped unless `search_binary`, decoding through the chosen encoding. Archives:
`ofm_vfs::archive::open_archive` with `NoPassword` (a search never prompts), one level, entries
streamed through `visit` with the same limits as browsing; unsafe entries are never reported. A
panicking worker ends the search with `Failed(Panic)` (invariant 7). Target: 100k-file name search
< 2 s. The engine never reads the clock (the UI turns "last 7 days" into absolute bounds).

**List provider (`ofm-vfs::list`).** `PanelList { title, base, entries }` holds real local paths: every
row is an `Entry` with `real_path = Some(..)`, `name` = path relative to `base` (else the full path),
unique within the list; duplicates collapse. `ListStore` (shared, `create/get/append/remove/real_paths/
forget_paths`) holds lists; `ListVfs` (registered for `LocationKind::List`) serves them. `PanelList::
from_paths` lstat's paths on a worker and ignores non-existing ones (counted). Capabilities: `read`,
`rename`, `trash`, `real_paths`; no `write` (nothing can be created in a list), no `watch`.
**File operations never see `Location::List`**: the app maps the selected rows to
`Location::Local(real_path)` (`ListStore::real_paths`) and submits normal copy/move/delete jobs;
afterwards `forget_paths` drops vanished rows and a refresh re-stats (`ListVfs::list`, T7.2). A list
has no parent: leaving it (Backspace/Esc) restores the previous location from panel history. Archive
hits cannot be panelized in v1.0 (`Hit::to_list_entry` is `None`); the UI reports how many were left out.

**Panelize command (`ofm-core::panelize`).** `run_panelize(&PanelizeSpec, &CancelToken, sink)` runs
`<shell> -c <command>` in the panel's directory with stdin closed on the caller's worker thread and
streams `PanelizeEvent::Paths` batches (<= 1024; cap 1 000 000) then one `Done(PanelizeSummary)`.
`parse_paths` (implemented, pure): split at `\n` or NUL, drop empty items and a trailing `\r`, join
relative items to `cwd`, drop `.` components, keep `..`; bytes, not `String`. A non-zero exit code is
not an error (`grep -l`, `find`): the summary carries the code and a stderr tail and the UI warns.
Existence is checked by `PanelList::from_paths`, not by core. Cancel kills the process group.

## Alternatives considered

- Search results as `Location::List` entries fed directly by the engine: couples engine and store;
  instead the UI converts hits (`Hit::to_list_entry`) and appends them to a `ListStore` list.
- A `Vfs::list`-style callback sink for search: loses batching, progress and warnings, and the
  bounded channel back-pressure.
- A unified `JobSpec::Search`: search is read-only, long-lived and result-oriented; the job queue
  (one disk-heavy job at a time, Ask protocol) does not fit.
- Resolving list rows through a new `Location::ListItem`: touches every provider; the real-path
  mapping keeps jobs oblivious to lists.

## Consequences

- T7.1 owns `crates/ofm-search/**` (adds dependencies `ofm-vfs`, `ignore`, `grep-*`, `regex`, `tracing`).
- T7.2 owns `crates/ofm-vfs/src/list/**` and `crates/ofm-core/src/panelize.rs` (`run_panelize` body,
  `from_paths`, re-stat in `ListVfs::list`); the types and `ListStore` are frozen.
- T7.3 builds the dialog/results panel against `SearchEvent`, `PanelList`, `PanelizeEvent` only.
- Content search inside archives and non-UTF-8 file names are covered by tests in T7.1/T7.4.
