//! Properties dialog (T3.3 owns this file; F19, Alt+Enter): chmod (octal entry + checkboxes),
//! owner/group (only if permitted: root), recursive option, directory size (`JobSpec::CalcSize`,
//! result `Outcome::Size`, cancellable: closing the dialog cancels it). Applies through
//! `JobSpec::SetAttrs`.
//!
//! The permission check boxes are tri-state for a multi-item selection: a bit that is set on some
//! items and clear on others shows as "inconsistent" and is left alone unless the user changes
//! it ([`mode_change`] only sends bits that differ from the initial state).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use ofm_core::{Entry, EntryKind};
use ofm_jobs::{JobSpec, ModeChange, Outcome, OwnerChange, RunMode};

use crate::chrome::statusline::{SizeFormat, format_size, format_time};
use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::idnames::{self, IdNames};
use crate::jobs::targets::Targets;
use crate::jobs::{self, SubmitOpts, ui};
use crate::navigation::modal;
use crate::wiring;

/// State of one permission bit over the selected items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bit {
    /// Set on every item.
    On,
    /// Clear on every item.
    Off,
    /// Set on some items only.
    Mixed,
}

/// Permission bits in display order: rwx for owner, group, others, then setuid, setgid, sticky.
pub const MASKS: [u32; 12] = [
    0o400, 0o200, 0o100, 0o040, 0o020, 0o010, 0o004, 0o002, 0o001, 0o4000, 0o2000, 0o1000,
];

/// All twelve bits.
pub type Bits = [Bit; 12];

/// Initial state of every bit over `modes` (empty: all off).
pub fn initial_bits(modes: &[u32]) -> Bits {
    let mut out = [Bit::Off; 12];
    for (slot, mask) in out.iter_mut().zip(MASKS) {
        let set = modes.iter().filter(|m| **m & mask != 0).count();
        *slot = if set == 0 {
            Bit::Off
        } else if set == modes.len() {
            Bit::On
        } else {
            Bit::Mixed
        };
    }
    out
}

/// Bits that must be set / cleared to get from `initial` to `current`; `None` if nothing changed.
pub fn mode_change(initial: &Bits, current: &Bits) -> Option<ModeChange> {
    let (mut set, mut clear) = (0, 0);
    for ((i, c), mask) in initial.iter().zip(current).zip(MASKS) {
        if i == c {
            continue;
        }
        match c {
            Bit::On => set |= mask,
            Bit::Off => clear |= mask,
            Bit::Mixed => {}
        }
    }
    (set | clear != 0).then_some(ModeChange { set, clear })
}

/// Parse an octal permission string (`644`, `0755`, `4755`).
pub fn parse_octal(text: &str) -> Option<u32> {
    let t = text.trim();
    if t.is_empty() || t.len() > 4 || !t.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return None;
    }
    u32::from_str_radix(t, 8).ok()
}

/// All bits from a mode value.
pub fn bits_from_mode(mode: u32) -> Bits {
    initial_bits(&[mode])
}

/// The octal value of `bits`, `None` while any bit is mixed.
pub fn octal_of(bits: &Bits) -> Option<u32> {
    let mut mode = 0;
    for (b, mask) in bits.iter().zip(MASKS) {
        match b {
            Bit::On => mode |= mask,
            Bit::Off => {}
            Bit::Mixed => return None,
        }
    }
    Some(mode)
}

/// What the user typed for owner and group, resolved against the name tables. Empty text or the
/// initial text means "leave unchanged".
pub fn owner_change(
    names: &IdNames,
    owner: &str,
    initial_owner: &str,
    group: &str,
    initial_group: &str,
) -> Result<Option<OwnerChange>, String> {
    let pick = |text: &str, initial: &str| {
        let t = text.trim();
        (!t.is_empty() && t != initial).then_some(t.to_owned())
    };
    let uid = match pick(owner, initial_owner) {
        Some(t) => Some(names.user_id(&t)?),
        None => None,
    };
    let gid = match pick(group, initial_group) {
        Some(t) => Some(names.group_id(&t)?),
        None => None,
    };
    Ok((uid.is_some() || gid.is_some()).then_some(OwnerChange { uid, gid }))
}

fn kind_one(e: &Entry) -> &'static str {
    match e.kind {
        EntryKind::File => tr("File"),
        EntryKind::Dir => tr("Folder"),
        EntryKind::Symlink { .. } => tr("Symbolic link"),
        EntryKind::Other => tr("Special file"),
    }
}

