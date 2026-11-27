//! Working with the *contents* of an archive panel (T6.3): F5/F6 = extract (dialog + job), F3/F4 =
//! extract one entry to a private temp directory and open it in the viewer, F3/Alt+Shift+Enter on
//! a folder = its size from the index. Everything blocking runs on the I/O pool or in the job
//! engine; this file only builds specs and consumes results.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use ofm_core::{CancelToken, EntryKind, Location};
use ofm_jobs::{ConflictAnswer, ExtractOpts, JobSpec, RunMode};
use ofm_vfs::archive::{ArchiveEntryKind, ArchiveError, ArchiveLimits, PasswordSource};

use super::archive::{
    ArchiveService, busy_after, busy_done, default_extract_dir, error_text, report_error, split,
    state,
};
use crate::chrome::statusline::{SizeFormat, format_size};
use crate::compare::message;
use crate::context::AppContext;
use crate::dialogs::password;
use crate::i18n::tr;
use crate::jobs::actions::{self, Action};
use crate::jobs::targets::{Targets, normalize, resolve_path_with_origin};
use crate::jobs::{self, SubmitOpts, ui};
use crate::navigation::modal;
use crate::wiring;

// ---- extract (F5 / F6) ------------------------------------------------------------------------

/// Everything the user decides in the extract dialog.
pub struct ExtractInput<'a> {
    /// The archive file.
    pub archive: &'a Path,
    /// The folder inside the archive the items live in.
    pub inner_dir: &'a Path,
    /// Names of the selected items (never `..`).
    pub names: &'a [OsString],
    /// Text of the destination field.
    pub dest_text: &'a str,
    /// Path the field was initially filled with (lossy) so non-UTF-8 folders keep their bytes.
    pub dest_origin: Option<&'a Path>,
    /// Home directory for `~`.
    pub home: &'a Path,
    /// `[general] confirm_overwrite = false`.
    pub overwrite_silently: bool,
    /// Passwords shared with browsing.
    pub passwords: &'a PasswordSource,
}

/// Validate the dialog text and build the extract job. `Err` is a message for the dialog.
pub fn build_extract(input: &ExtractInput<'_>) -> Result<JobSpec, String> {
    if input.names.is_empty() {
        return Err(tr("Nothing is selected.").to_owned());
    }
    let base = input.archive.parent().unwrap_or(Path::new("/"));
    let dest_dir = resolve_path_with_origin(input.dest_text, input.dest_origin, base, input.home)
        .ok_or_else(|| tr("Enter a destination folder.").to_owned())?;
    let dest_dir = normalize(&dest_dir);
    Ok(JobSpec::Extract {
        archive: input.archive.to_owned(),
        entries: input
            .names
            .iter()
            .map(|n| input.inner_dir.join(n))
            .collect(),
        dest_dir,
        opts: ExtractOpts {
            on_conflict: input
                .overwrite_silently
                .then_some(ConflictAnswer::Overwrite),
            password: input.passwords.clone(),
            limits: ArchiveLimits::default(),
            sync: false,
        },
    })
}

