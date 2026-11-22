//! Preview of a batch rename (and of its undo): every change as `old -> new`, notes about
//! temporary names, skipped names and replaced undo information. Enter applies, Esc cancels.

use std::rc::Rc;

use adw::prelude::*;

use super::batchrename_choice::{self, ButtonSpec, ChoiceSpec};
use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::ui;

/// What the preview shows.
pub struct PreviewView {
    /// Window title.
    pub title: &'static str,
    /// Line above the list ("3 renames in /dir").
    pub heading: String,
    /// The `old -> new` lines.
    pub lines: Vec<String>,
    /// Dimmed notes below the list.
    pub notes: Vec<String>,
    /// Label of the apply button (mnemonic).
    pub apply_label: &'static str,
}

/// Show the preview; `on_answer(true)` = apply, `false` = cancel (after the dialog closed).
pub fn present(ctx: &Rc<AppContext>, view: PreviewView, on_answer: impl FnOnce(bool) + 'static) {
    let mut content: Vec<gtk::Widget> = Vec::new();
    let heading = ui::label(&view.heading);
    heading.set_widget_name("ofm-batchrename-heading");
    content.push(heading.upcast());

    let text = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::None)
        .left_margin(8)
        .right_margin(8)
        .top_margin(6)
        .bottom_margin(6)
        .build();
    text.set_widget_name("ofm-batchrename-preview");
    text.update_property(&[gtk::accessible::Property::Label(tr("Planned renames"))]);
    text.buffer().set_text(&view.lines.join("\n"));
    let scroll = gtk::ScrolledWindow::builder()
        .child(&text)
        .min_content_height(160)
        .max_content_height(360)
        .propagate_natural_height(true)
        .build();
    scroll.add_css_class("card");
    content.push(scroll.upcast());

    for note in &view.notes {
        let c = ui::caption(note);
        c.set_widget_name("ofm-batchrename-note");
        content.push(c.upcast());
    }

    batchrename_choice::present(
        ctx,
        ChoiceSpec {
            title: view.title,
            width: 560,
            content,
            buttons: vec![
                ButtonSpec {
                    label: tr("Cancel"),
                    name: "ofm-batchrename-cancel",
                    suggested: false,
                },
                ButtonSpec {
                    label: view.apply_label,
                    name: "ofm-batchrename-apply",
                    suggested: true,
                },
            ],
            default: 1,
            cancel: 0,
        },
        move |i| on_answer(i == 1),
    );
}