fn kind_many(e: &Entry) -> &'static str {
    match e.kind {
        EntryKind::File => tr("files"),
        EntryKind::Dir => tr("folders"),
        EntryKind::Symlink { .. } => tr("symbolic links"),
        EntryKind::Other => tr("special files"),
    }
}

fn kind_text(entries: &[Entry]) -> String {
    match entries {
        [e] => kind_one(e).to_owned(),
        [first, rest @ ..] if rest.iter().all(|e| kind_many(e) == kind_many(first)) => {
            format!("{} {}", entries.len(), kind_many(first))
        }
        _ => format!("{} {}", entries.len(), tr("items")),
    }
}

/// `1.5 KiB (1 536 B)`; small sizes are shown once.
fn size_line(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    format!(
        "{} ({} B)",
        format_size(bytes, SizeFormat::Binary),
        format_size(bytes, SizeFormat::Bytes)
    )
}

fn row(grid: &gtk::Grid, r: i32, caption: &str, value: &gtk::Label) {
    grid.attach(
        &gtk::Label::builder()
            .label(caption)
            .xalign(0.0)
            .yalign(0.0)
            .css_classes(["dim-label"])
            .build(),
        0,
        r,
        1,
        1,
    );
    value.set_xalign(0.0);
    value.set_hexpand(true);
    value.set_selectable(true);
    value.set_wrap(true);
    grid.attach(value, 1, r, 1, 1);
}