/// The extract dialog: the copy dialog's shape (editable destination, Enter runs, F2 queues, Esc
/// cancels) for the selection of the archive panel. `moving` = F6: still extracts (an archive
/// cannot lose entries), and says so.
pub fn present(ctx: &Rc<AppContext>, moving: bool) {
    let Some(targets) = Targets::from_active(ctx) else {
        return;
    };
    let Some((archive, inner)) = split(&targets.dir).map(|(a, i)| (a.to_owned(), i.to_owned()))
    else {
        return;
    };
    let Some(controller) = jobs::controller(ctx) else {
        return;
    };
    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Extract"));
    dialog.set_content_width(520);
    let column = modal::padded_column(8);
    let what = targets.describe();
    column.append(&ui::label(&format!(
        "{} {what} {}",
        tr("Extract"),
        tr("to:")
    )));
    let dest_origin = default_extract_dir(&ctx.other_controller().location(), &archive);
    let dest = gtk::Entry::new();
    dest.set_widget_name("ofm-transfer-dest");
    dest.update_property(&[gtk::accessible::Property::Label(tr("Destination folder"))]);
    dest.set_hexpand(true);
    dest.set_text(
        &dest_origin
            .as_deref()
            .map_or(String::new(), |p| p.to_string_lossy().into_owned()),
    );
    column.append(&dest);
    if moving {
        column.append(&ui::caption(tr(
            "Archives are read-only: the items are extracted, not moved.",
        )));
    }
    let parallel =
        gtk::CheckButton::with_mnemonic(tr("Run in _parallel (do not wait in the queue)"));
    parallel.set_widget_name("ofm-transfer-parallel");
    column.append(&parallel);
    let error = ui::error_label();
    error.set_widget_name("ofm-transfer-error");
    column.append(&error);
    let cancel = ui::button(tr("Cancel"));
    let queue = ui::button(tr("_Queue"));
    queue.set_tooltip_text(Some(tr("Add to the job queue (F2)")));
    let run = ui::button(tr("_Extract"));
    run.add_css_class("suggested-action");
    run.set_widget_name("ofm-transfer-run");
    column.append(&ui::button_row(&[&cancel, &queue, &run]));
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&dest));

    let submit: Rc<dyn Fn(bool) -> bool> = {
        let (ctx, targets, controller) = (Rc::clone(ctx), targets.clone(), Rc::clone(&controller));
        let (dest, parallel, error) = (dest.clone(), parallel.clone(), error.clone());
        let origin = dest_origin.clone();
        Rc::new(move |force_queue: bool| {
            let home = ui::home();
            let names = targets.names();
            let input = ExtractInput {
                archive: &archive,
                inner_dir: &inner,
                names: &names,
                dest_text: &dest.text(),
                dest_origin: origin.as_deref(),
                home: &home,
                overwrite_silently: !ctx.config.borrow().general.confirm_overwrite,
                passwords: ctx.services.archives.passwords(),
            };
            match build_extract(&input) {
                Ok(spec) => {
                    let mode = if parallel.is_active() && !force_queue {
                        RunMode::Parallel
                    } else {
                        RunMode::Queued
                    };
                    controller.submit_with(
                        &ctx,
                        spec,
                        mode,
                        SubmitOpts {
                            origin: Some(Rc::downgrade(&targets.controller)),
                            on_finished: None,
                        },
                    );
                    true
                }
                Err(msg) => {
                    ui::set_error(&error, Some(&msg));
                    false
                }
            }
        })
    };

    let weak = dialog.downgrade();
    for (button, force_queue) in [(&run, false), (&queue, true)] {
        let (submit, weak) = (Rc::clone(&submit), weak.clone());
        button.connect_clicked(move |_| {
            if submit(force_queue)
                && let Some(d) = weak.upgrade()
            {
                d.close();
            }
        });
    }
    {
        let weak = weak.clone();
        cancel.connect_clicked(move |_| {
            if let Some(d) = weak.upgrade() {
                d.close();
            }
        });
    }
    {
        let error = error.clone();
        dest.connect_changed(move |_| ui::set_error(&error, None));
    }
    let ctx_ok = Rc::clone(ctx);
    let on_ok = {
        let submit = Rc::clone(&submit);
        move || {
            if ui::activate_focused_button(&ctx_ok) {
                return false;
            }
            submit(false)
        }
    };
    if modal::present(ctx, &dialog, on_ok) {
        actions::bind(ctx, &dialog, move |a| {
            if a != Action::Queue {
                return false;
            }
            if submit(true)
                && let Some(d) = weak.upgrade()
            {
                d.close();
            }
            true
        });
    }
}

// ---- one entry to a private temp file (viewer, quick view) ------------------------------------

/// Largest archive entry F3/F4 will write to a private temp file.
pub const ARCHIVE_VIEW_LIMIT: u64 = 64 << 20;

