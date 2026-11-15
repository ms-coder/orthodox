//! Create-link dialog (T3.3 owns this file; F18, Alt+F6): symbolic (absolute or relative target via
//! `ofm_jobs::ops::link::relative_target`) or hard link; submits `JobSpec::Link`. Works on the
//! item under the cursor; the link path defaults to the other panel's directory.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use ofm_core::{Entry, EntryKind, Location};
use ofm_jobs::ops::link::relative_target;
use ofm_jobs::{JobSpec, LinkKind, RunMode};

use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::targets::resolve_path_with_origin;
use crate::jobs::{self, SubmitOpts, ui};
use crate::navigation::modal;

/// What kind of link the user picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkChoice {
    /// Symlink storing the absolute target path.
    SymbolicAbsolute,
    /// Symlink storing the path relative to the link's directory.
    SymbolicRelative,
    /// Hard link.
    Hard,
}

/// Validate the dialog and build the job. `link_text` is resolved against `base`; a trailing `/`
/// means "inside that folder, named like the target". `target_is_dir` rejects hard links to
/// folders up front (the engine would refuse them too). `link_origin` is the path whose lossy
/// rendering the field was filled with: while the text still starts with it, its original
/// (possibly non-UTF-8) bytes are used.
pub fn build_link(
    target: &Path,
    target_is_dir: bool,
    link_text: &str,
    link_origin: Option<&Path>,
    base: &Path,
    home: &Path,
    choice: LinkChoice,
) -> Result<JobSpec, String> {
    let mut link = resolve_path_with_origin(link_text, link_origin, base, home)
        .ok_or_else(|| tr("Enter the path of the link.").to_owned())?;
    if link_text.trim().ends_with('/')
        && let Some(name) = target.file_name()
    {
        link.push(name);
    }
    if link == target {
        return Err(tr("The link cannot replace its own target.").to_owned());
    }
    if choice == LinkChoice::Hard && target_is_dir {
        return Err(tr("Hard links to folders are not possible. Use a symbolic link.").to_owned());
    }
    let (stored, kind) = match choice {
        LinkChoice::SymbolicAbsolute => (target.to_path_buf(), LinkKind::Symbolic),
        LinkChoice::SymbolicRelative => (relative_target(&link, target), LinkKind::Symbolic),
        LinkChoice::Hard => (target.to_path_buf(), LinkKind::Hard),
    };
    Ok(JobSpec::Link {
        target: stored,
        link,
        kind,
    })
}

/// The cursor entry as (real path, is directory).
fn cursor_target(ctx: &AppContext) -> Option<(Entry, PathBuf)> {
    let panel = ctx.active_controller();
    let dir = panel.location();
    let entry = panel
        .model()
        .cursor_entry()
        .filter(|e| e.name != "..")
        .cloned()?;
    let path = match (&dir, &entry.real_path) {
        (Location::List { .. }, Some(p)) => p.clone(),
        (Location::Local(d), _) => d.join(&entry.name),
        _ => return None,
    };
    Some((entry, path))
}

