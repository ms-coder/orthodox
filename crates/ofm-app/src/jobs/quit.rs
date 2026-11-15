//! Quit with running jobs (F15: "quitting with running jobs asks"). The main window's close
//! handler calls [`allow_close`] first; it returns false while a question is shown.

use std::rc::Rc;

use adw::prelude::*;

use super::ui;
use crate::context::AppContext;
use crate::i18n::tr;
use crate::navigation::modal;

/// Whether the window may close now. With unfinished jobs and `[general] confirm_quit_with_jobs`
/// a dialog asks first (Enter = quit and cancel the jobs, Esc = keep running); on confirmation the
/// jobs are cancelled and `window.close()` is called again. Without the confirmation setting the
/// jobs are cancelled silently.
pub fn allow_close(ctx: &Rc<AppContext>, window: &impl IsA<gtk::Window>) -> bool {
    let Some(ctrl) = super::controller(ctx) else {
        return true;
    };
    if !ctrl.has_unfinished() {
        return true;
    }
    if ctrl.quit_confirmed() {
        return true;
    }
    if !ctx.config.borrow().general.confirm_quit_with_jobs {
        ctrl.cancel_all();
        return true;
    }

    let n = ctrl.unfinished_count();
    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Quit OFM?"));
    dialog.set_content_width(460);
    let column = modal::padded_column(10);
    let what = if n == 1 {
        tr("A background job is still running.").to_owned()
    } else {
        format!("{n} {}", tr("background jobs are still running."))
    };
    column.append(
        &gtk::Label::builder()
            .label(what)
            .xalign(0.0)
            .css_classes(["title-4"])
            .build(),
    );
    column.append(&ui::caption(tr(
        "Quitting cancels them. Files that are already complete stay; partially written files are removed.",
    )));
    let keep = ui::button(tr("Keep _running"));
    let quit = ui::button(tr("_Quit and cancel jobs"));
    quit.set_widget_name("ofm-quit-confirm");
    quit.add_css_class("destructive-action");
    column.append(&ui::button_row(&[&keep, &quit]));
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&quit));

    let confirm: Rc<dyn Fn()> = {
        let (ctx, window) = (Rc::clone(ctx), window.as_ref().clone());
        Rc::new(move || {
            if let Some(ctrl) = super::controller(&ctx) {
                ctrl.cancel_all();
                ctrl.set_quit_confirmed();
            }
            window.close();
        })
    };
    let weak = dialog.downgrade();
    {
        let (confirm, weak) = (Rc::clone(&confirm), weak.clone());
        quit.connect_clicked(move |_| {
            if let Some(d) = weak.upgrade() {
                d.close();
            }
            confirm();
        });
    }
    keep.connect_clicked(move |_| {
        if let Some(d) = weak.upgrade() {
            d.close();
        }
    });
    let ctx_ok = Rc::clone(ctx);
    let on_ok = {
        let confirm = Rc::clone(&confirm);
        move || {
            if !ui::activate_focused_button(&ctx_ok) {
                confirm();
            }
            true
        }
    };
    modal::present(ctx, &dialog, on_ok);
    false
}