/// A file extracted for viewing.
#[derive(Debug)]
pub struct Extracted {
    /// The private directory holding it (delete with `ArchiveService::remove_temp`).
    pub dir: PathBuf,
    /// The file (named like the entry, mode 0600).
    pub file: PathBuf,
    /// Only the first part was extracted (the entry is larger than the limit).
    pub truncated: bool,
}

/// Why [`extract_entry`] failed.
#[derive(Debug)]
pub enum ExtractFail {
    /// Cancelled by the user.
    Cancelled,
    /// Archive problem.
    Archive(ArchiveError),
    /// Anything else (temp file trouble, not a regular file).
    Other(String),
}

impl ExtractFail {
    /// Text for a message dialog.
    pub fn text(&self) -> String {
        match self {
            ExtractFail::Cancelled => tr("Cancelled.").to_owned(),
            ExtractFail::Archive(e) => error_text(e),
            ExtractFail::Other(s) => s.clone(),
        }
    }
}

/// Extract the regular file `inner` of `archive` into a fresh private temp directory, at most
/// `limit` bytes of it. `quiet`: never ask for a password (quick view). Blocking: worker threads only.
#[allow(clippy::disallowed_methods)] // worker thread
pub fn extract_entry(
    service: &ArchiveService,
    archive: &Path,
    inner: &Path,
    limit: u64,
    quiet: bool,
    cancel: &CancelToken,
) -> Result<Extracted, ExtractFail> {
    use std::os::unix::fs::OpenOptionsExt;
    let _scope = password::scope(cancel);
    let _quiet = quiet.then(password::quiet);
    let index = service
        .vfs()
        .index(archive, cancel)
        .map_err(ExtractFail::Archive)?;
    let entry = index
        .lookup(inner)
        .ok_or(ExtractFail::Archive(ArchiveError::NoSuchEntry(0)))?;
    if !matches!(
        entry.kind,
        ArchiveEntryKind::File | ArchiveEntryKind::Hardlink
    ) {
        return Err(ExtractFail::Other(
            tr("Only regular files can be viewed.").to_owned(),
        ));
    }
    let id = entry.id;
    let name = entry
        .path
        .file_name()
        .map_or_else(|| OsString::from("entry"), ToOwned::to_owned);
    let dir = service
        .new_temp_dir()
        .map_err(|e| ExtractFail::Other(e.to_string()))?;
    let file = dir.join(&name);
    let result = (|| {
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file)
            .map_err(|e| ExtractFail::Other(format!("{}: {e}", file.display())))?;
        let mut reader = service
            .vfs()
            .open_reader(archive, cancel)
            .map_err(ExtractFail::Archive)?;
        let mut truncated = false;
        let mut written = 0u64;
        let visited = reader.visit(&[id], cancel, &mut |_entry, data| {
            copy_bounded(
                data,
                &mut out,
                limit,
                &mut written,
                &mut truncated,
                cancel,
                archive,
            )
        });
        match visited {
            Ok(()) => {}
            Err(ArchiveError::Cancelled) if truncated => {} // we stopped it ourselves
            Err(ArchiveError::Cancelled) => return Err(ExtractFail::Cancelled),
            Err(e) => return Err(ExtractFail::Archive(e)),
        }
        out.flush()
            .map_err(|e| ExtractFail::Other(format!("{}: {e}", file.display())))?;
        Ok(truncated)
    })();
    match result {
        Ok(truncated) => Ok(Extracted {
            dir,
            file,
            truncated,
        }),
        Err(e) => {
            service.remove_temp(&dir);
            Err(e)
        }
    }
}

