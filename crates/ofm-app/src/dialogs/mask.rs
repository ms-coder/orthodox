//! Select/unselect-by-mask dialog (Gray+ / Gray-), with mask history. Owner: T2.3 (F10).
//!
//! Masks use the `ofm_core::mask` syntax (`*.{jpg,png}`, `*.rs;*.toml`, `!*.tmp`), matched
//! case-insensitively. Accepted masks go to the persisted history (`history.toml`), newest first;
//! Up/Down in the entry walk through it and the list below shows it.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use ofm_config::history::push_recent;
use ofm_core::cmd;
use ofm_core::mask::{Mask, MaskOpts};

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::i18n::tr;
use crate::navigation::modal;
use crate::navigation::store::HistoryStore;

/// Number of recent masks listed in the dialog.
const SHOWN: usize = 8;

/// Register `select.by_mask` / `select.unmask` (the dialog itself; `select.invert` etc. are in
/// `navigation/selection.rs`).
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::SELECT_BY_MASK, |ctx| open(ctx, true));
    reg.register(cmd::SELECT_UNMASK, |ctx| open(ctx, false));
}

/// Make sure the history store (which holds the mask history) exists.
pub fn install_state(ctx: &Rc<AppContext>) {
    HistoryStore::ensure(ctx);
}

/// Apply `mask_text` to the active panel; `Err` carries the parse message. Shared by the dialog
/// and tests.
pub fn apply(
    ctx: &Rc<AppContext>,
    mask_text: &str,
    select: bool,
    files_only: bool,
) -> Result<(), String> {
    let mask = Mask::parse(
        mask_text,
        MaskOpts {
            case_insensitive: true,
        },
    )
    .map_err(|e| e.to_string())?;
    ctx.active_controller()
        .mutate(|m| m.select_by_mask(&mask, select, files_only));
    let text = mask_text.trim().to_owned();
    HistoryStore::ensure(ctx).update(|h| push_recent(&mut h.masks, &text));
    Ok(())
}

fn open(ctx: &Rc<AppContext>, select: bool) {
    let masks = HistoryStore::ensure(ctx).snapshot().masks;
    let dialog = adw::Dialog::new();
    dialog.set_title(&if select {
        tr("Select files").to_owned()
    } else {
        tr("Deselect files").to_owned()
    });
    dialog.set_content_width(440);
    let column = modal::padded_column(8);
    column.append(
        &gtk::Label::builder()
            .label(tr("Mask (for example *.{jpg,png};!*.tmp):"))
            .xalign(0.0)
            .build(),
    );
    let entry = gtk::Entry::new();
    entry.set_text(masks.first().map_or("*", String::as_str));
    entry.update_property(&[gtk::accessible::Property::Label(tr("File mask"))]);
    column.append(&entry);
    let error = gtk::Label::new(None);
    error.set_xalign(0.0);
    error.add_css_class("error");
    error.set_visible(false);
    column.append(&error);
    let files_only = gtk::CheckButton::with_mnemonic(tr("_Files only (keep folders)"));
    files_only.set_active(true);
    column.append(&files_only);

    // History list (click or Tab+Enter picks), Up/Down in the entry walk through it.
    let shown: Vec<String> = masks.iter().take(SHOWN).cloned().collect();
    if !shown.is_empty() {
        column.append(
            &gtk::Label::builder()
                .label(tr("Recent masks:"))
                .xalign(0.0)
                .css_classes(["dim-label"])
                .build(),
        );
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.update_property(&[gtk::accessible::Property::Label(tr("Recent masks"))]);
        for m in &shown {
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&gtk::Label::builder().label(m).xalign(0.0).build()));
            list.append(&row);
        }
        let e = entry.clone();
        let texts = shown.clone();
        list.connect_row_activated(move |_, row| {
            if let Some(t) = usize::try_from(row.index()).ok().and_then(|i| texts.get(i)) {
                e.set_text(t);
                e.grab_focus();
                e.set_position(-1);
            }
        });
        column.append(&list);

        // Text entry exception to "no key handling in widgets": history recall.
        let at = Rc::new(Cell::new(None::<usize>));
        let keys = gtk::EventControllerKey::new();
        let e = entry.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            let next = match key {
                gtk::gdk::Key::Up => at.get().map_or(0, |i| (i + 1).min(shown.len() - 1)),
                gtk::gdk::Key::Down => match at.get() {
                    Some(0) | None => return gtk::glib::Propagation::Proceed,
                    Some(i) => i - 1,
                },
                _ => return gtk::glib::Propagation::Proceed,
            };
            at.set(Some(next));
            e.set_text(&shown[next]);
            e.set_position(-1);
            gtk::glib::Propagation::Stop
        });
        entry.add_controller(keys);
    }
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&entry));

    let (ctx2, entry2, files2, error2) = (
        Rc::clone(ctx),
        entry.clone(),
        files_only.clone(),
        error.clone(),
    );
    modal::present(ctx, &dialog, move || {
        match apply(&ctx2, &entry2.text(), select, files2.is_active()) {
            Ok(()) => true,
            Err(msg) => {
                error2.set_text(&msg);
                error2.set_visible(true);
                entry2.add_css_class("error");
                false
            }
        }
    });
    entry.select_region(0, -1);
}
