//! "Panelize command" (`search.panelize_command`): a dialog for a shell command line and a small
//! progress dialog while it runs. The command runs on a worker in the active panel's directory;
//! its output (one path per line, or NUL separated) becomes a list panel.

use std::cell::Cell;
use std::ops::ControlFlow;
use std::rc::Rc;

use adw::prelude::*;
use ofm_config::history::push_recent;
use ofm_core::CancelToken;

use super::panelize::{self, CommandMsg};
use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::ui;
use crate::navigation::modal;
use crate::navigation::store::HistoryStore;
use crate::wiring;

/// Open the dialog.
pub fn open(ctx: &Rc<AppContext>) {
    let cwd = panelize::command_cwd(&ctx.active_controller().location());
    let history = HistoryStore::ensure(ctx).snapshot().panelize_commands;
    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Panelize command"));
    dialog.set_content_width(520);
    let column = modal::padded_column(8);
    column.append(&ui::label(&format!(
        "{} {}",
        tr("Command to run in"),
        cwd.to_string_lossy()
    )));
    let entry = gtk::Entry::new();
    entry.set_widget_name("ofm-panelize-command");
    entry.set_hexpand(true);
    entry.set_text(history.first().map_or("", String::as_str));
    entry.update_property(&[gtk::accessible::Property::Label(tr("Shell command"))]);
    column.append(&entry);
    column.append(&ui::caption(tr(
        "Its output lines (file names) become the panel. Example: git ls-files",
    )));
    let nul = gtk::CheckButton::with_mnemonic(tr("_NUL-separated output (find -print0)"));
    nul.set_widget_name("ofm-panelize-nul");
    nul.update_property(&[gtk::accessible::Property::Label(tr(
        "NUL-separated output",
    ))]);
    column.append(&nul);
    crate::dialogs::find::attach_history(&entry, Rc::new(history));
    let cancel = ui::button(tr("Cancel"));
    let run = ui::button(tr("_Panelize"));
    run.set_widget_name("ofm-panelize-run");
    run.add_css_class("suggested-action");
    column.append(&ui::button_row(&[&cancel, &run]));
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&entry));

    let confirm: Rc<dyn Fn() -> bool> = {
        let (ctx, entry, nul) = (Rc::clone(ctx), entry.clone(), nul.clone());
        Rc::new(move || {
            let command = entry.text().trim().to_owned();
            if command.is_empty() {
                entry.add_css_class("error");
                return false;
            }
            run_command(&ctx, command, nul.is_active());
            true
        })
    };
    {
        let (confirm, weak) = (Rc::clone(&confirm), dialog.downgrade());
        run.connect_clicked(move |_| {
            if confirm()
                && let Some(d) = weak.upgrade()
            {
                d.close();
            }
        });
    }
    {
        let weak = dialog.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(d) = weak.upgrade() {
                d.close();
            }
        });
    }
    let ctx2 = Rc::clone(ctx);
    modal::present(ctx, &dialog, move || {
        if ui::activate_focused_button(&ctx2) {
            return false;
        }
        confirm()
    });
    entry.select_region(0, -1);
}

/// Run `command` in the active panel's directory and show its output as a list panel. Shows a
/// progress dialog (Esc / Cancel kill the command); returns at once.
pub fn run_command(ctx: &Rc<AppContext>, command: String, nul: bool) {
    HistoryStore::ensure(ctx).update(|h| push_recent(&mut h.panelize_commands, &command));
    let cwd = panelize::command_cwd(&ctx.active_controller().location());
    let cancel = CancelToken::new();
    let title = format!("{} {command}", tr("Panelize:"));
    let rx = panelize::spawn_command(ctx, cwd, command.clone(), nul, title, cancel.clone());

    let finished = Rc::new(Cell::new(false));
    let count = gtk::Label::new(Some("0"));
    count.set_xalign(0.0);
    count.update_property(&[gtk::accessible::Property::Label(tr("Paths read"))]);
    let dialog = progress_dialog(ctx, &command, &count, &cancel, &finished);

    let weak = Rc::downgrade(ctx);
    wiring::attach(
        rx,
        move |msg| {
            let Some(ctx) = weak.upgrade() else {
                return ControlFlow::Break(());
            };
            match msg {
                CommandMsg::Count(n) => {
                    count.set_text(&n.to_string());
                    ControlFlow::Continue(())
                }
                CommandMsg::Done(result) => {
                    finished.set(true);
                    if let Some(d) = &dialog {
                        d.close();
                    }
                    finish(&ctx, result);
                    ControlFlow::Break(())
                }
            }
        },
        || {},
    );
}

fn finish(ctx: &Rc<AppContext>, result: Result<panelize::CommandDone, String>) {
    let done = match result {
        Ok(d) => d,
        Err(msg) => {
            ui::message(ctx, tr("Panelize command"), &msg);
            return;
        }
    };
    if done.summary.cancelled {
        super::notify(ctx, tr("Panelize command cancelled."));
        return;
    }
    let text = panelize::command_message(done.list.entries.len(), done.missing, &done.summary);
    let stderr = done.summary.stderr_tail.trim().to_owned();
    let failed = !matches!(done.summary.exit_code, Some(0) | None);
    if done.list.entries.is_empty() {
        super::notify(
            ctx,
            &format!("{} ({text})", tr("The command printed no existing file.")),
        );
    } else {
        panelize::show_list(ctx, done.list);
        super::notify(ctx, &text);
    }
    if failed && !stderr.is_empty() {
        ui::message(
            ctx,
            tr("Panelize command"),
            &format!("{}\n\n{stderr}", tr("The command reported errors:")),
        );
    }
}

/// The dialog shown while the command runs; closing it (Esc, Cancel) cancels the command.
fn progress_dialog(
    ctx: &Rc<AppContext>,
    command: &str,
    count: &gtk::Label,
    cancel: &CancelToken,
    finished: &Rc<Cell<bool>>,
) -> Option<adw::Dialog> {
    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Panelize command"));
    dialog.set_content_width(420);
    let column = modal::padded_column(8);
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top.append(&spinner);
    let what = ui::label(command);
    what.set_selectable(false);
    top.append(&what);
    column.append(&top);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&ui::caption(tr("Paths read:")));
    row.append(count);
    column.append(&row);
    let stop = ui::button(tr("_Cancel"));
    stop.set_widget_name("ofm-panelize-cancel");
    column.append(&ui::button_row(&[&stop]));
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&stop));
    {
        let weak = dialog.downgrade();
        stop.connect_clicked(move |_| {
            if let Some(d) = weak.upgrade() {
                d.close();
            }
        });
    }
    {
        let (cancel, finished) = (cancel.clone(), Rc::clone(finished));
        dialog.connect_closed(move |_| {
            if !finished.get() {
                cancel.cancel(); // Esc / Cancel / window close: kill the command
            }
        });
    }
    let ctx2 = Rc::clone(ctx);
    // Enter presses the focused Cancel button; nothing else to confirm.
    modal::present(ctx, &dialog, move || {
        ui::activate_focused_button(&ctx2);
        false
    })
    .then_some(dialog)
}
