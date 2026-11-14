//! A modal "pick one of these" list used by the history list (Alt+F12), the drive menu
//! (Alt+F1/F2) and the hotlist (Ctrl+D). Up/Down move the selection (native list navigation),
//! Enter picks (`dialog.ok`), Esc cancels (`dialog.cancel`); extra mnemonic buttons are optional.

use std::rc::Rc;

use adw::prelude::*;

use super::modal;
use crate::context::AppContext;
use crate::i18n::tr;

/// One row of the list.
#[derive(Debug, Clone)]
pub struct ChoiceRow {
    /// Main text (plain text, not markup).
    pub title: String,
    /// Secondary line (path, state), plain text.
    pub subtitle: Option<String>,
}

/// Callback with the context and the chosen row index.
pub type PickFn = Rc<dyn Fn(&Rc<AppContext>, usize)>;
/// Callback of an extra button; receives the dialog so it can close it.
pub type ButtonFn = Rc<dyn Fn(&Rc<AppContext>, &adw::Dialog)>;

/// An additional button below the list.
pub struct ChooserButton {
    /// Label with a `_` mnemonic.
    pub label: &'static str,
    /// Click handler.
    pub on_click: ButtonFn,
}

/// Everything [`open`] needs.
pub struct ChooserSpec {
    /// Dialog title.
    pub title: &'static str,
    /// Rows, in display order.
    pub rows: Vec<ChoiceRow>,
    /// Initially selected row.
    pub selected: usize,
    /// Text shown when `rows` is empty.
    pub empty_text: &'static str,
    /// Called with the picked row; the dialog closes afterwards.
    pub on_pick: PickFn,
    /// Extra buttons.
    pub buttons: Vec<ChooserButton>,
}

/// Show the chooser. `None` when there is no main window.
pub fn open(ctx: &Rc<AppContext>, spec: ChooserSpec) -> Option<adw::Dialog> {
    let dialog = adw::Dialog::new();
    dialog.set_title(tr(spec.title));
    dialog.set_content_width(520);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Browse);
    list.add_css_class("boxed-list");
    list.update_property(&[gtk::accessible::Property::Label(tr(spec.title))]);
    for row in &spec.rows {
        let r = adw::ActionRow::new();
        r.set_use_markup(false);
        r.set_title(&row.title);
        if let Some(sub) = &row.subtitle {
            r.set_subtitle(sub);
        }
        r.set_activatable(true);
        list.append(&r);
    }
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_min_content_height(if spec.rows.is_empty() { 60 } else { 280 });
    scroller.set_max_content_height(480);
    scroller.set_propagate_natural_height(true);
    scroller.set_child(Some(&list));

    let column = modal::padded_column(12);
    if spec.rows.is_empty() {
        let empty = gtk::Label::new(Some(tr(spec.empty_text)));
        empty.add_css_class("dim-label");
        column.append(&empty);
    } else {
        column.append(&scroller);
    }
    if !spec.buttons.is_empty() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.set_halign(gtk::Align::End);
        for b in &spec.buttons {
            let btn = gtk::Button::with_mnemonic(tr(b.label));
            let (ctx, dlg, f) = (Rc::clone(ctx), dialog.clone(), Rc::clone(&b.on_click));
            btn.connect_clicked(move |_| f(&ctx, &dlg));
            row.append(&btn);
        }
        column.append(&row);
    }
    dialog.set_child(Some(&modal::with_header(&column)));

    if let Some(row) = list.row_at_index(spec.selected.try_into().unwrap_or(0)) {
        list.select_row(Some(&row));
        dialog.set_focus(Some(&row));
    }

    // Mouse / row activation picks too.
    {
        let (ctx, dlg, pick) = (Rc::clone(ctx), dialog.clone(), Rc::clone(&spec.on_pick));
        list.connect_row_activated(move |_, row| {
            let idx = usize::try_from(row.index()).unwrap_or(0);
            dlg.close();
            pick(&ctx, idx);
        });
    }
    let shown = {
        let (ctx2, pick, list) = (Rc::clone(ctx), Rc::clone(&spec.on_pick), list.clone());
        modal::present(ctx, &dialog, move || {
            match list
                .selected_row()
                .and_then(|r| usize::try_from(r.index()).ok())
            {
                Some(idx) => {
                    pick(&ctx2, idx);
                    true
                }
                None => false,
            }
        })
    };
    shown.then_some(dialog)
}
