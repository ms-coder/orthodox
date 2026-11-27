//! The Find window (Alt+F7, F24): form, streaming results, and the actions on results.
//!
//! A separate, non-modal toplevel (transient for the main window) so the panels stay usable and
//! the viewer / editor can be opened from the results. Keys go through the central dispatcher
//! (`WindowKind::Dialog`): Enter = `dialog.ok` (start the search; "go to" while the results have
//! the focus), Esc = `dialog.cancel` (stop a running search, else close), F10 =
//! `search.panelize_results`; F3 / F4 (the keymap's `file.view` / `file.edit`) act on the selected
//! result through the dialog key fallback. See `search/mod.rs` for the routing.
//!
//! The results are a `gtk::ColumnView` over [`HitModel`]: only visible rows exist as widgets, so
//! a million hits scroll like a hundred. All logic that does not need widgets lives in
//! `crate::search`.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;
use gtk::pango;
use ofm_config::history::push_recent;
use ofm_core::{KeyChord, KeyContext, cmd};

use crate::context::AppContext;
use crate::dispatch::{self, DialogKeyFallback, WindowKind};
use crate::i18n::tr;
use crate::jobs::ui;
use crate::navigation::modal::ModalStack;
use crate::navigation::store::HistoryStore;
use crate::panel::archive::{self, ViewOrigin};
use crate::search::form::{
    ENCODING_CHOICES, Field, FindForm, LocalClock, ParseCtx, parse_roots, title_for,
};
use crate::search::model::{HitModel, HitRow};
use crate::search::run::{RunState, SearchRun};
use crate::search::{self, actions, panelize, rows, state, style};
use crate::widgets::file_list::settings;

// ---- clock -------------------------------------------------------------------------------------

/// The real clock and the user's time zone (glib `DateTime` computes in memory).
pub struct SystemClock;

impl LocalClock for SystemClock {
    fn now_ns(&self) -> i128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as i128)
    }

    fn local_to_unix(&self, y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Option<i64> {
        glib::DateTime::from_local(y, mo as i32, d as i32, h as i32, mi as i32, f64::from(s))
            .ok()
            .map(|dt| dt.to_unix())
    }

    fn unix_to_local(&self, secs: i64) -> Option<(i32, u32, u32, u32, u32, u32)> {
        let dt = glib::DateTime::from_unix_local(secs).ok()?;
        Some((
            dt.year(),
            dt.month() as u32,
            dt.day_of_month() as u32,
            dt.hour() as u32,
            dt.minute() as u32,
            dt.second() as u32,
        ))
    }
}

// ---- entry history -----------------------------------------------------------------------------

/// Up / Down in `entry` walk through `history` (newest first): the text-entry exception to "no key
/// handling in widgets", same behaviour as the mask dialog.
pub fn attach_history(entry: &gtk::Entry, history: Rc<Vec<String>>) {
    if history.is_empty() {
        return;
    }
    let at = Rc::new(Cell::new(None::<usize>));
    let keys = gtk::EventControllerKey::new();
    let e = entry.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        let next = match key {
            gtk::gdk::Key::Up => at.get().map_or(0, |i| (i + 1).min(history.len() - 1)),
            gtk::gdk::Key::Down => match at.get() {
                Some(0) | None => return glib::Propagation::Proceed,
                Some(i) => i - 1,
            },
            _ => return glib::Propagation::Proceed,
        };
        at.set(Some(next));
        e.set_text(&history[next]);
        e.set_position(-1);
        glib::Propagation::Stop
    });
    entry.add_controller(keys);
}

// ---- widgets -----------------------------------------------------------------------------------

fn entry(name: &str, label: &str) -> gtk::Entry {
    let e = gtk::Entry::new();
    e.set_widget_name(name);
    e.set_hexpand(true);
    e.update_property(&[gtk::accessible::Property::Label(label)]);
    e
}

fn check(mnemonic: &str, name: &str, active: bool) -> gtk::CheckButton {
    let c = gtk::CheckButton::with_mnemonic(mnemonic);
    c.set_widget_name(name);
    c.set_active(active);
    c.update_property(&[gtk::accessible::Property::Label(&mnemonic.replace('_', ""))]);
    c
}

fn row_label(mnemonic: &str, target: &impl IsA<gtk::Widget>) -> gtk::Label {
    let l = gtk::Label::with_mnemonic(mnemonic);
    l.set_xalign(0.0);
    l.set_mnemonic_widget(Some(target));
    l
}

