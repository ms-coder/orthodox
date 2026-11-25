//! The Help dialog (F1, `app.help`): every command of the registry with its description and the
//! keys the *active* keymap binds it to, grouped by keymap section, as a scrollable, searchable
//! list (T8.11). The problems of the last config/keymap/theme load are listed first, under
//! "Config warnings" — the status line points here.
//!
//! Esc closes (the dialog keymap section's `dialog.cancel`, like every dialog); typing in the
//! search field filters the rows by command id, description or key.

use std::rc::Rc;

use adw::prelude::*;
use ofm_config::Keymap;
use ofm_core::KeyContext;
use ofm_core::commands::ALL;

use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::ui;
use crate::navigation::modal;

/// One row of the list: the data only, no widgets (pure, unit-tested).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpRow {
    /// Command id, e.g. `file.copy`.
    pub cmd: ofm_core::CommandId,
    /// English description (the registry title).
    pub title: &'static str,
    /// Keymap section the default binding lives in (the group of the row).
    pub section: KeyContext,
    /// The bound chords of the active keymap, formatted with the chord formatter (empty when
    /// unbound).
    pub keys: String,
}

/// Rows for every command of the registry: grouped by section (in `KeyContext` order), then by
/// command id.
pub fn rows(keymap: &Keymap) -> Vec<HelpRow> {
    let mut out: Vec<HelpRow> = ALL
        .iter()
        .map(|info| HelpRow {
            cmd: info.id,
            title: info.title,
            section: info.context,
            keys: keymap
                .chords_for(info.id)
                .iter()
                .map(|(_, chord)| chord.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        })
        .collect();
    out.sort_by(|a, b| (a.section, a.cmd.as_str()).cmp(&(b.section, b.cmd.as_str())));
    out
}

/// The text a row is found by: id, description and keys, lower-case.
fn haystack(row: &HelpRow) -> String {
    format!("{} {} {}", row.cmd, row.title, row.keys).to_lowercase()
}

/// The section heading (English; `tr()` at build time).
fn section_title(section: KeyContext) -> &'static str {
    match section {
        KeyContext::Global => "Global keys",
        KeyContext::Panels => "Panels",
        KeyContext::CmdLine => "Command line",
        KeyContext::Viewer => "Viewer",
        KeyContext::Dialog => "Dialogs",
    }
}

/// One group of the list: the heading and its rows `(widget, haystack)`.
struct Group {
    header: gtk::Label,
    rows: Vec<(gtk::Label, String)>,
}

impl Group {
    fn new(list: &gtk::Box, title: &str) -> Group {
        let header = gtk::Label::new(Some(title));
        header.set_xalign(0.0);
        header.add_css_class("heading");
        header.set_margin_top(6);
        header.update_property(&[gtk::accessible::Property::Label(title)]);
        list.append(&header);
        Group {
            header,
            rows: Vec::new(),
        }
    }

    fn push_row(&mut self, list: &gtk::Box, text: &str, hay: String) {
        let label = gtk::Label::new(Some(text));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_selectable(true);
        label.add_css_class("ofm-help-row");
        label.update_property(&[gtk::accessible::Property::Label(text)]);
        list.append(&label);
        self.rows.push((label, hay));
    }

    /// Show the rows matching `needle`; hide the heading when no row matches.
    fn filter(&self, needle: &str) {
        let mut any = false;
        for (label, hay) in &self.rows {
            let show = needle.is_empty() || hay.contains(needle);
            label.set_visible(show);
            any |= show;
        }
        self.header.set_visible(any);
    }
}

