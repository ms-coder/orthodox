//! `file.new_file` (Shift+F4): ask for a name, create the file on a worker (nothing is written
//! to an existing file; an existing name is simply opened) and open it in the editor.

use std::io;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use ofm_core::cmd;
use ofm_jobs::{Fs, RealFs};

use super::editor;
use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::i18n::tr;
use crate::jobs::targets::resolve_path;
use crate::jobs::ui;
use crate::navigation::modal;
use crate::wiring;

/// Register `file.new_file`.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::FILE_NEW_FILE, present);
}

/// File to create for the text typed into the dialog; `Err` is a message for the dialog.
/// Relative text is taken relative to `dir`, `~` is expanded.
pub fn build_path(dir: &Path, text: &str, home: &Path) -> Result<PathBuf, String> {
    let name_missing = || tr("Enter a file name.").to_owned();
    if text.trim().is_empty() || text.ends_with('/') {
        return Err(name_missing());
    }
    let path = resolve_path(text, dir, home).ok_or_else(name_missing)?;
    if path == dir || path.file_name().is_none() {
        return Err(name_missing());
    }
    Ok(path)
}

/// What [`create`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Created {
    /// A new, empty file now exists.
    New,
    /// The name already was a file: left untouched.
    Existing,
}

/// Create `path` (and missing parent directories) through the jobs file-system layer: the file
/// is created exclusively (`O_EXCL`), so an existing file is never truncated. Blocking: call
/// from a worker.
pub fn create(fs: &dyn Fs, path: &Path) -> io::Result<Created> {
    if let Some(parent) = path.parent() {
        make_parents(fs, parent)?;
    }
    match fs.create_excl(path, 0o666) {
        Ok(file) => {
            file.close()?;
            Ok(Created::New)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            if fs.stat(path)?.is_dir() {
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "a folder with this name exists",
                ))
            } else {
                Ok(Created::Existing)
            }
        }
        Err(e) => Err(e),
    }
}

fn make_parents(fs: &dyn Fs, dir: &Path) -> io::Result<()> {
    if fs.stat(dir).is_ok() {
        return Ok(());
    }
    if let Some(up) = dir.parent() {
        make_parents(fs, up)?;
    }
    match fs.mkdir(dir, 0o777) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
        _ => Ok(()),
    }
}

/// Ask for a name and create + edit the file in the active panel's directory.
pub fn present(ctx: &Rc<AppContext>) {
    let panel = ctx.active_controller();
    let Some(dir) = panel.location().local_path().map(Path::to_path_buf) else {
        ui::message(
            ctx,
            tr("New file"),
            tr("Files can only be created in local directories."),
        );
        return;
    };

    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Edit new file"));
    dialog.set_content_width(440);
    let column = modal::padded_column(8);
    column.append(&ui::label(tr("File name:")));
    let entry = gtk::Entry::new();
    entry.set_widget_name("ofm-newfile-name");
    entry.update_property(&[gtk::accessible::Property::Label(tr("File name"))]);
    column.append(&entry);
    column.append(&ui::caption(tr(
        "The file is created empty and opened in the editor. An existing file is opened as it is.",
    )));
    let error = ui::error_label();
    error.set_widget_name("ofm-newfile-error");
    column.append(&error);
    let cancel = ui::button(tr("Cancel"));
    let create_btn = ui::button(tr("_Edit"));
    create_btn.set_widget_name("ofm-newfile-run");
    create_btn.add_css_class("suggested-action");
    column.append(&ui::button_row(&[&cancel, &create_btn]));
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&entry));

    let submit: Rc<dyn Fn() -> bool> = {
        let (ctx, entry, error) = (Rc::clone(ctx), entry.clone(), error.clone());
        Rc::new(move || match build_path(&dir, &entry.text(), &ui::home()) {
            Ok(path) => {
                create_and_edit(&ctx, path);
                true
            }
            Err(msg) => {
                ui::set_error(&error, Some(&msg));
                false
            }
        })
    };
    {
        let error = error.clone();
        entry.connect_changed(move |_| ui::set_error(&error, None));
    }
    let weak = dialog.downgrade();
    {
        let (submit, weak) = (Rc::clone(&submit), weak.clone());
        create_btn.connect_clicked(move |_| {
            if submit()
                && let Some(d) = weak.upgrade()
            {
                d.close();
            }
        });
    }
    cancel.connect_clicked(move |_| {
        if let Some(d) = weak.upgrade() {
            d.close();
        }
    });
    let ctx_ok = Rc::clone(ctx);
    modal::present(ctx, &dialog, move || {
        if ui::activate_focused_button(&ctx_ok) {
            return false;
        }
        submit()
    });
}

