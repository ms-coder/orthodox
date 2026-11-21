//! The file clipboard (F31): `file.clipboard_copy` (Ctrl+C), `file.clipboard_cut` (Ctrl+X),
//! `file.clipboard_paste` (Ctrl+V) and `file.copy_path` (Ctrl+Shift+C).
//!
//! The clipboard carries a `GdkFileList` (serialised to `text/uri-list`) next to
//! `x-special/gnome-copied-files`, so GNOME Files and OFM exchange copy/cut in both directions.
//! Paste runs the ordinary copy/move jobs into the active panel's directory (conflicts are
//! asked through the usual dialog). Text entries and the terminal keep their own Ctrl+C/V/X
//! (the key dispatcher leaves those chords to them); these commands only run from the panels.

use std::path::PathBuf;
use std::rc::Rc;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use ofm_core::cmd;

use super::clip::{self, ClipFiles, ClipOp, GNOME_COPIED_FILES};
use super::transfer;
use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::i18n::tr;
use crate::jobs::targets::Targets;
use crate::jobs::ui;

/// Upper bound of the clipboard payload we read (a list of a few hundred thousand paths).
const MAX_PAYLOAD: usize = 8 << 20;

/// Register the clipboard commands.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::FILE_CLIPBOARD_COPY, |ctx| put(ctx, ClipOp::Copy));
    reg.register(cmd::FILE_CLIPBOARD_CUT, |ctx| put(ctx, ClipOp::Cut));
    reg.register(cmd::FILE_CLIPBOARD_PASTE, paste);
    reg.register(cmd::FILE_COPY_PATH, copy_path);
}

fn clipboard_of(ctx: &AppContext) -> Option<gdk::Clipboard> {
    ctx.window().map(|w| w.window.clipboard())
}

/// Content provider carrying `paths` as a file list and as `x-special/gnome-copied-files`.
pub fn provider_for(op: ClipOp, paths: &[PathBuf]) -> gdk::ContentProvider {
    let files: Vec<gio::File> = paths.iter().map(gio::File::for_path).collect();
    let list = gdk::FileList::from_array(&files);
    let gnome = glib::Bytes::from_owned(clip::encode_gnome(op, paths));
    gdk::ContentProvider::new_union(&[
        gdk::ContentProvider::for_value(&list.to_value()),
        gdk::ContentProvider::for_bytes(GNOME_COPIED_FILES, &gnome),
    ])
}

/// Local paths of the active panel's selection (or the item under the cursor); a message is
/// shown when the items are not on this computer.
fn selected_paths(ctx: &Rc<AppContext>, title: &str) -> Option<Vec<PathBuf>> {
    let targets = Targets::from_active(ctx)?;
    let paths = targets.local_paths();
    if paths.is_none() {
        ui::message(
            ctx,
            title,
            tr("Only files and folders on this computer can be put on the clipboard."),
        );
    }
    paths
}

/// Ctrl+C / Ctrl+X.
fn put(ctx: &Rc<AppContext>, op: ClipOp) {
    let title = match op {
        ClipOp::Copy => tr("Copy"),
        ClipOp::Cut => tr("Cut"),
    };
    let Some(paths) = selected_paths(ctx, title) else {
        return;
    };
    let Some(clipboard) = clipboard_of(ctx) else {
        return;
    };
    if let Err(e) = clipboard.set_content(Some(&provider_for(op, &paths))) {
        tracing::warn!(error = %e, "cannot set the clipboard");
        ui::message(ctx, title, &e.to_string());
    }
}

/// Ctrl+Shift+C: the full paths as text (one per line).
fn copy_path(ctx: &Rc<AppContext>) {
    let Some(paths) = selected_paths(ctx, tr("Copy path")) else {
        return;
    };
    if let Some(clipboard) = clipboard_of(ctx) {
        clipboard.set_text(&clip::paths_as_text(&paths));
    }
}

/// Read everything from `stream` (bounded) without blocking the main loop.
async fn read_all(stream: &gio::InputStream) -> Option<Vec<u8>> {
    let mut data = Vec::new();
    loop {
        let chunk = stream
            .read_bytes_future(64 * 1024, glib::Priority::DEFAULT)
            .await
            .ok()?;
        if chunk.is_empty() {
            return Some(data);
        }
        data.extend_from_slice(&chunk);
        if data.len() > MAX_PAYLOAD {
            return None;
        }
    }
}

/// What the clipboard holds as files, from GNOME's format first, else a plain file list (copy).
/// `None` when it holds no (local) files.
pub async fn read_files(clipboard: &gdk::Clipboard) -> Option<ClipFiles> {
    let formats = clipboard.formats();
    if formats.contain_mime_type(GNOME_COPIED_FILES)
        && let Ok((stream, _mime)) = clipboard
            .read_future(&[GNOME_COPIED_FILES], glib::Priority::DEFAULT)
            .await
        && let Some(data) = read_all(&stream).await
        && let Some(files) = clip::decode_gnome(&data)
    {
        return Some(files);
    }
    if formats.contains_type(gdk::FileList::static_type())
        && let Ok(value) = clipboard
            .read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT)
            .await
        && let Ok(list) = value.get::<gdk::FileList>()
    {
        let paths: Vec<PathBuf> = list.files().iter().filter_map(gio::File::path).collect();
        if !paths.is_empty() {
            return Some(ClipFiles {
                op: ClipOp::Copy,
                paths,
            });
        }
    }
    None
}

/// Ctrl+V.
fn paste(ctx: &Rc<AppContext>) {
    let Some(clipboard) = clipboard_of(ctx) else {
        return;
    };
    if !clipboard.formats().contain_mime_type(GNOME_COPIED_FILES)
        && !clipboard
            .formats()
            .contains_type(gdk::FileList::static_type())
    {
        return; // text or nothing: not ours to paste into a panel
    }
    let Some(dest) = transfer::active_dest_dir(ctx, tr("Paste")) else {
        return;
    };
    let weak = Rc::downgrade(ctx);
    glib::spawn_future_local(async move {
        let Some(files) = read_files(&clipboard).await else {
            return;
        };
        let Some(ctx) = weak.upgrade() else { return };
        let mv = files.op == ClipOp::Cut;
        let started = transfer::submit(&ctx, &files.paths, &dest, mv);
        if mv && started {
            // The files are about to move: what the clipboard offers would be stale (GNOME
            // Files empties it after a cut-paste as well).
            let _ = clipboard.set_content(None::<&gdk::ContentProvider>);
        }
    });
}