/// Chord of `id` for a button label, e.g. `F10`.
fn hint(ctx: &AppContext, id: ofm_core::CommandId) -> String {
    let km = ctx.keymap.borrow();
    km.chords_for(id)
        .into_iter()
        .find(|(c, _)| matches!(c, KeyContext::Dialog | KeyContext::Panels))
        .map_or_else(String::new, |(_, chord)| chord.to_string())
}

fn with_hint(label: &str, key: &str) -> String {
    if key.is_empty() {
        label.to_owned()
    } else {
        format!("{label} ({key})")
    }
}

struct Widgets {
    mask: gtk::Entry,
    mask_case: gtk::CheckButton,
    text: gtk::Entry,
    text_regex: gtk::CheckButton,
    text_case: gtk::CheckButton,
    whole_words: gtk::CheckButton,
    encoding: gtk::DropDown,
    size_min: gtk::Entry,
    size_max: gtk::Entry,
    after: gtk::Entry,
    before: gtk::Entry,
    roots: gtk::Entry,
    subdirs: gtk::CheckButton,
    follow: gtk::CheckButton,
    same_fs: gtk::CheckButton,
    hidden: gtk::CheckButton,
    archives: gtk::CheckButton,
    start: gtk::Button,
    goto: gtk::Button,
    view: gtk::Button,
    edit: gtk::Button,
    panelize: gtk::Button,
    close_after: gtk::CheckButton,
    status: gtk::Label,
    notice: gtk::Label,
    warnings_button: gtk::MenuButton,
    warnings_text: gtk::TextView,
}

/// The Find window.
pub struct FindWindow {
    ctx: Weak<AppContext>,
    /// The toplevel.
    pub window: adw::Window,
    run: Rc<SearchRun>,
    w: Widgets,
    results: gtk::ColumnView,
    selection: gtk::MultiSelection,
    /// Directory relative "search in" entries are resolved against.
    base: PathBuf,
    /// The form of the search whose results are shown (panel title, panelize base).
    last_form: RefCell<Option<FindForm>>,
    last_roots: RefCell<Vec<PathBuf>>,
}

/// Open the Find window, or bring the open one to the front.
pub fn open(ctx: &Rc<AppContext>) -> Option<Rc<FindWindow>> {
    let st = state(ctx)?;
    if let Some(w) = st.window() {
        w.window.present();
        w.w.mask.grab_focus();
        return Some(w);
    }
    let w = FindWindow::new(ctx)?;
    st.set_window(Some(Rc::clone(&w)));
    w.window.present();
    w.w.mask.grab_focus();
    w.w.mask.select_region(0, -1);
    Some(w)
}