/// Copy up to `limit` bytes; at the limit set `truncated` and stop the visit with `Cancelled`.
fn copy_bounded(
    from: &mut dyn Read,
    to: &mut dyn Write,
    limit: u64,
    written: &mut u64,
    truncated: &mut bool,
    cancel: &CancelToken,
    archive: &Path,
) -> Result<(), ArchiveError> {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let want = if *written >= limit {
            // Anything left? Only then is the result truncated.
            let mut probe = [0u8; 1];
            if from
                .read(&mut probe)
                .map_err(|e| ArchiveError::io(archive, e))?
                > 0
            {
                *truncated = true;
                return Err(ArchiveError::Cancelled);
            }
            return Ok(());
        } else {
            buf.len()
                .min(usize::try_from(limit - *written).unwrap_or(buf.len()))
        };
        let n = from
            .read(&mut buf[..want])
            .map_err(|e| ArchiveError::io(archive, e))?;
        if n == 0 {
            return Ok(());
        }
        to.write_all(&buf[..n])
            .map_err(|e| ArchiveError::io(archive, e))?;
        *written += n as u64;
    }
}

// ---- F3 / F4 ----------------------------------------------------------------------------------

/// F3/F4/Enter on the entry under the cursor of an archive panel: files open read-only in the
/// viewer (extracted to a private temp file first), folders show their size.
pub fn view_cursor(ctx: &Rc<AppContext>) {
    let ctrl = ctx.active_controller();
    let (entry, loc) = {
        let model = ctrl.model();
        (model.cursor_entry().cloned(), ctrl.location())
    };
    let Some(entry) = entry else { return };
    if entry.name == ".." {
        return;
    }
    let Some((archive, inner)) = split(&loc) else {
        return;
    };
    match entry.kind {
        k if k.is_dir_like() => calc_dirs(ctx, &ctrl, vec![entry.name]),
        EntryKind::File => {
            let (archive, path) = (archive.to_owned(), inner.join(&entry.name));
            view_file(ctx, archive, path);
        }
        _ => message::show(ctx, tr("Only regular files can be viewed.")),
    }
}

/// Extract `inner` of `archive` on the pool and open the viewer on it.
pub fn view_file(ctx: &Rc<AppContext>, archive: PathBuf, inner: PathBuf) {
    view_file_at(ctx, archive, inner, None);
}

/// View an archive entry and scroll to `offset` after its document opens.
pub fn view_file_at(ctx: &Rc<AppContext>, archive: PathBuf, inner: PathBuf, offset: Option<u64>) {
    let Some(st) = state(ctx) else { return };
    let cancel = CancelToken::new();
    let serial = st.begin(&cancel);
    busy_after(ctx, serial, tr("Extracting... (Esc cancels)"));
    let service = ctx.services.archives.clone();
    let worker_cancel = cancel.clone();
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let _ = tx.send_blocking(extract_entry(
            &service,
            &archive,
            &inner,
            ARCHIVE_VIEW_LIMIT,
            false,
            &worker_cancel,
        ));
    });
    let weak = Rc::downgrade(ctx);
    wiring::attach(
        rx,
        move |result| {
            let Some(ctx) = weak.upgrade() else {
                return std::ops::ControlFlow::Break(());
            };
            let Some(st) = state(&ctx) else {
                return std::ops::ControlFlow::Break(());
            };
            let current = st.is_current(serial);
            if current {
                st.end(serial);
                busy_done(&ctx);
            }
            match result {
                Ok(x) if current && x.truncated => {
                    remove_temp_later(&ctx, x.dir);
                    report_error(
                        &ctx,
                        tr(
                            "Archive entry is too large to view (64 MiB limit). Extract it with F5 instead.",
                        ),
                    );
                }
                Ok(x) if current => open_in_viewer(&ctx, x, offset),
                Ok(x) => remove_temp_later(&ctx, x.dir), // cancelled meanwhile
                Err(ExtractFail::Cancelled) => {}
                Err(e) if current => report_error(&ctx, &e.text()),
                Err(_) => {}
            }
            std::ops::ControlFlow::Break(())
        },
        || {},
    );
}

fn remove_temp_later(ctx: &Rc<AppContext>, dir: PathBuf) {
    let service = ctx.services.archives.clone();
    ctx.services.pool.spawn(move || service.remove_temp(&dir));
}

