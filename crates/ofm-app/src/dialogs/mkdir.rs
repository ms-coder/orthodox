//! Make-directory dialog (T3.3 owns this file; F13, F7): name entry (may contain `/` for nested
//! directories), submits `JobSpec::Mkdir`; after the job the panel cursor goes to the new
//! directory (`Report::created`, see `jobs::controller`). Runs in parallel mode: creating a
//! folder must not wait behind a long copy.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use ofm_jobs::{JobSpec, RunMode};

use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::targets::resolve_path;
use crate::jobs::{self, SubmitOpts, ui};
use crate::navigation::modal;

/// Directory to create for the text typed into the dialog; `Err` is a message for the dialog.
/// Relative text is taken relative to `dir`, `~` is expanded.
pub fn build_mkdir(dir: &Path, text: &str, home: &Path) -> Result<PathBuf, String> {
    let path =
        resolve_path(text, dir, home).ok_or_else(|| tr("Enter a folder name.").to_owned())?;
    if path == dir {
        return Err(tr("Enter a folder name.").to_owned());
    }
    Ok(path)
}

/// Ask for a name and create the directory in the active panel.
pub fn present(ctx: &Rc<AppContext>) {
    let panel = ctx.active_controller();
    let Some(dir) = panel.location().local_path().map(Path::to_path_buf) else {
        ui::message(
            ctx,
            tr("Create folder"),
            tr("Folders can only be created in local directories."),
        );
        return;
    };
    let Some(controller) = jobs::controller(ctx) else {
        return;
    };

    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Create folder"));
    dialog.set_content_width(440);
    let column = modal::padded_column(8);
    column.append(&ui::label(tr("Folder name:")));
    let entry = gtk::Entry::new();
    entry.set_widget_name("ofm-mkdir-name");
    entry.update_property(&[gtk::accessible::Property::Label(tr("Folder name"))]);
    column.append(&entry);
    column.append(&ui::caption(tr(
        "Use / to create nested folders (a/b/c). Existing folders are kept.",
    )));
    let error = ui::error_label();
    error.set_widget_name("ofm-mkdir-error");
    column.append(&error);
    let cancel = ui::button(tr("Cancel"));
    let create = ui::button(tr("_Create"));
    create.set_widget_name("ofm-mkdir-run");
    create.add_css_class("suggested-action");
    column.append(&ui::button_row(&[&cancel, &create]));
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&entry));

    let submit: Rc<dyn Fn() -> bool> = {
        let (ctx, entry, error) = (Rc::clone(ctx), entry.clone(), error.clone());
        Rc::new(
            move || match build_mkdir(&dir, &entry.text(), &ui::home()) {
                Ok(path) => {
                    controller.submit_with(
                        &ctx,
                        JobSpec::Mkdir { path },
                        RunMode::Parallel,
                        SubmitOpts::default(),
                    );
                    true
                }
                Err(msg) => {
                    ui::set_error(&error, Some(&msg));
                    false
                }
            },
        )
    };
    {
        let error = error.clone();
        entry.connect_changed(move |_| ui::set_error(&error, None));
    }
    let weak = dialog.downgrade();
    {
        let (submit, weak) = (Rc::clone(&submit), weak.clone());
        create.connect_clicked(move |_| {
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn builds_paths_relative_to_the_panel() {
        let (dir, home) = (Path::new("/w"), Path::new("/h"));
        assert_eq!(build_mkdir(dir, "new", home), Ok(PathBuf::from("/w/new")));
        assert_eq!(
            build_mkdir(dir, "a/b/c", home),
            Ok(PathBuf::from("/w/a/b/c"))
        );
        assert_eq!(build_mkdir(dir, "~/x", home), Ok(PathBuf::from("/h/x")));
        assert_eq!(
            build_mkdir(dir, "/abs/d", home),
            Ok(PathBuf::from("/abs/d"))
        );
        assert!(build_mkdir(dir, "", home).is_err());
        assert!(build_mkdir(dir, ".", home).is_err());
    }
}
