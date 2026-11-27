//! M9 N5: on file systems without `RENAME_NOREPLACE` (NFS, many FUSE mounts answer `EINVAL`)
//! the trash move uses the shared no-replace fallback instead of failing for every item.
//! Never touches the real trash: every test builds its own `TrashEnv` in a temp dir.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;

use ofm_jobs::TrashError;
use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::ops::trash::{TrashEnv, trash_path_with_fs};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::temp::TempDir;

const EPERM: i32 = 1;
const EINVAL: i32 = 22;

fn env_in(t: &TempDir) -> TrashEnv {
    TrashEnv {
        home_trash: t.path().join("share/Trash"),
        uid: rustix::process::getuid().as_raw(),
    }
}

fn no_noreplace() -> FaultPlan {
    FaultPlan::new().fail_from(FaultOp::RenameNoreplace, 1, FaultErrno::Raw(EINVAL))
}

fn count(dir: &Path) -> usize {
    fs::read_dir(dir).unwrap().count()
}

#[test]
fn n5_a_file_a_symlink_and_a_folder_reach_the_trash_without_noreplace() {
    let t = TempDir::new().unwrap();
    let env = env_in(&t);
    let (f, l, d) = (t.path().join("f"), t.path().join("l"), t.path().join("d"));
    fs::write(&f, b"data").unwrap();
    std::os::unix::fs::symlink("nowhere", &l).unwrap();
    fs::create_dir(&d).unwrap();
    fs::write(d.join("x"), b"x").unwrap();

    let faulty = FaultyFs::over_real(no_noreplace());
    for p in [&f, &l, &d] {
        trash_path_with_fs(&faulty, &env, p).unwrap();
        assert!(fs::symlink_metadata(p).is_err(), "{p:?}");
    }
    let files = env.home_trash.join("files");
    assert_eq!(fs::read(files.join("f")).unwrap(), b"data");
    assert_eq!(
        fs::read_link(files.join("l")).unwrap(),
        Path::new("nowhere")
    );
    assert_eq!(fs::read(files.join("d/x")).unwrap(), b"x");
    assert_eq!(count(&env.home_trash.join("info")), 3);
    // File and symlink by hard link; the folder by the checked rename.
    assert_eq!(faulty.calls_of(FaultOp::HardLink), 2);
    assert_eq!(faulty.calls_of(FaultOp::RenameReplace), 1);
}

#[test]
fn n5_without_hard_links_the_checked_rename_is_used() {
    let t = TempDir::new().unwrap();
    let env = env_in(&t);
    let f = t.path().join("f");
    fs::write(&f, b"data").unwrap();
    let faulty =
        FaultyFs::over_real(no_noreplace().fail_from(FaultOp::HardLink, 1, FaultErrno::Raw(EPERM)));
    trash_path_with_fs(&faulty, &env, &f).unwrap();
    assert!(!f.exists());
    assert_eq!(fs::read(env.home_trash.join("files/f")).unwrap(), b"data");
}

#[test]
fn n5_a_failing_move_keeps_the_item_and_leaves_no_trace_in_the_trash() {
    let t = TempDir::new().unwrap();
    let env = env_in(&t);
    let f = t.path().join("f");
    fs::write(&f, b"data").unwrap();
    // The link is made, then the old name cannot be removed: the link is undone.
    let faulty = FaultyFs::over_real(no_noreplace().fail_nth(
        FaultOp::Unlink,
        1,
        FaultErrno::PermissionDenied,
    ));
    let err = trash_path_with_fs(&faulty, &env, &f).unwrap_err();
    assert!(matches!(&err, TrashError::Io(_)), "{err:?}");
    assert_eq!(fs::read(&f).unwrap(), b"data");
    assert_eq!(count(&env.home_trash.join("files")), 0);
    assert_eq!(count(&env.home_trash.join("info")), 0);
}
