//! The "other" operations (everything except copy/move): one file per operation so parallel tasks
//! never share a file (frozen layout by T3.0).
//!
//! | file | owner | signature of `run` |
//! |---|---|---|
//! | `delete.rs` | T3.2 | `(ctx, &[PathBuf], to_trash: bool)` |
//! | `mkdir.rs` | T3.2 | `(ctx, &Path)` |
//! | `link.rs` | T3.2 | `(ctx, target, link, LinkKind)` (+ `relative_target`, done) |
//! | `attrs.rs` | T3.2 | `(ctx, &[PathBuf], Option<ModeChange>, Option<OwnerChange>, recursive)` |
//! | `size.rs` | T3.2 | `(ctx, &[PathBuf]) -> SizeTotals` |
//! | `trash.rs` | T3.2 | `trash_path(&Path) -> Result<(), TrashError>` (called by `RealFs::trash`) |
//! | (extract, rename) | moved | extraction lives in `../extract.rs` (M6); batch rename lives in `../rename_batch.rs` |
//!
//! Rules for every operation: all I/O through `ctx.fs()`; `ctx.checkpoint()` between items and in
//! every loop; per-item failures through `ctx.attempt` / `ctx.resolve_error` (retry/skip/abort);
//! progress through `ctx.set_totals/add_bytes/file_done/set_current`; report top-level results with
//! `ctx.note_created`; never follow symlinks unless the operation says so; never `unwrap`.

pub mod attrs;
pub mod delete;
pub mod link;
pub mod mkdir;
pub mod size;
pub mod trash;
