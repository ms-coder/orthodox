//! Integration tests for the list/panelize provider (T7.2, F25, ADR-0009):
//! `PanelList::from_paths`, `ListStore` real-path mapping and `ListVfs` refresh re-stat.
//! Rows must map to the real files so F5/F6/F8 operate on them.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::sync::Arc;

use ofm_core::{CancelToken, EntryKind, ListId, Location, TargetKind};
use ofm_vfs::list::{ListStore, ListVfs, PanelList};
use ofm_vfs::vfs::{ListEvent, Vfs};

fn collect(vfs: &ListVfs, loc: &Location) -> (Vec<ofm_core::Entry>, Option<usize>) {
    let mut events = Vec::new();
    vfs.list(loc, &CancelToken::new(), &mut |ev| events.push(ev));
    let entries = events
        .iter()
        .filter_map(|ev| match ev {
            ListEvent::Batch(b) => Some(b.iter().cloned()),
            _ => None,
        })
        .flatten()
        .collect();
    let total = events.iter().find_map(|ev| match ev {
        ListEvent::Done { total } => Some(*total),
        _ => None,
    });
    (entries, total)
}

#[test]
fn from_paths_builds_rows_over_real_files() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let sub = root.join("sub dir");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(root.join("plain"), b"12345").unwrap();
    std::fs::write(sub.join("nested"), b"x").unwrap();
    std::fs::write(root.join("-dash"), b"x").unwrap();
    let weird = root.join(OsString::from_vec(b"w\xffid".to_vec()));
    std::fs::write(&weird, b"x").unwrap();
    std::os::unix::fs::symlink("plain", root.join("link")).unwrap();
    std::os::unix::fs::symlink("gone", root.join("broken")).unwrap();

    let paths = vec![
        root.join("plain"),
        root.join("sub dir").join("nested"),
        root.join("-dash"),
        weird.clone(),
        root.join("link"),
        root.join("broken"),
        root.join("plain"), // duplicate: dropped, not counted
        root.join("does-not-exist"),
    ];
    let (list, ignored) = PanelList::from_paths(
        "Panelize: test",
        Some(root.to_path_buf()),
        paths,
        &CancelToken::new(),
    )
    .unwrap();
    assert_eq!(ignored, 1, "only the missing path is counted");
    assert_eq!(
        list.entries.len(),
        6,
        "input order preserved, duplicate removed"
    );

    // Display names are relative to base; identity is the full real path.
    assert_eq!(list.entries[0].name, OsStr::new("plain"));
    assert_eq!(list.entries[0].real_path, Some(root.join("plain")));
    assert_eq!(list.entries[1].name, OsStr::new("sub dir/nested"));
    assert_eq!(list.entries[1].kind, EntryKind::File);
    assert_eq!(list.entries[1].size, 1);
    // Non-UTF-8 name survives as bytes.
    assert_eq!(list.entries[3].name, weird.file_name().unwrap());
    // lstat semantics: symlinks not followed, broken links shown as such.
    assert_eq!(
        list.entries[4].kind,
        EntryKind::Symlink {
            target: Some(TargetKind::File)
        }
    );
    assert_eq!(list.entries[5].kind, EntryKind::Symlink { target: None });
    // Real metadata landed on the rows.
    assert!(list.entries[0].mtime_ns > 0);
    assert_ne!(list.entries[0].mode, 0);
}

#[test]
fn from_paths_without_base_uses_full_paths_and_counts_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("a"), b"").unwrap();
    let (list, ignored) = PanelList::from_paths(
        "t",
        None,
        vec![root.join("a"), root.join("missing"), root.join("missing2")],
        &CancelToken::new(),
    )
    .unwrap();
    assert_eq!(ignored, 2);
    assert_eq!(list.entries.len(), 1);
    assert_eq!(list.entries[0].name, OsStr::new(root.join("a").as_os_str()));
}

#[test]
fn store_maps_names_to_real_paths_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("a"), b"").unwrap();
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("b"), b"").unwrap();

    let store = Arc::new(ListStore::default());
    let (list, _) = PanelList::from_paths(
        "Find: *",
        Some(root.to_path_buf()),
        vec![root.join("a"), sub.join("b")],
        &CancelToken::new(),
    )
    .unwrap();
    let id: ListId = store.create(list);
    let loc = Location::List { id };

    // Single-row and bulk resolution agree; unknown names are skipped.
    assert_eq!(
        store.real_path(id, OsStr::new("a")).unwrap(),
        Some(root.join("a"))
    );
    assert_eq!(store.real_path(id, OsStr::new("zz")).unwrap(), None);
    let bulk = store
        .real_paths(id, &[OsString::from("sub/b"), OsString::from("a")])
        .unwrap();
    assert_eq!(bulk, [sub.join("b"), root.join("a")]);

    // Simulate an F8-delete job on the real files, then the app drops the rows.
    std::fs::remove_file(root.join("a")).unwrap();
    assert_eq!(store.forget_paths(id, &[root.join("a")]), 1);
    let vfs = ListVfs::new(store.clone());
    let (entries, total) = collect(&vfs, &loc);
    assert_eq!(total, Some(1));
    assert_eq!(entries[0].real_path, Some(sub.join("b")));
    store.remove(id);
    assert!(store.get(id).is_none());
}

#[test]
fn refresh_drops_vanished_and_updates_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let f = root.join("f");
    std::fs::write(&f, b"small").unwrap();
    let g = root.join("g");
    std::fs::write(&g, b"x").unwrap();

    let store = Arc::new(ListStore::default());
    let (list, _) = PanelList::from_paths(
        "t",
        Some(root.to_path_buf()),
        vec![f.clone(), g.clone()],
        &CancelToken::new(),
    )
    .unwrap();
    let id = store.create(list);
    let loc = Location::List { id };
    let vfs = ListVfs::new(store.clone());

    // Change the world: g grows, f vanishes.
    std::fs::write(&g, b"much bigger now").unwrap();
    std::fs::remove_file(&f).unwrap();

    let (entries, total) = collect(&vfs, &loc);
    assert_eq!(total, Some(1), "vanished row dropped on refresh");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, OsStr::new("g"));
    assert_eq!(entries[0].size, 15, "fresh size after re-stat");
    assert_eq!(entries[0].real_path, Some(g.clone()));

    // The store was told to forget the vanished row too.
    let stored = store.get(id).expect("list still there");
    assert_eq!(stored.entries.len(), 1);
    assert_eq!(stored.entries[0].real_path, Some(g));
}

#[test]
fn refresh_is_cancelled_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for i in 0..1200 {
        std::fs::write(root.join(format!("f{i}")), b"").unwrap();
    }
    let store = Arc::new(ListStore::default());
    let paths: Vec<_> = (0..1200).map(|i| root.join(format!("f{i}"))).collect();
    let (list, _) =
        PanelList::from_paths("t", Some(root.to_path_buf()), paths, &CancelToken::new()).unwrap();
    let id = store.create(list);
    let loc = Location::List { id };
    let vfs = ListVfs::new(store);

    let cancel = CancelToken::new();
    cancel.cancel();
    let mut saw_cancelled = false;
    let mut saw_done = false;
    vfs.list(&loc, &cancel, &mut |ev| match ev {
        ListEvent::Error(ofm_vfs::VfsError::Cancelled) => saw_cancelled = true,
        ListEvent::Done { .. } => saw_done = true,
        _ => {}
    });
    assert!(saw_cancelled);
    assert!(!saw_done, "cancelled listing must not emit Done");
}
