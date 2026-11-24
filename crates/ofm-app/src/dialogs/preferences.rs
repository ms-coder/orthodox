//! The Preferences dialog (`app.preferences`, T8.11): minimal on purpose — the paths of the
//! config files with buttons to open them in the editor / the file manager and one to reload the
//! configuration. No settings UI: v1.0 keeps editing `config.toml` in the user's editor (F29).
//! The problems of the last config/keymap/theme load are listed here (and in Help).
//!
//! State: [`ConfigWarnings`] in `ctx.ext`, written at startup (`app::build_context`) and after
//! every live reload (`chrome::apply_reloaded`).

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use ofm_config::Warning;
use ofm_core::cmd;

use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::ui;
use crate::navigation::modal;

/// Warnings of the last config/keymap/theme load (state in `ctx.ext`).
#[derive(Default)]
pub struct ConfigWarnings(RefCell<Vec<Warning>>);

/// Create the state.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(ConfigWarnings::default()));
}

/// Replace the stored warnings (startup and `app.reload_config`).
pub fn record(ctx: &AppContext, warnings: Vec<Warning>) {
    if let Some(state) = ctx.ext.get::<ConfigWarnings>() {
        *state.0.borrow_mut() = warnings;
    }
}

/// The stored warnings as display lines "file: message" (Help shows the same list).
pub fn warning_lines(ctx: &AppContext) -> Vec<String> {
    ctx.ext
        .get::<ConfigWarnings>()
        .map(|state| {
            state
                .0
                .borrow()
                .iter()
                .map(|w| {
                    format!(
                        "{}: {}",
                        w.file
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                        w.message
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The status-line hint for `n` problems ("N problems in config (see Help > Config warnings)").
pub fn problems_hint(n: usize) -> String {
    let text = if n == 1 {
        tr("problem in config (see Help > Config warnings)")
    } else {
        tr("problems in config (see Help > Config warnings)")
    };
    format!("{n} {text}")
}

/// Build and show the dialog over the main window.
pub fn present(ctx: &Rc<AppContext>) {
    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Preferences"));
    dialog.set_content_width(540);
    let column = modal::padded_column(8);

    // File paths.
    column.append(&ui::label(tr("Configuration files:")));
    let paths = ctx.paths.clone();
    for (what, path) in [
        (tr("config.toml"), paths.config_file()),
        (tr("keymap.toml (own bindings)"), paths.keymap_file()),
        (tr("themes directory"), paths.themes_dir()),
    ] {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let name = ui::caption(what);
        let path_label = gtk::Label::new(Some(&path.to_string_lossy()));
        path_label.set_xalign(0.0);
        path_label.set_hexpand(true);
        path_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        path_label.set_selectable(true);
        path_label.update_property(&[gtk::accessible::Property::Label(&format!(
            "{what}: {}",
            path.to_string_lossy()
        ))]);
        row.append(&name);
        row.append(&path_label);
        column.append(&row);
    }

    // Buttons.
    let edit = ui::button(tr("_Open config.toml in editor"));
    edit.set_widget_name("ofm-prefs-edit");
    let folder = ui::button(tr("Open _config folder"));
    folder.set_widget_name("ofm-prefs-folder");
    let reload = ui::button(tr("_Reload configuration"));
    reload.set_widget_name("ofm-prefs-reload");
    column.append(&ui::button_row(&[&edit, &folder, &reload]));

    // Problems of the last load.
    let problems = warning_lines(ctx);
    if !problems.is_empty() {
        column.append(&ui::label(&format!(
            "{} {}",
            problems.len(),
            tr("problems in the configuration:")
        )));
        let list = ui::label(&problems.join("\n"));
        list.set_selectable(true);
        list.set_wrap(true);
        column.append(&list);
    }

    let close = ui::button(tr("_Close"));
    close.set_widget_name("ofm-prefs-close");
    column.append(&ui::button_row(&[&close]));
    dialog.set_child(Some(&modal::with_header(&column)));

    {
        let (ctx_edit, path) = (Rc::clone(ctx), paths.config_file());
        edit.connect_clicked(move |_| {
            crate::desktop::editor::open_in_editor(&ctx_edit, &path);
        });
    }
    {
        let ctx_folder = Rc::clone(ctx);
        let config_dir = paths.config_dir.clone();
        folder.connect_clicked(move |_| {
            open_folder(&ctx_folder, &config_dir);
        });
    }
    {
        let weak = dialog.downgrade();
        let ctx_reload = Rc::clone(ctx);
        reload.connect_clicked(move |_| {
            ctx_reload.execute(cmd::APP_RELOAD_CONFIG);
            if let Some(d) = weak.upgrade() {
                d.close();
            }
        });
    }
    {
        let weak = dialog.downgrade();
        close.connect_clicked(move |_| {
            if let Some(d) = weak.upgrade() {
                d.close();
            }
        });
    }
    modal::present(ctx, &dialog, || true);
}

/// Open `dir` in the desktop file manager (`gtk::FileLauncher`, like `desktop::open`); a failure
/// is only logged (there may simply be no file manager installed).
fn open_folder(ctx: &Rc<AppContext>, dir: &std::path::Path) {
    let launcher = gtk::FileLauncher::new(Some(&gtk::gio::File::for_path(dir)));
    let parent = ctx.window().map(|w| w.window.clone());
    let dir = dir.to_path_buf();
    gtk::glib::spawn_future_local(async move {
        if let Err(e) = launcher.launch_future(parent.as_ref()).await {
            tracing::warn!(dir = %dir.display(), error = %e, "could not open the config folder");
        }
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn hint_counts() {
        assert_eq!(problems_hint(1), "1 problem in config (see Help > Config warnings)");
        assert_eq!(
            problems_hint(3),
            "3 problems in config (see Help > Config warnings)"
        );
    }

    #[test]
    fn warning_lines_name_the_file() {
        let w = Warning {
            file: Some(std::path::PathBuf::from("/x/config.toml")),
            message: "unknown key".into(),
        };
        // (pure formatting check through a one-element list)
        let state = ConfigWarnings(RefCell::new(vec![w]));
        let lines: Vec<String> = state
            .0
            .borrow()
            .iter()
            .map(|w| {
                format!(
                    "{}: {}",
                    w.file
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    w.message
                )
            })
            .collect();
        assert_eq!(lines, vec!["/x/config.toml: unknown key".to_owned()]);
    }
}
