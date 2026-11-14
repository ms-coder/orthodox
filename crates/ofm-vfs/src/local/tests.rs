#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use ofm_core::{CancelToken, Entry, EntryKind, Location, TargetKind};

use super::*;

fn list_all(vfs: &LocalVfs, dir: &Path) -> (Vec<Entry>, Option<VfsError>, Option<usize>, usize) {
    let mut entries = Vec::new();
    let mut err = None;
    let mut done = None;
    let mut events = 0;
    vfs.list(
        &Location::Local(dir.to_path_buf()),
        &CancelToken::new(),
        &mut |ev| {
            events += 1;
            match ev {
                ListEvent::Batch(b) => entries.extend(b),
                ListEvent::Done { total } => done = Some(total),
                ListEvent::Error(e) => err = Some(e),
            }
        },
    );
    (entries, err, done, events)
}

fn find<'a>(v: &'a [Entry], name: &str) -> &'a Entry {
    v.iter()
        .find(|e| e.name == OsStr::new(name))
        .unwrap_or_else(|| panic!("missing {name}"))
}

#[test]
fn lists_files_dirs_symlinks_and_broken_links() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    std::fs::write(d.join("f.txt"), b"hello").unwrap();
    std::fs::create_dir(d.join("sub")).unwrap();
    symlink("f.txt", d.join("lf")).unwrap();
    symlink("sub", d.join("ld")).unwrap();
    symlink("nope", d.join("broken")).unwrap();
    symlink("/dev/null", d.join("lo")).unwrap();

    let (e, err, done, _) = list_all(&LocalVfs::new(), d);
    assert!(err.is_none());
    assert_eq!(done, Some(6));
    assert_eq!(e.len(), 6);
    let f = find(&e, "f.txt");
    assert_eq!((f.kind, f.size), (EntryKind::File, 5));
    assert!(f.mtime_ns > 0);
    assert_eq!(f.mode & 0o170000, 0o100000);
    assert_eq!(find(&e, "sub").kind, EntryKind::Dir);
    assert_eq!(
        find(&e, "lf").kind,
        EntryKind::Symlink {
            target: Some(TargetKind::File)
        }
    );
    assert_eq!(
        find(&e, "ld").kind,
        EntryKind::Symlink {
            target: Some(TargetKind::Dir)
        }
    );
    assert!(find(&e, "ld").kind.is_dir_like());
    assert_eq!(
        find(&e, "lo").kind,
        EntryKind::Symlink {
            target: Some(TargetKind::Other)
        }
    );
    assert_eq!(find(&e, "broken").kind, EntryKind::Symlink { target: None });
}

#[test]
fn non_utf8_names_round_trip() {
    let t = tempfile::tempdir().unwrap();
    let name = OsStr::from_bytes(b"bad-\xff\xfe-name");
    std::fs::write(t.path().join(name), b"x").unwrap();
    let (e, err, _, _) = list_all(&LocalVfs::new(), t.path());
    assert!(err.is_none());
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].name, name);
    let st = LocalVfs::new()
        .stat(&Location::Local(t.path().join(name)))
        .unwrap();
    assert_eq!(st.name, name);
    assert_eq!(st.size, 1);
}

#[test]
fn batches_of_512_and_totals() {
    let t = tempfile::tempdir().unwrap();
    for i in 0..1300 {
        std::fs::write(t.path().join(format!("f{i}")), b"").unwrap();
    }
    let mut sizes = Vec::new();
    let mut done = None;
    LocalVfs::new().list(
        &Location::Local(t.path().into()),
        &CancelToken::new(),
        &mut |ev| match ev {
            ListEvent::Batch(b) => sizes.push(b.len()),
            ListEvent::Done { total } => done = Some(total),
            ListEvent::Error(e) => panic!("{e}"),
        },
    );
    assert_eq!(done, Some(1300));
    assert_eq!(sizes.iter().sum::<usize>(), 1300);
    assert!(sizes.iter().all(|&n| n <= BATCH_SIZE));
    assert!(sizes.len() >= 3);
}