/// Ask for link path/kind for the item under the cursor and create it.
pub fn present(ctx: &Rc<AppContext>) {
    let Some((entry, target)) = cursor_target(ctx) else {
        return;
    };
    let Some(controller) = jobs::controller(ctx) else {
        return;
    };
    let name = entry.name.to_string_lossy().into_owned();
    let src_dir = target.parent().map(Path::to_path_buf).unwrap_or_default();
    let other_dir = ctx
        .other_controller()
        .location()
        .local_path()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| src_dir.clone());
    let mut default_link = other_dir.join(&entry.name);
    if default_link == target {
        let mut linked = entry.name.clone();
        linked.push(" link");
        default_link = other_dir.join(linked);
    }
    let is_dir = entry.kind == EntryKind::Dir;

    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Create link"));
    dialog.set_content_width(520);
    let column = modal::padded_column(8);
    column.append(&ui::label(&format!("{} '{name}'", tr("Create a link to"))));
    column.append(&ui::label(tr("Link path:")));
    let path_entry = gtk::Entry::new();
    path_entry.set_widget_name("ofm-link-path");
    path_entry.update_property(&[gtk::accessible::Property::Label(tr("Link path"))]);
    path_entry.set_text(&default_link.to_string_lossy());
    column.append(&path_entry);

    let absolute = gtk::CheckButton::with_mnemonic(tr("Symbolic link, _absolute target"));
    absolute.set_widget_name("ofm-link-absolute");
    let relative = gtk::CheckButton::with_mnemonic(tr("Symbolic link, _relative target"));
    relative.set_widget_name("ofm-link-relative");
    relative.set_group(Some(&absolute));
    let hard = gtk::CheckButton::with_mnemonic(tr("_Hard link"));
    hard.set_widget_name("ofm-link-hard");
    hard.set_group(Some(&absolute));
    absolute.set_active(true);
    hard.set_sensitive(!is_dir);
    for c in [&absolute, &relative, &hard] {
        column.append(c);
    }
    let error = ui::error_label();
    error.set_widget_name("ofm-link-error");
    column.append(&error);
    let cancel = ui::button(tr("Cancel"));
    let create = ui::button(tr("_Create"));
    create.set_widget_name("ofm-link-run");
    create.add_css_class("suggested-action");
    column.append(&ui::button_row(&[&cancel, &create]));
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&path_entry));
    path_entry.select_region(0, -1);

    let submit: Rc<dyn Fn() -> bool> = {
        let (ctx, path_entry, error) = (Rc::clone(ctx), path_entry.clone(), error.clone());
        let (relative, hard) = (relative.clone(), hard.clone());
        let link_origin = default_link.clone();
        Rc::new(move || {
            let choice = if hard.is_active() {
                LinkChoice::Hard
            } else if relative.is_active() {
                LinkChoice::SymbolicRelative
            } else {
                LinkChoice::SymbolicAbsolute
            };
            match build_link(
                &target,
                is_dir,
                &path_entry.text(),
                Some(&link_origin),
                &src_dir,
                &ui::home(),
                choice,
            ) {
                Ok(spec) => {
                    controller.submit_with(&ctx, spec, RunMode::Parallel, SubmitOpts::default());
                    true
                }
                Err(msg) => {
                    ui::set_error(&error, Some(&msg));
                    false
                }
            }
        })
    };
    {
        let error = error.clone();
        path_entry.connect_changed(move |_| ui::set_error(&error, None));
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

    fn link(spec: JobSpec) -> (PathBuf, PathBuf, LinkKind) {
        match spec {
            JobSpec::Link { target, link, kind } => (target, link, kind),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn absolute_relative_and_hard() {
        let (t, base, home) = (Path::new("/a/x/file"), Path::new("/a/x"), Path::new("/h"));
        let abs = build_link(
            t,
            false,
            "/a/y/l",
            None,
            base,
            home,
            LinkChoice::SymbolicAbsolute,
        );
        assert_eq!(
            link(abs.expect("valid")),
            ("/a/x/file".into(), "/a/y/l".into(), LinkKind::Symbolic)
        );
        let rel = build_link(
            t,
            false,
            "/a/y/l",
            None,
            base,
            home,
            LinkChoice::SymbolicRelative,
        );
        assert_eq!(
            link(rel.expect("valid")),
            ("../x/file".into(), "/a/y/l".into(), LinkKind::Symbolic)
        );
        let hard = build_link(t, false, "l", None, base, home, LinkChoice::Hard);
        assert_eq!(
            link(hard.expect("valid")),
            ("/a/x/file".into(), "/a/x/l".into(), LinkKind::Hard)
        );
    }

    #[test]
    fn trailing_slash_means_inside_the_folder() {
        let (t, base, home) = (Path::new("/a/file"), Path::new("/a"), Path::new("/h"));
        let spec = build_link(
            t,
            false,
            "/b/",
            None,
            base,
            home,
            LinkChoice::SymbolicAbsolute,
        );
        assert_eq!(link(spec.expect("valid")).1, PathBuf::from("/b/file"));
    }

    #[test]
    fn non_utf8_paths_keep_their_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let raw = |b: &[u8]| PathBuf::from(std::ffi::OsStr::from_bytes(b));
        let target = raw(b"/a/t\xe9");
        let (base, home) = (Path::new("/a"), Path::new("/h"));
        // Default link path (other folder + the target's name), untouched in the field.
        let default_link = raw(b"/b\xff/t\xe9");
        let shown = default_link.to_string_lossy().into_owned();
        let spec = build_link(
            &target,
            false,
            &shown,
            Some(&default_link),
            base,
            home,
            LinkChoice::SymbolicAbsolute,
        );
        assert_eq!(
            link(spec.expect("valid")),
            (target.clone(), default_link.clone(), LinkKind::Symbolic)
        );
        // A trailing slash after a non-UTF-8 folder: the link is named like the target.
        let folder = raw(b"/b\xff");
        let text = format!("{}/", folder.to_string_lossy());
        let spec = build_link(
            &target,
            false,
            &text,
            Some(&folder),
            base,
            home,
            LinkChoice::Hard,
        );
        assert_eq!(link(spec.expect("valid")).1, default_link);
        // Relative target computed from the exact bytes.
        let spec = build_link(
            &target,
            false,
            &shown,
            Some(&default_link),
            base,
            home,
            LinkChoice::SymbolicRelative,
        );
        let (stored, l, _) = link(spec.expect("valid"));
        assert_eq!(l, default_link);
        assert_eq!(stored, raw(b"../a/t\xe9"));
    }

    #[test]
    fn refuses_nonsense() {
        let (t, base, home) = (Path::new("/a/d"), Path::new("/a"), Path::new("/h"));
        assert!(build_link(t, true, "", None, base, home, LinkChoice::SymbolicAbsolute).is_err());
        assert!(build_link(t, true, "d", None, base, home, LinkChoice::SymbolicAbsolute).is_err());
        assert!(build_link(t, true, "l", None, base, home, LinkChoice::Hard).is_err());
    }
}