/// Show properties of the selection / item under the cursor.
pub fn present(ctx: &Rc<AppContext>) {
    let Some(targets) = Targets::from_active(ctx) else {
        return;
    };
    let Some(controller) = jobs::controller(ctx) else {
        return;
    };
    let paths = targets.local_paths();
    let entries = targets.entries.clone();

    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Properties"));
    dialog.set_content_width(480);
    let column = modal::padded_column(10);

    // ---- facts ----------------------------------------------------------------------------------
    let facts = gtk::Grid::new();
    facts.set_row_spacing(4);
    facts.set_column_spacing(12);
    let name = gtk::Label::new(Some(&match entries.as_slice() {
        [e] => e.name.to_string_lossy().into_owned(),
        _ => targets.describe(),
    }));
    name.add_css_class("title-4");
    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    row(&facts, 0, tr("Name"), &name);
    let place = gtk::Label::new(Some(&targets.dir.display_lossy()));
    row(&facts, 1, tr("Location"), &place);
    let kind = gtk::Label::new(Some(&kind_text(&entries)));
    row(&facts, 2, tr("Type"), &kind);
    let known: u64 = entries
        .iter()
        .filter(|e| e.kind != EntryKind::Dir)
        .map(|e| e.size)
        .sum();
    let size = gtk::Label::new(Some(&size_line(known)));
    size.set_widget_name("ofm-props-size");
    row(&facts, 3, tr("Size"), &size);
    if let [e] = entries.as_slice() {
        let modified = gtk::Label::new(Some(&format_time(e.mtime_ns, "%Y-%m-%d %H:%M:%S")));
        row(&facts, 4, tr("Modified"), &modified);
    }
    column.append(&facts);

    // Directory sizes: calculated in the background, cancelled when the dialog closes.
    if let (true, Some(paths)) = (targets.has_dirs(), paths.clone()) {
        size.set_text(tr("Calculating..."));
        let weak = size.downgrade();
        let id = controller.submit_with(
            ctx,
            JobSpec::CalcSize { paths },
            RunMode::Parallel,
            SubmitOpts {
                origin: None,
                on_finished: Some(Box::new(move |_ctx, outcome| {
                    let Some(label) = weak.upgrade() else { return };
                    label.set_text(&match outcome {
                        Outcome::Size(t) => format!(
                            "{} \u{b7} {} {}, {} {}",
                            size_line(t.bytes),
                            t.files,
                            tr("files"),
                            t.dirs,
                            tr("folders")
                        ),
                        Outcome::Cancelled(_) => String::new(),
                        Outcome::Failed { error, .. } => format!("{}: {error}", tr("Failed")),
                        Outcome::Ok(_) => String::new(),
                    });
                })),
            },
        );
        let controller = Rc::clone(&controller);
        dialog.connect_closed(move |_| {
            if controller
                .job(id)
                .is_some_and(|j| j.state != ofm_jobs::JobState::Finished)
            {
                controller.cancel(id);
            }
        });
    }

    // ---- permissions -----------------------------------------------------------------------------
    let apply = ui::button(tr("_Apply"));
    apply.set_widget_name("ofm-props-apply");
    apply.add_css_class("suggested-action");
    let cancel = ui::button(tr("Cancel"));
    let error = ui::error_label();
    error.set_widget_name("ofm-props-error");

    let editable = paths.is_some();
    let chmod_entries: Vec<&Entry> = entries
        .iter()
        .filter(|e| !matches!(e.kind, EntryKind::Symlink { .. }))
        .collect();
    let initial = initial_bits(
        &chmod_entries
            .iter()
            .map(|e| e.mode & 0o7777)
            .collect::<Vec<_>>(),
    );
    let current = Rc::new(RefCell::new(initial));
    let busy = Rc::new(Cell::new(false)); // true while we set widgets ourselves
    let octal = gtk::Entry::new();
    octal.set_widget_name("ofm-props-octal");
    octal.set_max_length(4);
    octal.set_width_chars(6);
    octal.update_property(&[gtk::accessible::Property::Label(tr("Permissions in octal"))]);
    let mut checks: Vec<gtk::CheckButton> = Vec::new();

    if editable && !chmod_entries.is_empty() {
        column.append(
            &gtk::Label::builder()
                .label(tr("Permissions"))
                .xalign(0.0)
                .css_classes(["heading"])
                .build(),
        );
        let grid = gtk::Grid::new();
        grid.set_row_spacing(4);
        grid.set_column_spacing(16);
        for (c, title) in [tr("Read"), tr("Write"), tr("Execute")].iter().enumerate() {
            grid.attach(
                &gtk::Label::builder()
                    .label(*title)
                    .css_classes(["dim-label"])
                    .build(),
                c as i32 + 1,
                0,
                1,
                1,
            );
        }
        for (r, title) in [tr("Owner"), tr("Group"), tr("Others")].iter().enumerate() {
            grid.attach(
                &gtk::Label::builder()
                    .label(*title)
                    .xalign(0.0)
                    .hexpand(true)
                    .build(),
                0,
                r as i32 + 1,
                1,
                1,
            );
        }
        let who = ["owner", "group", "others"];
        let what = ["read", "write", "execute"];
        let special = [
            (tr("Set _user ID"), "setuid"),
            (tr("Set _group ID"), "setgid"),
            (tr("S_ticky"), "sticky"),
        ];
        for i in 0..12 {
            let check = if i < 9 {
                let c = gtk::CheckButton::new();
                c.update_property(&[gtk::accessible::Property::Label(&format!(
                    "{} {}",
                    who[i / 3],
                    what[i % 3]
                ))]);
                grid.attach(&c, (i % 3) as i32 + 1, (i / 3) as i32 + 1, 1, 1);
                c
            } else {
                let (label, _) = special[i - 9];
                let c = gtk::CheckButton::with_mnemonic(label);
                grid.attach(&c, (i - 9) as i32 + 1, 4, 1, 1);
                c
            };
            check.set_widget_name(&format!("ofm-props-bit{i}"));
            checks.push(check);
        }
        let specials = gtk::Label::builder()
            .label(tr("Special"))
            .xalign(0.0)
            .build();
        grid.attach(&specials, 0, 4, 1, 1);
        column.append(&grid);

        let octal_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        octal_row.append(&gtk::Label::new(Some(tr("Octal:"))));
        octal_row.append(&octal);
        column.append(&octal_row);

        let show = {
            let (checks, octal, current, busy) = (
                checks.clone(),
                octal.clone(),
                Rc::clone(&current),
                Rc::clone(&busy),
            );
            Rc::new(move |rewrite_octal: bool| {
                busy.set(true);
                let bits = *current.borrow();
                for (c, b) in checks.iter().zip(bits) {
                    c.set_inconsistent(b == Bit::Mixed);
                    c.set_active(b == Bit::On);
                }
                if rewrite_octal {
                    octal.set_text(&octal_of(&bits).map_or(String::new(), |m| format!("{m:04o}")));
                }
                busy.set(false);
            })
        };
        show(true);
        for (i, c) in checks.iter().enumerate() {
            let (current, busy, octal) = (Rc::clone(&current), Rc::clone(&busy), octal.clone());
            c.connect_toggled(move |c| {
                if busy.get() {
                    return;
                }
                busy.set(true);
                c.set_inconsistent(false);
                let bits = {
                    let mut bits = current.borrow_mut();
                    bits[i] = if c.is_active() { Bit::On } else { Bit::Off };
                    *bits
                };
                octal.set_text(&octal_of(&bits).map_or(String::new(), |m| format!("{m:04o}")));
                busy.set(false);
            });
        }
        {
            let (show, current, busy) = (Rc::clone(&show), Rc::clone(&current), Rc::clone(&busy));
            octal.connect_changed(move |e| {
                if busy.get() {
                    return;
                }
                if let Some(mode) = parse_octal(&e.text()) {
                    *current.borrow_mut() = bits_from_mode(mode);
                    // Redraw the check boxes without rewriting the text being typed.
                    show(false);
                }
            });
        }
    } else if !editable {
        column.append(&ui::caption(tr(
            "Items inside archives are read-only; their attributes cannot be changed.",
        )));
    }

    // ---- owner / group --------------------------------------------------------------------------
    let owner = gtk::Entry::new();
    owner.set_widget_name("ofm-props-owner");
    owner.update_property(&[gtk::accessible::Property::Label(tr("Owner"))]);
    let group = gtk::Entry::new();
    group.set_widget_name("ofm-props-group");
    group.update_property(&[gtk::accessible::Property::Label(tr("Group"))]);
    let uids: Vec<u32> = entries.iter().map(|e| e.uid).collect();
    let gids: Vec<u32> = entries.iter().map(|e| e.gid).collect();
    let same = |v: &[u32]| v.first().filter(|f| v.iter().all(|x| x == *f)).copied();
    let (uid0, gid0) = (same(&uids), same(&gids));
    let initial_owner = Rc::new(RefCell::new(uid0.map_or(String::new(), |u| u.to_string())));
    let initial_group = Rc::new(RefCell::new(gid0.map_or(String::new(), |g| g.to_string())));
    owner.set_text(&initial_owner.borrow());
    group.set_text(&initial_group.borrow());
    owner.set_placeholder_text(Some(tr("(mixed)")));
    group.set_placeholder_text(Some(tr("(mixed)")));
    let can_chown = editable && idnames::is_root();
    owner.set_sensitive(can_chown);
    group.set_sensitive(can_chown);
    let names: Rc<RefCell<Option<Arc<IdNames>>>> = Rc::new(RefCell::new(None));
    if editable {
        let grid = gtk::Grid::new();
        grid.set_row_spacing(4);
        grid.set_column_spacing(12);
        grid.attach(
            &gtk::Label::builder().label(tr("Owner")).xalign(0.0).build(),
            0,
            0,
            1,
            1,
        );
        grid.attach(&owner, 1, 0, 1, 1);
        grid.attach(
            &gtk::Label::builder().label(tr("Group")).xalign(0.0).build(),
            0,
            1,
            1,
            1,
        );
        grid.attach(&group, 1, 1, 1, 1);
        owner.set_hexpand(true);
        group.set_hexpand(true);
        column.append(&grid);
        if !can_chown {
            column.append(&ui::caption(tr(
                "Changing owner and group needs administrator rights.",
            )));
        }
        // Names are looked up on a worker; the ids are replaced by names when they arrive.
        let (names, owner, group) = (Rc::clone(&names), owner.clone(), group.clone());
        let (io, ig) = (Rc::clone(&initial_owner), Rc::clone(&initial_group));
        wiring::attach(
            idnames::load(&ctx.services.pool),
            move |loaded| {
                if let Some(u) = uid0 {
                    let n = loaded.user_name(u);
                    if owner.text() == io.borrow().as_str() {
                        owner.set_text(&n);
                    }
                    *io.borrow_mut() = n;
                }
                if let Some(g) = gid0 {
                    let n = loaded.group_name(g);
                    if group.text() == ig.borrow().as_str() {
                        group.set_text(&n);
                    }
                    *ig.borrow_mut() = n;
                }
                *names.borrow_mut() = Some(loaded);
                std::ops::ControlFlow::Break(())
            },
            || {},
        );
    }

    let recursive = gtk::CheckButton::with_mnemonic(tr("Apply to _contents of folders"));
    recursive.set_widget_name("ofm-props-recursive");
    if editable && targets.has_dirs() {
        column.append(&recursive);
    }
    column.append(&error);
    column.append(&ui::button_row(&[&cancel, &apply]));
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&apply));
    apply.set_sensitive(editable);

    // ---- apply ----------------------------------------------------------------------------------
    let submit: Rc<dyn Fn() -> bool> = {
        let (ctx, error, recursive) = (Rc::clone(ctx), error.clone(), recursive.clone());
        let (current, names) = (Rc::clone(&current), Rc::clone(&names));
        let (owner, group) = (owner.clone(), group.clone());
        Rc::new(move || {
            let Some(paths) = paths.clone() else {
                return true;
            };
            let mode = mode_change(&initial, &current.borrow());
            let owner_change = if can_chown {
                let Some(table) = names.borrow().clone() else {
                    ui::set_error(&error, Some(tr("User names are still loading, try again.")));
                    return false;
                };
                match owner_change(
                    &table,
                    &owner.text(),
                    &initial_owner.borrow(),
                    &group.text(),
                    &initial_group.borrow(),
                ) {
                    Ok(c) => c,
                    Err(msg) => {
                        ui::set_error(&error, Some(&msg));
                        return false;
                    }
                }
            } else {
                None
            };
            if mode.is_some() || owner_change.is_some() {
                controller.submit_with(
                    &ctx,
                    JobSpec::SetAttrs {
                        paths,
                        mode,
                        owner: owner_change,
                        recursive: recursive.is_active(),
                    },
                    RunMode::Parallel,
                    SubmitOpts::default(),
                );
            }
            true
        })
    };
    let weak = dialog.downgrade();
    {
        let (submit, weak) = (Rc::clone(&submit), weak.clone());
        apply.connect_clicked(move |_| {
            if submit()
                && let Some(d) = weak.upgrade()
            {
                d.close();
            }
        });
    }
    cancel.connect_clicked(move |_| {
        if let Some(d) = weak.upgrade() {
            d.close();
        }
    });
    let ctx_ok = Rc::clone(ctx);
    modal::present(ctx, &dialog, move || {
        if ui::activate_focused_button(&ctx_ok) {
            return false;
        }
        submit()
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn bits_over_one_and_many_items() {
        let one = initial_bits(&[0o644]);
        assert_eq!(one[0], Bit::On); // owner read
        assert_eq!(one[2], Bit::Off); // owner execute
        assert_eq!(octal_of(&one), Some(0o644));
        let many = initial_bits(&[0o644, 0o755]);
        assert_eq!(many[2], Bit::Mixed); // owner execute differs
        assert_eq!(many[0], Bit::On);
        assert_eq!(octal_of(&many), None);
        assert_eq!(octal_of(&initial_bits(&[])), Some(0));
    }

    #[test]
    fn only_changed_bits_are_sent() {
        let initial = initial_bits(&[0o644, 0o755]);
        // Unchanged: nothing to do.
        assert_eq!(mode_change(&initial, &initial), None);
        // Make group-write on, owner-execute explicitly on (it was mixed), others-read off.
        let mut now = initial;
        now[4] = Bit::On;
        now[2] = Bit::On;
        now[6] = Bit::Off;
        let c = mode_change(&initial, &now).unwrap();
        assert_eq!(c.set, 0o020 | 0o100);
        assert_eq!(c.clear, 0o004);
        // Leaving a mixed bit mixed sends nothing for it.
        let mut same = initial;
        same[0] = Bit::On;
        assert_eq!(mode_change(&initial, &same), None);
    }

    #[test]
    fn octal_parsing() {
        assert_eq!(parse_octal("644"), Some(0o644));
        assert_eq!(parse_octal(" 4755 "), Some(0o4755));
        assert_eq!(parse_octal("0"), Some(0));
        assert_eq!(parse_octal(""), None);
        assert_eq!(parse_octal("888"), None);
        assert_eq!(parse_octal("12345"), None);
        assert_eq!(parse_octal("rw-"), None);
        assert_eq!(octal_of(&bits_from_mode(0o7777)), Some(0o7777));
        assert_eq!(octal_of(&bits_from_mode(0o1755)), Some(0o1755));
    }

    #[test]
    fn owner_text_resolution() {
        let names =
            IdNames::from_texts("root:x:0:0\nbob:x:1000:1000\n", "root:x:0:\nstaff:x:50:\n");
        // Untouched fields mean no change.
        assert_eq!(
            owner_change(&names, "bob", "bob", "staff", "staff"),
            Ok(None)
        );
        assert_eq!(
            owner_change(&names, "root", "bob", "staff", "staff"),
            Ok(Some(OwnerChange {
                uid: Some(0),
                gid: None
            }))
        );
        assert_eq!(
            owner_change(&names, "", "bob", "50", "root"),
            Ok(Some(OwnerChange {
                uid: None,
                gid: Some(50)
            }))
        );
        assert!(owner_change(&names, "ghost", "bob", "", "").is_err());
    }
}
