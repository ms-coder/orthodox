//! Quit with running jobs (F15: "quitting with running jobs asks"). The main window's close
//! handler calls [`allow_close`] first; it returns false while a question is shown and while
//! cancelled jobs are still cleaning up.
//!
//! Data safety: cancelling only *asks* the workers to stop; each one then removes its
//! `*.ofm-part-*` temp file before it reports `Finished`. Closing the window right away would end
//! the process first and leave the temp files behind, so [`cancel_then_close`] waits (polling the
//! job table on the main loop, at most [`CANCEL_WAIT`]) until every job has finished.

use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;

use super::ui;
use crate::context::AppContext;
use crate::i18n::tr;
use crate::navigation::modal;

/// Longest wait for cancelled jobs to clean up before the window closes anyway.
pub const CANCEL_WAIT: Duration = Duration::from_secs(15);

/// Cancel every job, wait until all of them reported `Finished` (or [`CANCEL_WAIT`] passed), then
/// close `window` for good.
fn cancel_then_close(ctx: &Rc<AppContext>, window: &gtk::Window) {
    let Some(ctrl) = super::controller(ctx) else {
        window.close();
        return;
    };
    ctrl.cancel_all();
    ctrl.set_quitting(true);
    let deadline = Instant::now() + CANCEL_WAIT;
    let (weak, window) = (Rc::downgrade(&ctrl), window.clone());
    let finish = move || {
        if let Some(ctrl) = weak.upgrade() {
            if ctrl.has_unfinished() {
                tracing::warn!("closing with jobs that did not stop in time");
            }
            ctrl.set_quit_confirmed();
            ctrl.set_quitting(false);
        }
        window.close();
    };
    if !ctrl.has_unfinished() {
        finish();
        return;
    }
    let weak = Rc::downgrade(&ctrl);
    let finish = std::cell::RefCell::new(Some(finish));
    glib::timeout_add_local(Duration::from_millis(50), move || {
        let busy = weak.upgrade().is_some_and(|c| c.has_unfinished());
        if busy && Instant::now() < deadline {
            return glib::ControlFlow::Continue;
        }
        if let Some(f) = finish.borrow_mut().take() {
            f();
        }
        glib::ControlFlow::Break
    });
}

/// Whether the window may close now. With unfinished jobs and `[general] confirm_quit_with_jobs`
/// a dialog asks first (it starts on "Keep running": Enter and Esc both keep running); on confirmation the
/// jobs are cancelled, and once they have cleaned up `window.close()` is called again. Without the
/// confirmation setting the jobs are cancelled without asking (and waited for the same way).
pub fn allow_close(ctx: &Rc<AppContext>, window: &impl IsA<gtk::Window>) -> bool {
    let Some(ctrl) = super::controller(ctx) else {
        return true;
    };
    if ctrl.quit_confirmed() {
        return true;
    }
    if ctrl.quitting() {
        return false; // still waiting for the cancelled jobs
    }
    if !ctrl.has_unfinished() {
        return true;
    }
    if !ctx.config.borrow().general.confirm_quit_with_jobs {
        cancel_then_close(ctx, window.as_ref());
        return false;
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
    keep.set_widget_name("ofm-quit-keep");
    dialog.set_focus(Some(&keep));

    let confirm: Rc<dyn Fn()> = {
        let (ctx, window) = (Rc::clone(ctx), window.as_ref().clone());
        Rc::new(move || cancel_then_close(&ctx, &window))
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
    // Enter presses the focused button; with none focused it keeps running.
    let on_ok = move || {
        ui::activate_focused_button(&ctx_ok);
        true
    };
    modal::present(ctx, &dialog, on_ok);
    false
}