/// Open the extracted file; its directory is deleted when the viewer window closes.
fn open_in_viewer(ctx: &Rc<AppContext>, x: Extracted, offset: Option<u64>) {
    match crate::viewer::window::open(ctx, vec![x.file.clone()], 0) {
        Some(session) => {
            let (service, pool, dir) = (
                ctx.services.archives.clone(),
                ctx.services.pool.clone(),
                x.dir,
            );
            session.set_on_close(move || {
                let (service, dir) = (service.clone(), dir.clone());
                pool.spawn(move || service.remove_temp(&dir));
            });
            if let Some(offset) = offset {
                crate::viewer::window::jump_to_offset_when_ready(&session, offset);
            }
        }
        None => remove_temp_later(ctx, x.dir), // no window (headless): nothing to show
    }
}

// ---- sizes ------------------------------------------------------------------------------------

/// Totals of one folder of an archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DirTotals {
    /// Sum of the files' sizes.
    pub bytes: u64,
    /// Files below (links and special entries count as files).
    pub files: u64,
    /// Folders below.
    pub dirs: u64,
}

/// Totals of the folder `path` (the folder itself is not counted) from the index.
pub fn totals_of(index: &ofm_vfs::archive::ArchiveIndex, path: &Path) -> DirTotals {
    let mut t = DirTotals::default();
    for e in index
        .select(&[path.to_owned()])
        .iter()
        .filter_map(|id| index.get(*id))
    {
        match e.kind {
            ArchiveEntryKind::Dir => t.dirs += 1,
            _ => {
                t.files += 1;
                t.bytes += e.size;
            }
        }
    }
    t.dirs = t.dirs.saturating_sub(1); // the folder itself
    t
}

/// `file.calc_size` in an archive: sizes of the selected folders (or the one under the cursor).
pub fn calc_sizes(ctx: &Rc<AppContext>) {
    let Some(targets) = Targets::from_active(ctx) else {
        return;
    };
    let names: Vec<OsString> = targets
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::Dir)
        .map(|e| e.name.clone())
        .collect();
    if !names.is_empty() {
        calc_dirs(ctx, &targets.controller, names);
    }
}

/// Add up the folders `names` of `ctrl`'s archive location on the pool (from the index: nothing
/// is extracted) and show each size in the panel's size column.
pub fn calc_dirs(ctx: &Rc<AppContext>, ctrl: &Rc<super::PanelController>, names: Vec<OsString>) {
    let location = ctrl.location();
    let Some((archive, inner)) = split(&location).map(|(a, i)| (a.to_owned(), i.to_owned())) else {
        return;
    };
    let service = ctx.services.archives.clone();
    let worker_names = names.clone();
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let cancel = CancelToken::new();
        let _scope = password::scope(&cancel);
        let result = service.vfs().index(&archive, &cancel).map(|index| {
            worker_names
                .iter()
                .map(|n| (n.clone(), totals_of(&index, &inner.join(n))))
                .collect::<Vec<_>>()
        });
        let _ = tx.send_blocking(result);
    });
    let (weak_ctx, weak_ctrl) = (Rc::downgrade(ctx), Rc::downgrade(ctrl));
    wiring::attach(
        rx,
        move |result| {
            let (Some(ctx), Some(ctrl)) = (weak_ctx.upgrade(), weak_ctrl.upgrade()) else {
                return std::ops::ControlFlow::Break(());
            };
            match result {
                Ok(sizes) => show_sizes(&ctx, &ctrl, &location, sizes),
                Err(e) => report_error(&ctx, &error_text(&e)),
            }
            std::ops::ControlFlow::Break(())
        },
        || {},
    );
}