impl FindWindow {
    fn new(ctx: &Rc<AppContext>) -> Option<Rc<FindWindow>> {
        let main = ctx.window()?;
        style::FindStyle::ensure(ctx);
        let history = HistoryStore::ensure(ctx).snapshot();
        let base = search::panelize::command_cwd(&ctx.active_controller().location());

        // ---- form
        let mask = entry("ofm-find-mask", tr("File mask"));
        mask.set_text(history.find_masks.first().map_or("*", String::as_str));
        let mask_case = check(tr("Match _case"), "ofm-find-mask-case", false);
        let text = entry("ofm-find-text", tr("Containing text"));
        let text_regex = check(tr("_Regular expression"), "ofm-find-regex", false);
        let text_case = check(tr("Ca_se sensitive"), "ofm-find-text-case", false);
        let whole_words = check(tr("_Whole words"), "ofm-find-whole", false);
        let names: Vec<&str> = ENCODING_CHOICES.iter().map(|(_, n)| *n).collect();
        let encoding = gtk::DropDown::from_strings(&names);
        encoding.set_widget_name("ofm-find-encoding");
        encoding.update_property(&[gtk::accessible::Property::Label(tr("Text encoding"))]);
        let size_min = entry("ofm-find-size-min", tr("Minimum size"));
        size_min.set_placeholder_text(Some("10k"));
        let size_max = entry("ofm-find-size-max", tr("Maximum size"));
        size_max.set_placeholder_text(Some("2M"));
        let after = entry("ofm-find-after", tr("Modified after"));
        after.set_placeholder_text(Some("2026-01-31  7d  yesterday"));
        let before = entry("ofm-find-before", tr("Modified before"));
        before.set_placeholder_text(Some("2026-12-31 18:00"));
        let roots = entry("ofm-find-roots", tr("Search in"));
        roots.set_text(&base.to_string_lossy());
        let browse = ui::button(tr("Br_owse..."));
        browse.set_widget_name("ofm-find-browse");
        let subdirs = check(tr("Sub_directories"), "ofm-find-subdirs", true);
        let follow = check(tr("Fol_low symlinks"), "ofm-find-follow", false);
        let same_fs = check(tr("Sta_y on one file system"), "ofm-find-samefs", false);
        let hidden = check(tr("Include _hidden"), "ofm-find-hidden", true);
        let archives = check(tr("Search in archi_ves"), "ofm-find-archives", false);

        let grid = gtk::Grid::new();
        grid.set_row_spacing(6);
        grid.set_column_spacing(8);
        grid.attach(&row_label(tr("File _mask:"), &mask), 0, 0, 1, 1);
        grid.attach(&mask, 1, 0, 1, 1);
        grid.attach(&mask_case, 2, 0, 1, 1);
        grid.attach(&row_label(tr("Containing _text:"), &text), 0, 1, 1, 1);
        grid.attach(&text, 1, 1, 1, 1);
        grid.attach(&text_regex, 2, 1, 1, 1);
        let text_opts = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        text_opts.append(&text_case);
        text_opts.append(&whole_words);
        let enc_label = row_label(tr("_Encoding:"), &encoding);
        text_opts.append(&enc_label);
        text_opts.append(&encoding);
        grid.attach(&text_opts, 1, 2, 2, 1);
        let size_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        size_row.append(&size_min);
        size_row.append(&gtk::Label::new(Some("..")));
        size_row.append(&size_max);
        grid.attach(&row_label(tr("Si_ze, min .. max:"), &size_min), 0, 3, 1, 1);
        grid.attach(&size_row, 1, 3, 2, 1);
        let date_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        date_row.append(&after);
        date_row.append(&row_label(tr("_before:"), &before));
        date_row.append(&before);
        grid.attach(&row_label(tr("Modified _after:"), &after), 0, 4, 1, 1);
        grid.attach(&date_row, 1, 4, 2, 1);
        grid.attach(&row_label(tr("Search _in:"), &roots), 0, 5, 1, 1);
        grid.attach(&roots, 1, 5, 1, 1);
        grid.attach(&browse, 2, 5, 1, 1);
        let opts = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        for c in [&subdirs, &follow, &same_fs, &hidden, &archives] {
            opts.append(c);
        }
        grid.attach(&opts, 1, 6, 2, 1);

        let notice = ui::error_label();
        let start = ui::button(tr("Start"));
        start.set_widget_name("ofm-find-start");
        start.add_css_class("suggested-action");

        // ---- results
        let model = HitModel::new();
        let selection = gtk::MultiSelection::new(Some(model.clone()));
        let results = gtk::ColumnView::new(Some(selection.clone()));
        results.set_widget_name("ofm-find-results");
        results.add_css_class("ofm-find-results");
        results.set_show_row_separators(false);
        results.set_hexpand(true);
        results.set_vexpand(true);
        results.update_property(&[gtk::accessible::Property::Label(tr("Search results"))]);
        let frame = gtk::ScrolledWindow::new();
        frame.add_css_class("ofm-find-frame");
        frame.set_min_content_height(200);
        frame.set_child(Some(&results));
        let run = SearchRun::new(model.clone());
        add_columns(&results, &model);

        let status = gtk::Label::new(Some(tr("Enter a mask and press Enter to search.")));
        status.add_css_class("ofm-find-status");
        status.set_xalign(0.0);
        status.set_hexpand(true);
        status.set_ellipsize(pango::EllipsizeMode::Middle);
        status.set_accessible_role(gtk::AccessibleRole::Status);
        status.update_property(&[gtk::accessible::Property::Label(tr("Search status"))]);
        let warnings_text = gtk::TextView::new();
        warnings_text.set_editable(false);
        warnings_text.set_cursor_visible(false);
        warnings_text.set_monospace(true);
        warnings_text.update_property(&[gtk::accessible::Property::Label(tr("Search warnings"))]);
        let warn_scroll = gtk::ScrolledWindow::new();
        warn_scroll.set_min_content_width(560);
        warn_scroll.set_min_content_height(200);
        warn_scroll.set_child(Some(&warnings_text));
        let popover = gtk::Popover::new();
        popover.set_child(Some(&warn_scroll));
        let warnings_button = gtk::MenuButton::new();
        warnings_button.set_label(tr("Warnings (0)"));
        warnings_button.set_popover(Some(&popover));
        warnings_button.set_sensitive(false);
        warnings_button.set_widget_name("ofm-find-warnings");
        warnings_button.update_property(&[gtk::accessible::Property::Label(tr("Show warnings"))]);

        let (key_goto, key_view, key_edit, key_pan) = (
            hint(ctx, cmd::DIALOG_OK),
            hint(ctx, cmd::FILE_VIEW),
            hint(ctx, cmd::FILE_EDIT),
            hint(ctx, cmd::SEARCH_PANELIZE_RESULTS),
        );
        let goto = ui::button(&with_hint(tr("Go to"), &key_goto));
        goto.set_widget_name("ofm-find-goto");
        let view = ui::button(&with_hint(tr("View"), &key_view));
        view.set_widget_name("ofm-find-view");
        let edit = ui::button(&with_hint(tr("Edit"), &key_edit));
        edit.set_widget_name("ofm-find-edit");
        let panelize_btn = ui::button(&with_hint(tr("Panelize"), &key_pan));
        panelize_btn.set_widget_name("ofm-find-panelize");
        let close_after = check(
            tr("Close this window after go to / panelize"),
            "ofm-find-close-after",
            true,
        );
        let close_button = ui::button(tr("Close"));
        close_button.set_widget_name("ofm-find-close");

        let actions_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions_row.append(&goto);
        actions_row.append(&view);
        actions_row.append(&edit);
        actions_row.append(&panelize_btn);
        actions_row.append(&close_after);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        actions_row.append(&spacer);
        actions_row.append(&warnings_button);
        actions_row.append(&close_button);

        let form_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        form_buttons.set_halign(gtk::Align::End);
        form_buttons.append(&start);

        let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
        column.set_margin_top(12);
        column.set_margin_bottom(12);
        column.set_margin_start(12);
        column.set_margin_end(12);
        column.append(&grid);
        column.append(&notice);
        column.append(&form_buttons);
        column.append(&row_label(tr("Res_ults:"), &results));
        column.append(&frame);
        column.append(&status);
        column.append(&actions_row);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&column));
        let window = adw::Window::builder()
            .title(tr("Find files"))
            .default_width(980)
            .default_height(720)
            .content(&toolbar)
            .build();
        window.set_transient_for(Some(&main.window));
        window.set_destroy_with_parent(true);
        window.add_css_class("ofm-window");
        if let Some(app) = main.window.application() {
            window.set_application(Some(&app));
        }
        dispatch::install_key_controller(&window, ctx, WindowKind::Dialog);

        let this = Rc::new(FindWindow {
            ctx: Rc::downgrade(ctx),
            window: window.clone(),
            run,
            w: Widgets {
                mask,
                mask_case,
                text,
                text_regex,
                text_case,
                whole_words,
                encoding,
                size_min,
                size_max,
                after,
                before,
                roots,
                subdirs,
                follow,
                same_fs,
                hidden,
                archives,
                start,
                goto,
                view,
                edit,
                panelize: panelize_btn,
                close_after,
                status,
                notice,
                warnings_button,
                warnings_text,
            },
            results,
            selection,
            base,
            last_form: RefCell::new(None),
            last_roots: RefCell::new(Vec::new()),
        });
        this.wire(ctx, &history, &browse, &close_button);
        Some(this)
    }

    fn wire(
        self: &Rc<Self>,
        ctx: &Rc<AppContext>,
        history: &ofm_config::history::History,
        browse: &gtk::Button,
        close_button: &gtk::Button,
    ) {
        attach_history(&self.w.mask, Rc::new(history.find_masks.clone()));
        attach_history(&self.w.text, Rc::new(history.find_texts.clone()));
        attach_history(&self.w.roots, Rc::new(history.find_roots.clone()));

        let weak = Rc::downgrade(self);
        let on = |f: fn(&Rc<FindWindow>)| {
            let weak = weak.clone();
            move |_: &gtk::Button| {
                if let Some(t) = weak.upgrade() {
                    f(&t);
                }
            }
        };
        self.w.start.connect_clicked(on(|t| {
            if t.run.is_active() {
                t.run.stop();
            } else {
                t.start();
            }
        }));
        self.w.goto.connect_clicked(on(|t| t.go_to_selected()));
        self.w.view.connect_clicked(on(|t| t.view_selected()));
        self.w.edit.connect_clicked(on(|t| t.edit_selected()));
        self.w.panelize.connect_clicked(on(|t| t.panelize()));
        close_button.connect_clicked(on(|t| t.window.close()));
        browse.connect_clicked(on(|t| t.browse()));

        // Double click / Enter on a row of the ColumnView (Enter normally arrives as dialog.ok).
        {
            let weak = weak.clone();
            self.results.connect_activate(move |_, _| {
                if let Some(t) = weak.upgrade() {
                    t.go_to_selected();
                }
            });
        }

        // Search progress -> status line, buttons, warnings.
        {
            let weak = weak.clone();
            self.run.changed.connect(move |_| {
                if let Some(t) = weak.upgrade() {
                    t.refresh_status();
                }
            });
        }
        self.selection.connect_selection_changed({
            let weak = weak.clone();
            move |_, _, _| {
                if let Some(t) = weak.upgrade() {
                    t.refresh_buttons();
                }
            }
        });
        self.refresh_buttons();

        // The window's life: install the F3/F4 fallback while it has the focus; cancel the
        // search and forget the window when it closes.
        {
            let weak = weak.clone();
            self.window.connect_is_active_notify(move |w| {
                if w.is_active()
                    && let Some(t) = weak.upgrade()
                    && let Some(ctx) = t.ctx.upgrade()
                {
                    install_fallback(&ctx);
                }
            });
        }
        install_fallback(ctx);
        {
            let weak_ctx = Rc::downgrade(ctx);
            let this = Rc::clone(self);
            self.window.connect_close_request(move |_| {
                this.run.shutdown();
                if let Some(ctx) = weak_ctx.upgrade() {
                    if let Some(st) = state(&ctx) {
                        st.set_window(None);
                    }
                    DialogKeyFallback::ensure(&ctx).clear();
                    // An extraction asked for here ends with the window, and so does any
                    // dialog (password, error) shown over it.
                    if let Some(st) = archive::state(&ctx) {
                        st.cancel_view_from(this.window.upcast_ref());
                    }
                    if let Some(stack) = ctx.ext.get::<ModalStack>() {
                        stack.close_over(&this.window);
                    }
                }
                glib::Propagation::Proceed
            });
        }
    }

    // ---- form -----------------------------------------------------------------------------------

    /// The fields as the user left them.
    pub fn form(&self) -> FindForm {
        let w = &self.w;
        FindForm {
            mask: w.mask.text().to_string(),
            mask_case: w.mask_case.is_active(),
            text: w.text.text().to_string(),
            text_regex: w.text_regex.is_active(),
            text_case: w.text_case.is_active(),
            whole_words: w.whole_words.is_active(),
            encoding: w.encoding.selected() as usize,
            size_min: w.size_min.text().to_string(),
            size_max: w.size_max.text().to_string(),
            after: w.after.text().to_string(),
            before: w.before.text().to_string(),
            roots: w.roots.text().to_string(),
            subdirs: w.subdirs.is_active(),
            follow_symlinks: w.follow.is_active(),
            same_filesystem: w.same_fs.is_active(),
            hidden: w.hidden.is_active(),
            archives: w.archives.is_active(),
        }
    }

    /// Fill the fields (tests, "search again").
    pub fn set_form(&self, f: &FindForm) {
        let w = &self.w;
        w.mask.set_text(&f.mask);
        w.mask_case.set_active(f.mask_case);
        w.text.set_text(&f.text);
        w.text_regex.set_active(f.text_regex);
        w.text_case.set_active(f.text_case);
        w.whole_words.set_active(f.whole_words);
        w.encoding
            .set_selected(u32::try_from(f.encoding).unwrap_or(0));
        w.size_min.set_text(&f.size_min);
        w.size_max.set_text(&f.size_max);
        w.after.set_text(&f.after);
        w.before.set_text(&f.before);
        w.roots.set_text(&f.roots);
        w.subdirs.set_active(f.subdirs);
        w.follow.set_active(f.follow_symlinks);
        w.same_fs.set_active(f.same_filesystem);
        w.hidden.set_active(f.hidden);
        w.archives.set_active(f.archives);
    }

    fn field_widget(&self, f: Field) -> &gtk::Entry {
        match f {
            Field::Mask => &self.w.mask,
            Field::Text => &self.w.text,
            Field::SizeMin => &self.w.size_min,
            Field::SizeMax => &self.w.size_max,
            Field::After => &self.w.after,
            Field::Before => &self.w.before,
            Field::Roots => &self.w.roots,
        }
    }

    fn clear_errors(&self) {
        ui::set_error(&self.w.notice, None);
        for f in [
            Field::Mask,
            Field::Text,
            Field::SizeMin,
            Field::SizeMax,
            Field::After,
            Field::Before,
            Field::Roots,
        ] {
            self.field_widget(f).remove_css_class("error");
        }
    }

    // ---- search ---------------------------------------------------------------------------------

    /// Validate the form and start the search. Shows the problem and focuses the field when the
    /// form is invalid.
    pub fn start(self: &Rc<Self>) {
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        self.clear_errors();
        let form = self.form();
        let home = glib::home_dir();
        let query = form.to_query(&ParseCtx {
            base: &self.base,
            home: &home,
            clock: &SystemClock,
        });
        let query = match query {
            Ok(q) => q,
            Err(e) => {
                ui::set_error(&self.w.notice, Some(&e.message));
                let w = self.field_widget(e.field);
                w.add_css_class("error");
                w.grab_focus();
                return;
            }
        };
        HistoryStore::ensure(&ctx).update(|h| {
            push_recent(&mut h.find_masks, form.mask.trim());
            push_recent(&mut h.find_texts, &form.text);
            push_recent(&mut h.find_roots, form.roots.trim());
        });
        *self.last_roots.borrow_mut() = query.roots.clone();
        *self.last_form.borrow_mut() = Some(form);
        let starter = state(&ctx).map(|s| s.starter());
        if let Some(starter) = starter {
            self.run.start(&ctx.services, &starter, query);
        }
        self.refresh_status();
    }

    /// The search run (tests).
    pub fn run(&self) -> &Rc<SearchRun> {
        &self.run
    }

    fn refresh_status(&self) {
        let stats = self.run.stats();
        let state = self.run.state();
        let counts = format!(
            "{} {}   {} {}   {} {}",
            stats.found,
            tr("found"),
            stats.dirs,
            tr("folders"),
            stats.files,
            tr("files"),
        );
        let text = match &state {
            RunState::Idle => tr("Enter a mask and press Enter to search.").to_owned(),
            RunState::Starting => tr("Starting...").to_owned(),
            RunState::Running => match &stats.current {
                Some(p) => format!("{} {counts}   {}", tr("Searching:"), p.to_string_lossy()),
                None => format!("{} {counts}", tr("Searching:")),
            },
            RunState::Stopping => format!("{} {counts}", tr("Stopping:")),
            RunState::Finished {
                cancelled,
                truncated,
            } => {
                let secs = stats.elapsed.map_or(0.0, |d| d.as_secs_f64());
                let head = if *cancelled {
                    tr("Stopped:")
                } else {
                    tr("Done:")
                };
                let mut s = format!("{head} {counts}   ({secs:.1} s)");
                if *truncated {
                    s.push_str(&format!("   {}", tr("Result limit reached.")));
                }
                s
            }
            RunState::Failed(msg) => format!("{} {msg}", tr("Search failed:")),
        };
        self.w.status.set_text(&text);
        let active = state.is_active();
        self.w
            .start
            .set_label(if active { tr("Stop") } else { tr("Start") });
        self.w
            .start
            .update_property(&[gtk::accessible::Property::Label(if active {
                tr("Stop")
            } else {
                tr("Start")
            })]);
        self.window.set_title(Some(&if stats.found > 0 {
            format!("{} - {}", tr("Find files"), stats.found)
        } else {
            tr("Find files").to_owned()
        }));
        self.refresh_warnings(stats.warnings);
        self.refresh_buttons();
    }

    fn refresh_warnings(&self, count: u64) {
        self.w
            .warnings_button
            .set_label(&format!("{} ({count})", tr("Warnings")));
        self.w.warnings_button.set_sensitive(count > 0);
        if count > 0 && self.w.warnings_button.is_active() {
            let text: String = self
                .run
                .warnings()
                .iter()
                .map(|w| format!("{}: {}\n", w.path.to_string_lossy(), w.message))
                .collect();
            self.w.warnings_text.buffer().set_text(&text);
        }
    }

    fn refresh_buttons(&self) {
        let has_rows = !self.run.model().is_empty();
        for b in [&self.w.goto, &self.w.view, &self.w.edit, &self.w.panelize] {
            b.set_sensitive(has_rows);
        }
    }

    // ---- results --------------------------------------------------------------------------------

    /// Number of results.
    pub fn result_count(&self) -> usize {
        self.run.model().len()
    }

    /// Text of the status line (tests).
    pub fn status_text(&self) -> String {
        self.w.status.text().to_string()
    }

    /// Positions of the selected rows, ascending.
    pub fn selected_indices(&self) -> Vec<usize> {
        let bits = self.selection.selection();
        (0..bits.size())
            .map(|i| bits.nth(i as u32) as usize)
            .collect()
    }

    /// Select exactly `rows` (tests; the user uses arrows, Shift and Ctrl+Space).
    pub fn select_rows(&self, rows: &[usize]) {
        let bits = gtk::Bitset::new_empty();
        for r in rows {
            bits.add(u32::try_from(*r).unwrap_or(u32::MAX));
        }
        self.selection.set_selection(
            &bits,
            &gtk::Bitset::new_range(0, self.run.model().len() as u32),
        );
    }

    /// The row the single-row actions use: the first selected one, else the first row.
    fn current_hit(&self) -> Option<ofm_search::Hit> {
        let model = self.run.model();
        let idx = self.selected_indices().first().copied().unwrap_or(0);
        model.hit(idx)
    }

    /// Open the directory of the current result in the active panel (Enter).
    pub fn go_to_selected(self: &Rc<Self>) {
        let (Some(ctx), Some(hit)) = (self.ctx.upgrade(), self.current_hit()) else {
            return;
        };
        if !actions::go_to(&ctx, &hit) {
            self.notice(tr("This result cannot be shown in a panel."));
            return;
        }
        if self.w.close_after.is_active() {
            self.window.close();
        } else {
            actions::raise_main(&ctx);
        }
    }

    /// F3: view the current result.
    pub fn view_selected(&self) {
        let (Some(ctx), Some(hit)) = (self.ctx.upgrade(), self.current_hit()) else {
            return;
        };
        if let Err(msg) = actions::view(&ctx, &hit, Some(self.view_origin())) {
            self.notice(&msg);
        }
    }

    /// F4: edit the current result.
    pub fn edit_selected(&self) {
        let (Some(ctx), Some(hit)) = (self.ctx.upgrade(), self.current_hit()) else {
            return;
        };
        if let Err(msg) = actions::edit(&ctx, &hit, Some(self.view_origin())) {
            self.notice(&msg);
        }
    }

    fn notice(&self, text: &str) {
        self.w.status.set_text(text);
    }

    /// This window stays on top of the main one: what an archive view has to say goes here.
    fn view_origin(&self) -> ViewOrigin {
        ViewOrigin {
            window: self.window.clone().upcast::<gtk::Window>().downgrade(),
            status: self.w.status.downgrade(),
        }
    }

    /// F10: show the results (the selected ones when several are selected, else all) in the active
    /// panel. A running search is stopped first: the list holds what was found so far.
    pub fn panelize(self: &Rc<Self>) {
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        if self.run.model().is_empty() && !self.run.is_active() {
            self.notice(tr("There are no results to panelize."));
            return;
        }
        if self.run.is_active() {
            self.run.stop();
        }
        self.run.settle();
        let selected = self.selected_indices();
        let only = (selected.len() > 1).then_some(selected);
        let title = self
            .last_form
            .borrow()
            .as_ref()
            .map_or_else(|| tr("Search results").to_owned(), title_for);
        let base = {
            let roots = self.last_roots.borrow();
            (roots.len() == 1).then(|| roots[0].clone())
        };
        let weak = Rc::downgrade(self);
        let ctx2 = Rc::clone(&ctx);
        panelize::panelize_hits(
            &ctx,
            self.run.model().shared(),
            only,
            title,
            base,
            move |outcome| {
                search::notify(&ctx2, &panelize::hits_message(&outcome));
                if let Some(t) = weak.upgrade()
                    && outcome.rows > 0
                {
                    if t.w.close_after.is_active() {
                        t.window.close();
                    } else {
                        actions::raise_main(&ctx2);
                    }
                }
            },
        );
    }

    fn browse(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::new();
        dialog.set_title(tr("Search in"));
        let weak = Rc::downgrade(self);
        dialog.select_folder(
            Some(&self.window),
            gtk::gio::Cancellable::NONE,
            move |res| {
                if let (Some(t), Ok(file)) = (weak.upgrade(), res)
                    && let Some(path) = file.path()
                {
                    t.w.roots.set_text(&path.to_string_lossy());
                    t.w.roots.grab_focus();
                    t.w.roots.set_position(-1);
                }
            },
        );
    }

    // ---- keys -----------------------------------------------------------------------------------

    fn focus_in_results(&self) -> bool {
        gtk::prelude::RootExt::focus(&self.window).is_some_and(|f| {
            f == *self.results.upcast_ref::<gtk::Widget>() || f.is_ancestor(&self.results)
        })
    }

    /// Enter (`dialog.ok`): press the focused button, "go to" in the results, else start.
    pub fn on_ok(self: &Rc<Self>) {
        let focus = gtk::prelude::RootExt::focus(&self.window);
        if let Some(button) = focus.as_ref().and_then(|f| f.downcast_ref::<gtk::Button>()) {
            // Includes the (toggle) button inside the warnings menu button.
            button.emit_clicked();
            return;
        }
        if self.focus_in_results() {
            self.go_to_selected();
            return;
        }
        if !self.run.is_active() {
            self.start();
        }
    }

    /// Esc (`dialog.cancel`): cancel an archive extraction started here, close the warnings
    /// popup, stop a running search, else close.
    pub fn on_cancel(self: &Rc<Self>) {
        if self
            .ctx
            .upgrade()
            .and_then(|ctx| archive::state(&ctx))
            .is_some_and(|st| st.cancel_view_from(self.window.upcast_ref()))
        {
        } else if self.w.warnings_button.is_active() {
            self.w.warnings_button.popdown();
        } else if self.run.is_active() {
            self.run.stop();
        } else {
            self.window.close();
        }
    }

    /// A chord the keymap does not bind in the `dialog` section: F3 / F4 and selection commands
    /// (their `panels` bindings) act on the results unless a text field has the focus.
    fn on_chord(self: &Rc<Self>, ctx: &Rc<AppContext>, chord: KeyChord) -> bool {
        if let Some(f) = gtk::prelude::RootExt::focus(&self.window)
            && (f.is::<gtk::Editable>() || f.is::<gtk::TextView>())
        {
            return false;
        }
        let id = ctx.keymap.borrow().resolve_in(KeyContext::Panels, chord);
        match id {
            Some(c) if c == cmd::FILE_VIEW => self.view_selected(),
            Some(c) if c == cmd::FILE_EDIT => self.edit_selected(),
            Some(c) if c == cmd::SELECT_ALL => {
                self.selection.select_all();
            }
            Some(c) if c == cmd::SELECT_NONE => {
                self.selection.unselect_all();
            }
            _ => return false,
        }
        true
    }

    /// The folders `FindForm::roots` would resolve to (tests).
    pub fn resolved_roots(&self) -> Vec<PathBuf> {
        parse_roots(&self.w.roots.text(), &self.base, &glib::home_dir())
    }
}