/// Build and show the dialog over the main window.
pub fn present(ctx: &Rc<AppContext>) {
    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Help"));
    dialog.set_content_width(560);
    dialog.set_content_height(480);
    let column = modal::padded_column(8);

    // Search field: filters the rows while typing.
    let search = gtk::SearchEntry::new();
    search.set_widget_name("ofm-help-search");
    search.set_placeholder_text(Some(tr("Search command, description or key")));
    search.update_property(&[gtk::accessible::Property::Label(tr("Search commands"))]);
    column.append(&search);

    // The scrollable list of groups.
    let all_rows = rows(&ctx.keymap.borrow());
    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);
    scrolled.set_vexpand(true);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list.set_widget_name("ofm-help-list");
    let mut groups: Vec<Group> = Vec::new();

    // Config problems first (the status line's "see Help > Config warnings" points here).
    let problems = super::preferences::warning_lines(ctx);
    if !problems.is_empty() {
        let mut group = Group::new(&list, tr("Config warnings"));
        for text in &problems {
            group.push_row(&list, &format!("\u{2022} {text}"), text.to_lowercase());
        }
        groups.push(group);
    }

    for section in [
        KeyContext::Global,
        KeyContext::Panels,
        KeyContext::CmdLine,
        KeyContext::Viewer,
        KeyContext::Dialog,
    ] {
        let section_rows: Vec<&HelpRow> =
            all_rows.iter().filter(|r| r.section == section).collect();
        if section_rows.is_empty() {
            continue;
        }
        let mut group = Group::new(&list, tr(section_title(section)));
        for r in section_rows {
            // "command - description - key(s)".
            let text = if r.keys.is_empty() {
                format!("{} \u{2014} {}", r.cmd, tr(r.title))
            } else {
                format!("{} \u{2014} {} \u{2014} {}", r.cmd, tr(r.title), r.keys)
            };
            group.push_row(&list, &text, haystack(r));
        }
        groups.push(group);
    }
    scrolled.set_child(Some(&list));
    column.append(&scrolled);

    let close = ui::button(tr("_Close"));
    close.set_widget_name("ofm-help-close");
    column.append(&ui::button_row(&[&close]));
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&search));

    {
        let weak = dialog.downgrade();
        close.connect_clicked(move |_| {
            if let Some(d) = weak.upgrade() {
                d.close();
            }
        });
    }
    {
        let groups = Rc::new(groups);
        search.connect_changed(move |e| {
            let needle = e.text().to_lowercase();
            let needle = needle.trim();
            for group in groups.iter() {
                group.filter(needle);
            }
        });
    }
    modal::present(ctx, &dialog, || true);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use ofm_core::CommandId;

    fn far_keymap() -> Keymap {
        let tmp = ofm_testkit::temp::TempDir::new().unwrap();
        let paths = ofm_config::ConfigPaths::with_dirs(tmp.path().join("c"), tmp.path().join("s"));
        Keymap::load(&paths, ofm_config::Preset::Far, None).0
    }

    #[test]
    fn every_command_is_listed_once_in_section_order() {
        let km = far_keymap();
        let out = rows(&km);
        assert_eq!(out.len(), ALL.len());
        let mut seen = std::collections::HashSet::new();
        for r in &out {
            assert!(seen.insert(r.cmd), "{} twice", r.cmd);
            assert_eq!(r.title, r.cmd.info().map(|i| i.title).unwrap_or_default());
            assert_eq!(r.section, r.cmd.info().map(|i| i.context).unwrap());
        }
        // Sections are contiguous and in `KeyContext` order (Global < Panels < ...).
        let sections: Vec<KeyContext> = out.iter().map(|r| r.section).collect();
        let mut sorted = sections.clone();
        sorted.sort();
        assert_eq!(sections, sorted);
    }

    #[test]
    fn keys_are_shown_through_the_chord_formatter() {
        let km = far_keymap();
        let out = rows(&km);
        let view = out
            .iter()
            .find(|r| r.cmd == CommandId::new("file.view"))
            .unwrap();
        assert!(view.keys.contains("F3"), "got {:?}", view.keys);
        // An unbound command (a keymap.toml binds nothing) still lists, with empty keys.
        assert!(out.iter().all(|r| r.cmd != CommandId::new("nope.nothing")));
    }

    #[test]
    fn haystack_covers_id_title_and_keys() {
        let km = far_keymap();
        let out = rows(&km);
        let copy = out.iter().find(|r| r.cmd.as_str() == "file.copy").unwrap();
        let hay = haystack(copy);
        assert!(hay.contains("file.copy"));
        assert!(hay.contains("copy"));
        assert!(hay.contains('5'), "F5 binding: {copy:?}");
    }
}