#[test]
fn empty_dir_and_errors() {
    let t = tempfile::tempdir().unwrap();
    let vfs = LocalVfs::new();
    let (e, err, done, _) = list_all(&vfs, t.path());
    assert!(e.is_empty() && err.is_none());
    assert_eq!(done, Some(0));

    let (_, err, done, _) = list_all(&vfs, &t.path().join("missing"));
    assert!(matches!(err, Some(VfsError::NotFound(_))));
    assert!(done.is_none());

    std::fs::write(t.path().join("file"), b"").unwrap();
    let (_, err, _, _) = list_all(&vfs, &t.path().join("file"));
    assert!(matches!(err, Some(VfsError::NotADirectory(_))));

    let (_, err, _, _) = list_all(&vfs, Path::new("/"));
    assert!(err.is_none());
    let mut got = None;
    vfs.list(
        &Location::List {
            id: ofm_core::ListId(1),
        },
        &CancelToken::new(),
        &mut |ev| got = Some(ev),
    );
    assert!(matches!(
        got,
        Some(ListEvent::Error(VfsError::Unsupported(_)))
    ));
}

#[test]
fn unreadable_dir_is_permission_denied() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("locked");
    std::fs::create_dir(&d).unwrap();
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o000)).unwrap();
    let denied = std::fs::read_dir(&d).is_err(); // false when running as root
    let (_, err, done, _) = list_all(&LocalVfs::new(), &d);
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
    if denied {
        assert!(
            matches!(err, Some(VfsError::PermissionDenied(_))),
            "{err:?}"
        );
        assert!(done.is_none());
    }
}

#[test]
fn cancelled_listing_reports_cancelled() {
    let t = tempfile::tempdir().unwrap();
    for i in 0..50 {
        std::fs::write(t.path().join(format!("f{i}")), b"").unwrap();
    }
    let cancel = CancelToken::new();
    cancel.cancel();
    let mut last = None;
    LocalVfs::new().list(&Location::Local(t.path().into()), &cancel, &mut |ev| {
        last = Some(ev)
    });
    assert!(matches!(last, Some(ListEvent::Error(VfsError::Cancelled))));
}

#[test]
fn panicking_sink_still_ends_with_error_event() {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("f"), b"").unwrap();
    let mut calls = 0;
    let mut events = Vec::new();
    // The sink panics while handling the first batch (injected fault); the listing must
    // still deliver one terminal Error instead of dying silently.
    LocalVfs::new().list(
        &Location::Local(t.path().into()),
        &CancelToken::new(),
        &mut |ev| {
            calls += 1;
            if calls == 1 {
                panic!("injected");
            }
            events.push(ev);
        },
    );
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], ListEvent::Error(VfsError::Io { .. })));
}

#[test]
fn stat_does_not_follow_symlinks() {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir(t.path().join("d")).unwrap();
    symlink("d", t.path().join("l")).unwrap();
    let vfs = LocalVfs::new();
    let e = vfs.stat(&Location::Local(t.path().join("l"))).unwrap();
    assert_eq!(
        e.kind,
        EntryKind::Symlink {
            target: Some(TargetKind::Dir)
        }
    );
    assert_eq!(e.name, "l");
    assert!(matches!(
        vfs.stat(&Location::Local(t.path().join("zzz"))),
        Err(VfsError::NotFound(_))
    ));
    let root = vfs.stat(&Location::Local("/".into())).unwrap();
    assert_eq!(root.kind, EntryKind::Dir);
}

#[test]
fn open_read_reads_and_seeks() {
    use std::io::{Read, Seek, SeekFrom};
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("a"), b"0123456789").unwrap();
    let vfs = LocalVfs::new();
    let mut r = vfs.open_read(&Location::Local(t.path().join("a"))).unwrap();
    r.seek(SeekFrom::Start(7)).unwrap();
    let mut s = String::new();
    r.read_to_string(&mut s).unwrap();
    assert_eq!(s, "789");
    assert!(vfs.open_read(&Location::Local(t.path().into())).is_err());
    assert!(matches!(
        vfs.open_read(&Location::Local(t.path().join("none"))),
        Err(VfsError::NotFound(_))
    ));
}

#[test]
fn parent_and_space() {
    let vfs = LocalVfs::new();
    assert_eq!(
        vfs.parent(&Location::Local("/a/b".into())),
        Some(Location::Local("/a".into()))
    );
    assert_eq!(
        vfs.parent(&Location::Local("/a".into())),
        Some(Location::Local("/".into()))
    );
    assert_eq!(vfs.parent(&Location::Local("/".into())), None);
    let t = tempfile::tempdir().unwrap();
    let s = vfs.space(&Location::Local(t.path().into())).unwrap();
    assert!(s.total > 0 && s.free <= s.total);
    assert!(matches!(
        vfs.space(&Location::Local(t.path().join("nope"))),
        Err(VfsError::NotFound(_))
    ));
}
