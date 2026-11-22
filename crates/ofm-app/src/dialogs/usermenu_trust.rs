//! Trust prompt for a folder's `.ofm-menu.toml` (F28): a menu file can run any command, so the
//! first time (and whenever the content changes) the user sees the file and its sha256 and
//! decides: Show contents / Trust / Ignore. Esc and closing the window mean Ignore; the
//! initial focus is on "Show contents", so a reflex Enter never trusts anything.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use ofm_config::menu::local_menu_path;

use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::ui;
use crate::navigation::modal;

/// What the user decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Remember this file content as trusted.
    Trust,
    /// Do not use the local menu now.
    Ignore,
}

type DecisionFn = Box<dyn FnOnce(Decision)>;

/// What to review.
#[derive(Debug, Clone)]
pub struct Review {
    /// The folder with the menu file.
    pub dir: PathBuf,
    /// sha256 of the content.
    pub hash: String,
    /// The (possibly cut) file text.
    pub text: String,
    /// `text` was cut at the preview limit.
    pub truncated: bool,
    /// Problems found in the file (displayed lines).
    pub problems: Vec<String>,
}

/// Show the prompt; `on_decision` runs after the dialog has closed.
pub fn present(
    ctx: &Rc<AppContext>,
    review: &Review,
    show_contents: bool,
    on_decision: impl FnOnce(Decision) + 'static,
) {
    let done: Rc<RefCell<Option<DecisionFn>>> = Rc::new(RefCell::new(Some(Box::new(on_decision))));
    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Local menu file"));
    dialog.set_content_width(560);
    let column = modal::padded_column(8);
    column.append(&ui::label(tr(
        "This folder contains a menu file that can run commands.",
    )));
    let file: &Path = &local_menu_path(&review.dir);
    let path = ui::caption(&file.to_string_lossy());
    path.set_selectable(true);
    path.set_widget_name("ofm-usermenu-trust-path");
    column.append(&path);
    let hash = gtk::Label::builder()
        .label(format!("sha256 {}", review.hash))
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::Char)
        .selectable(true)
        .css_classes(["monospace", "dim-label"])
        .build();
    hash.set_widget_name("ofm-usermenu-trust-hash");
    hash.update_property(&[gtk::accessible::Property::Label(tr("Menu file hash"))]);
    column.append(&hash);
    if !review.problems.is_empty() {
        column.append(&ui::caption(&format!(
            "{} {}",
            review.problems.len(),
            tr("problem(s) in this file")
        )));
    }

    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .build();
    let mut text = review.text.clone();
    if review.truncated {
        text.push_str("\n...\n");
        text.push_str(tr("(the rest of the file is not shown)"));
    }
    view.buffer().set_text(&text);
    view.update_property(&[gtk::accessible::Property::Label(tr("Menu file contents"))]);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&view)
        .min_content_height(220)
        .max_content_height(320)
        .propagate_natural_height(true)
        .build();
    scroll.add_css_class("card");
    let revealer = gtk::Revealer::builder()
        .child(&scroll)
        .reveal_child(show_contents)
        .build();
    revealer.set_widget_name("ofm-usermenu-trust-contents");
    column.append(&revealer);

    let show = ui::button(tr("_Show contents"));
    show.set_widget_name("ofm-usermenu-trust-show");
    let ignore = ui::button(tr("_Ignore"));
    ignore.set_widget_name("ofm-usermenu-trust-ignore");
    let trust = ui::button(tr("_Trust"));
    trust.set_widget_name("ofm-usermenu-trust-trust");
    trust.add_css_class("destructive-action");
    column.append(&ui::button_row(&[&show, &ignore, &trust]));
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&show));

    {
        let revealer = revealer.clone();
        let show_for_sync = show.clone();
        let label_show = tr("_Show contents");
        let label_hide = tr("_Hide contents");
        let sync = move |on: bool| {
            show_for_sync.set_label(if on { label_hide } else { label_show });
        };
        sync(show_contents);
        show.connect_clicked(move |_| {
            let on = !revealer.reveals_child();
            revealer.set_reveal_child(on);
            sync(on);
        });
    }
    let decision = Rc::new(Cell::new(Decision::Ignore));
    for (button, d) in [(&trust, Decision::Trust), (&ignore, Decision::Ignore)] {
        let (decision, weak) = (Rc::clone(&decision), dialog.downgrade());
        button.connect_clicked(move |_| {
            decision.set(d);
            if let Some(dialog) = weak.upgrade() {
                dialog.close();
            }
        });
    }
    {
        let (done, decision) = (Rc::clone(&done), Rc::clone(&decision));
        dialog.connect_closed(move |_| {
            let cb = done.borrow_mut().take();
            if let Some(cb) = cb {
                let d = decision.get();
                // After the dialog is gone: the next dialog must not stack on a closing one.
                gtk::glib::idle_add_local_once(move || cb(d));
            }
        });
    }
    let ctx_ok = Rc::clone(ctx);
    let shown = modal::present(ctx, &dialog, move || {
        // Enter presses the focused button; with none focused it means "Ignore" (never Trust).
        !ui::activate_focused_button(&ctx_ok)
    });
    if !shown {
        let cb = done.borrow_mut().take();
        if let Some(cb) = cb {
            cb(Decision::Ignore);
        }
    }
}