/// Create `path` on a worker, then (main thread) reveal it in the panel and open the editor.
fn create_and_edit(ctx: &Rc<AppContext>, path: PathBuf) {
    let worker_path = path.clone();
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let fs: Arc<dyn Fs> = Arc::new(RealFs);
        let _ = tx.send_blocking(create(fs.as_ref(), &worker_path));
    });
    let weak = Rc::downgrade(ctx);
    wiring::attach(
        rx,
        move |result| {
            let Some(ctx) = weak.upgrade() else {
                return ControlFlow::Break(());
            };
            match result {
                Ok(_) => {
                    let panel = ctx.active_controller();
                    if let (Some(name), Some(parent)) = (path.file_name(), path.parent())
                        && panel.location().local_path() == Some(parent)
                    {
                        panel.focus_name(name.to_owned());
                    }
                    editor::open_in_editor(&ctx, &path);
                }
                Err(e) => ui::message(
                    &ctx,
                    tr("New file"),
                    &format!("{}: {e}", path.to_string_lossy()),
                ),
            }
            ControlFlow::Break(())
        },
        || {},
    );
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods // test fixtures on the test thread
)]
mod tests {
    use super::*;

    #[test]
    fn builds_paths_relative_to_the_panel() {
        let (dir, home) = (Path::new("/w"), Path::new("/h"));
        assert_eq!(
            build_path(dir, "n.txt", home),
            Ok(PathBuf::from("/w/n.txt"))
        );
        assert_eq!(
            build_path(dir, "sub/n.txt", home),
            Ok(PathBuf::from("/w/sub/n.txt"))
        );
        assert_eq!(build_path(dir, "~/n", home), Ok(PathBuf::from("/h/n")));
        assert!(build_path(dir, "", home).is_err());
        assert!(build_path(dir, "  ", home).is_err());
        assert!(build_path(dir, "dir/", home).is_err());
        assert!(build_path(dir, ".", home).is_err());
    }

    #[test]
    fn creates_an_empty_file_with_parents_and_keeps_existing_content() {
        let tmp = ofm_testkit::temp::TempDir::new().unwrap();
        let fs = RealFs;
        let p = tmp.path().join("a/b/new.txt");
        assert_eq!(create(&fs, &p).unwrap(), Created::New);
        assert_eq!(std::fs::read(&p).unwrap(), b"");
        std::fs::write(&p, b"keep me").unwrap();
        assert_eq!(create(&fs, &p).unwrap(), Created::Existing);
        assert_eq!(std::fs::read(&p).unwrap(), b"keep me");
    }

    #[test]
    fn a_folder_with_the_name_is_an_error() {
        let tmp = ofm_testkit::temp::TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("d")).unwrap();
        assert!(create(&RealFs, &tmp.path().join("d")).is_err());
    }

    #[test]
    fn non_utf8_names_work() {
        use std::os::unix::ffi::OsStrExt;
        let tmp = ofm_testkit::temp::TempDir::new().unwrap();
        let p = tmp
            .path()
            .join(std::ffi::OsStr::from_bytes(b"\xff\xfe.txt"));
        assert_eq!(create(&RealFs, &p).unwrap(), Created::New);
        assert!(p.exists());
    }
}