/// Route unbound dialog chords of the Find window (see `FindWindow::on_chord`).
fn install_fallback(ctx: &Rc<AppContext>) {
    DialogKeyFallback::ensure(ctx).set(Rc::new(|ctx, chord| {
        let Some(w) = state(ctx).and_then(|s| s.window()) else {
            return false;
        };
        w.window.is_active() && !has_dialog(ctx, &w) && w.on_chord(ctx, chord)
    }));
}

/// Whether a dialog (password, error) is shown over the Find window: it takes the keys then.
pub(crate) fn has_dialog(ctx: &AppContext, w: &FindWindow) -> bool {
    ctx.ext
        .get::<ModalStack>()
        .is_some_and(|s| s.top_is_over(&w.window))
}

// ---- result columns ----------------------------------------------------------------------------

fn time_text(secs: i64) -> String {
    let fmt = settings().time_format.clone();
    glib::DateTime::from_unix_local(secs)
        .ok()
        .and_then(|dt| dt.format(&fmt).ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

type CellFn = fn(&rows::Cells) -> &str;

fn add_columns(view: &gtk::ColumnView, model: &HitModel) {
    let cols: [(&str, CellFn, i32, bool, f32); 5] = [
        (tr("Name"), |c| &c.name, 200, false, 0.0),
        (tr("Folder"), |c| &c.folder, 240, true, 0.0),
        (tr("Size"), |c| &c.size, 70, false, 1.0),
        (tr("Modified"), |c| &c.mtime, 130, false, 0.0),
        (tr("Match"), |c| &c.matched, 200, true, 0.0),
    ];
    for (title, text, width, expand, xalign) in cols {
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let label = gtk::Label::new(None);
            label.set_xalign(xalign);
            label.set_single_line_mode(true);
            label.set_ellipsize(if xalign > 0.5 {
                pango::EllipsizeMode::None
            } else {
                pango::EllipsizeMode::Middle
            });
            item.set_child(Some(&label));
        });
        let model = model.clone();
        let is_match = title == tr("Match");
        factory.connect_bind(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let (Some(row), Some(label)) = (
                item.item().and_downcast::<HitRow>(),
                item.child().and_downcast::<gtk::Label>(),
            ) else {
                return;
            };
            let Some(cells) = model.with_hit(row.index(), |h| {
                rows::cells(h, settings().size_format, &time_text)
            }) else {
                label.set_text("");
                return;
            };
            label.set_text(text(&cells));
            if is_match {
                match &cells.match_range {
                    Some(r) => {
                        let attrs = pango::AttrList::new();
                        let mut bold = pango::AttrInt::new_weight(pango::Weight::Bold);
                        bold.set_start_index(r.start as u32);
                        bold.set_end_index(r.end as u32);
                        attrs.insert(bold);
                        label.set_attributes(Some(&attrs));
                    }
                    None => label.set_attributes(None),
                }
            }
        });
        let col = gtk::ColumnViewColumn::new(Some(title), Some(factory));
        col.set_resizable(true);
        col.set_fixed_width(width);
        col.set_expand(expand);
        view.append_column(&col);
    }
}
