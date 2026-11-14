//! Drive / mount menu (F08): Alt+F1 for the left panel, Alt+F2 for the right one.
//!
//! Lists Home, `/`, the Trash, mounted volumes and GVfs mounts that have a local path (the FUSE
//! mount GNOME makes below `/run/user/*/gvfs`), volumes that can still be mounted (picking one
//! mounts it through `gtk::MountOperation`, then opens it) and the hotlist bookmarks.
//! `gio::VolumeMonitor` only reads in-memory state; mounting is asynchronous.

use std::path::PathBuf;
use std::rc::Rc;

use gtk::gio;
use gtk::gio::prelude::*;
use ofm_core::{Location, cmd};

use super::chooser::{self, ChoiceRow, ChooserSpec};
use super::goto::navigate;
use super::store::HistoryStore;
use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::i18n::tr;
use crate::panel::Side;

/// Register `panel.drive_menu_left/right`.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::PANEL_DRIVE_MENU_LEFT, |ctx| show(ctx, 0));
    reg.register(cmd::PANEL_DRIVE_MENU_RIGHT, |ctx| show(ctx, 1));
}

/// What picking a row does.
#[derive(Clone)]
pub enum Target {
    /// Go to a directory.
    Dir(PathBuf),
    /// Mount this volume, then go to its root.
    Mount(gio::Volume),
}

/// One row of the menu.
#[derive(Clone)]
pub struct DriveItem {
    /// Title and subtitle shown.
    pub row: ChoiceRow,
    /// Action on pick.
    pub target: Target,
}

fn dir_item(title: &str, path: PathBuf) -> DriveItem {
    DriveItem {
        row: ChoiceRow {
            title: title.to_owned(),
            subtitle: Some(path.to_string_lossy().into_owned()),
        },
        target: Target::Dir(path),
    }
}

/// Collect the menu rows (main thread: only in-memory state is read).
pub fn collect(ctx: &Rc<AppContext>) -> Vec<DriveItem> {
    let mut items = vec![
        dir_item(tr("Home"), gtk::glib::home_dir()),
        dir_item(tr("Root"), PathBuf::from("/")),
        dir_item(tr("Trash"), gtk::glib::user_data_dir().join("Trash/files")),
    ];
    let monitor = gio::VolumeMonitor::get();
    for mount in monitor.mounts() {
        if mount.is_shadowed() {
            continue;
        }
        match mount.root().path() {
            Some(path) => items.push(dir_item(&mount.name(), path)),
            None => tracing::debug!(name = %mount.name(), "mount without local path skipped"),
        }
    }
    for volume in monitor.volumes() {
        if volume.get_mount().is_none() && volume.can_mount() {
            items.push(DriveItem {
                row: ChoiceRow {
                    title: format!("{} {}", tr("Mount"), volume.name()),
                    subtitle: Some(tr("Not mounted").to_owned()),
                },
                target: Target::Mount(volume),
            });
        }
    }
    let store = HistoryStore::ensure(ctx);
    for b in store.snapshot().hotlist {
        if let Some(path) = b.path_buf() {
            items.push(dir_item(&b.title, path));
        }
    }
    items
}

fn show(ctx: &Rc<AppContext>, side: Side) {
    ctx.set_active_side(side);
    let items = collect(ctx);
    let rows = items.iter().map(|i| i.row.clone()).collect();
    let on_pick = Rc::new(move |ctx: &Rc<AppContext>, idx: usize| {
        if let Some(item) = items.get(idx) {
            pick(ctx, side, item.target.clone());
        }
    });
    chooser::open(
        ctx,
        ChooserSpec {
            title: "Drives and mounts",
            rows,
            selected: 0,
            empty_text: "Nothing to show",
            on_pick,
            buttons: Vec::new(),
        },
    );
}

fn pick(ctx: &Rc<AppContext>, side: Side, target: Target) {
    match target {
        Target::Dir(path) => {
            let ctrl = ctx.host(side).active();
            navigate(ctx, &ctrl, Location::Local(path), None);
        }
        Target::Mount(volume) => mount_and_open(ctx, side, volume),
    }
}

fn mount_and_open(ctx: &Rc<AppContext>, side: Side, volume: gio::Volume) {
    let op = gtk::MountOperation::new(ctx.window().map(|w| &w.window));
    let ctx = Rc::clone(ctx);
    gtk::glib::spawn_future_local(async move {
        match volume
            .mount_future(gio::MountMountFlags::NONE, Some(&op))
            .await
        {
            Ok(()) => match volume.get_mount().and_then(|m| m.root().path()) {
                Some(path) => {
                    let ctrl = ctx.host(side).active();
                    navigate(&ctx, &ctrl, Location::Local(path), None);
                }
                None => tracing::warn!("mounted volume has no local path"),
            },
            Err(e) => tracing::warn!(error = %e, "mounting failed"),
        }
    });
}