fn show_sizes(
    ctx: &Rc<AppContext>,
    ctrl: &Rc<super::PanelController>,
    location: &Location,
    sizes: Vec<(OsString, DirTotals)>,
) {
    if &ctrl.location() != location {
        return; // navigated away meanwhile
    }
    let mut updated = Vec::new();
    for (name, totals) in &sizes {
        let entry = {
            let m = ctrl.model();
            (0..m.visible_len())
                .filter_map(|i| m.visible(i))
                .find(|e| &e.name == name)
                .cloned()
        };
        if let Some(mut e) = entry {
            e.size = totals.bytes;
            updated.push(e);
        }
    }
    if !updated.is_empty() {
        ctrl.mutate_view(|m| m.apply_update(updated, &[]));
    }
    if let [(name, t)] = sizes.as_slice() {
        message::show(
            ctx,
            &format!(
                "{}: {} \u{b7} {} {}, {} {}",
                name.to_string_lossy(),
                format_size(t.bytes, SizeFormat::Binary),
                t.files,
                tr("files"),
                t.dirs,
                tr("folders")
            ),
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn input<'a>(
        names: &'a [OsString],
        dest: &'a str,
        passwords: &'a PasswordSource,
    ) -> ExtractInput<'a> {
        ExtractInput {
            archive: Path::new("/data/x.zip"),
            inner_dir: Path::new("d"),
            names,
            dest_text: dest,
            dest_origin: None,
            home: Path::new("/home/u"),
            overwrite_silently: false,
            passwords,
        }
    }

    #[test]
    fn builds_an_extract_job_with_inner_paths() {
        let pw = PasswordSource::none();
        let names = [OsString::from("a.txt"), OsString::from("sub")];
        let spec = build_extract(&input(&names, "~/out/", &pw)).unwrap();
        match spec {
            JobSpec::Extract {
                archive,
                entries,
                dest_dir,
                opts,
            } => {
                assert_eq!(archive, Path::new("/data/x.zip"));
                assert_eq!(entries, [PathBuf::from("d/a.txt"), PathBuf::from("d/sub")]);
                assert_eq!(dest_dir, Path::new("/home/u/out"));
                assert!(opts.on_conflict.is_none());
            }
            other => panic!("unexpected {other:?}"),
        }
        // Relative destinations are relative to the archive's folder; blank is refused.
        let spec = build_extract(&input(&names, "../o", &pw)).unwrap();
        assert!(matches!(spec, JobSpec::Extract { dest_dir, .. } if dest_dir == Path::new("/o")));
        assert!(build_extract(&input(&names, "  ", &pw)).is_err());
        assert!(build_extract(&input(&[], "/x", &pw)).is_err());
        let mut silent = input(&names, "/x", &pw);
        silent.overwrite_silently = true;
        assert!(matches!(
            build_extract(&silent).unwrap(),
            JobSpec::Extract { opts, .. } if opts.on_conflict == Some(ConflictAnswer::Overwrite)
        ));
    }

    #[test]
    fn bounded_copy_truncates_and_flags_it() {
        let cancel = CancelToken::new();
        let data = vec![7u8; 200_000];
        let (mut out, mut written, mut truncated) = (Vec::new(), 0u64, false);
        let r = copy_bounded(
            &mut data.as_slice(),
            &mut out,
            100,
            &mut written,
            &mut truncated,
            &cancel,
            Path::new("/a.zip"),
        );
        assert!(matches!(r, Err(ArchiveError::Cancelled)));
        assert!(truncated);
        assert_eq!((out.len(), written), (100, 100));
        // Exactly the limit: not truncated.
        let (mut out, mut written, mut truncated) = (Vec::new(), 0u64, false);
        let r = copy_bounded(
            &mut &data[..100],
            &mut out,
            100,
            &mut written,
            &mut truncated,
            &cancel,
            Path::new("/a.zip"),
        );
        assert!(r.is_ok() && !truncated && out.len() == 100);
        // A zero limit writes nothing and still detects that data remains.
        let (mut out, mut written, mut truncated) = (Vec::new(), 0u64, false);
        let r = copy_bounded(
            &mut data.as_slice(),
            &mut out,
            0,
            &mut written,
            &mut truncated,
            &cancel,
            Path::new("/a.zip"),
        );
        assert!(matches!(r, Err(ArchiveError::Cancelled)));
        assert!(truncated && out.is_empty() && written == 0);
    }
}
