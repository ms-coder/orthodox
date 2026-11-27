//! M9 (review finding m1): permanent delete, recursive chmod and the source side of a move walk
//! through directory handles. A directory or file swapped for a symlink (or a directory moved
//! away) between the check and the action must not redirect it.
//!
//! Each race is staged from inside the job: a hook on the n-th call of one operation changes the
//! tree just before that call is performed. Everything lives in temp dirs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod t32_support;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ofm_core::Location;
use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{CopyOpts, ErrorAction, Fs, JobSpec, ModeChange, Outcome, OwnerChange, run_sync};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::temp::TempDir;
use t32_support::{TestFs, error_policy, no_questions, questions, report};

const EXDEV: i32 = 18;

/// A `TestFs` that runs `change` once, just before the `nth` call of `op` (1-based).
fn racing(op: &'static str, nth: usize, change: impl Fn() + Send + Sync + 'static) -> TestFs {
    let calls = AtomicUsize::new(0);
    TestFs::real().hooked(move |o| {
        if o == op && calls.fetch_add(1, Ordering::SeqCst) + 1 == nth {
            change();
        }
    })
}

/// Move `path` to `aside` and put a symlink to `target` in its place.
fn swap_for_link(path: &Path, aside: &Path, target: &Path) {
    fs::rename(path, aside).unwrap();
    symlink(target, path).unwrap();
}

struct World {
    t: TempDir,
}

impl World {
    /// `tree/d/{a,b}` (what the job works on) and `outside/{a,b}` (what it must never touch).
    fn new() -> World {
        let t = TempDir::new().unwrap();
        for dir in ["tree/d", "outside", "dest"] {
            fs::create_dir_all(t.path().join(dir)).unwrap();
        }
        // No group/other bits: a recursive chmod has something to change on every object.
        for dir in ["tree", "tree/d", "outside"] {
            set_perm(&t.path().join(dir), 0o700);
        }
        for (file, text) in [
            ("tree/d/a", "mine a"),
            ("tree/d/b", "mine b"),
            ("outside/a", "theirs a"),
            ("outside/b", "theirs b"),
        ] {
            fs::write(t.path().join(file), text).unwrap();
            set_perm(&t.path().join(file), 0o600);
        }
        World { t }
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.t.path().join(rel)
    }

    /// Swap `rel` for a symlink to `to` when run.
    fn swap(&self, rel: &str, to: &str) -> impl Fn() + Send + Sync + 'static {
        let (path, aside, to) = (self.p(rel), self.p("aside"), self.p(to));
        move || swap_for_link(&path, &aside, &to)
    }

    fn assert_outside_untouched(&self) {
        for (file, text) in [("outside/a", "theirs a"), ("outside/b", "theirs b")] {
            assert_eq!(fs::read_to_string(self.p(file)).unwrap(), text, "{file}");
            assert_eq!(perm(&self.p(file)), 0o600, "{file}");
        }
        assert_eq!(fs::read_dir(self.p("outside")).unwrap().count(), 2);
        assert_eq!(perm(&self.p("outside")), 0o700);
    }
}

fn set_perm(p: &Path, mode: u32) {
    fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
}

fn perm(p: &Path) -> u32 {
    fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
}

fn run(spec: &JobSpec, fs: impl Fs + 'static) -> (Outcome, usize) {
    let (sink, rec) = error_policy(ErrorAction::Skip);
    let out = run_sync(spec, Arc::new(fs), sink);
    let asked = questions(&rec).len();
    (out, asked)
}

fn delete(path: PathBuf) -> JobSpec {
    JobSpec::Delete {
        paths: vec![path],
        to_trash: false,
    }
}

fn chmod_r(path: PathBuf) -> JobSpec {
    JobSpec::SetAttrs {
        paths: vec![path],
        mode: Some(ModeChange {
            set: 0o044,
            clear: 0,
        }),
        owner: None,
        recursive: true,
    }
}

fn move_into(src: PathBuf, dest_dir: PathBuf) -> JobSpec {
    JobSpec::Move {
        sources: vec![Location::Local(src)],
        dest_dir,
        opts: CopyOpts::default(),
    }
}

// ------------------------------------------------------------------------------------- delete

/// Between `lstat` (a directory) and the listing. `read_dir` #1/#2 count the plan, #3 lists
/// `tree`, #4 lists `tree/d`.
#[test]
fn delete_does_not_follow_a_directory_swapped_before_the_listing() {
    let w = World::new();
    let fs_ = racing("read_dir", 4, w.swap("tree/d", "outside"));
    let (out, asked) = run(&delete(w.p("tree")), fs_);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(asked > 0);
    w.assert_outside_untouched();
}

/// Between the listing and the first `unlink`.
#[test]
fn delete_does_not_follow_a_directory_swapped_before_an_unlink() {
    let w = World::new();
    let fs_ = racing("unlink", 1, w.swap("tree/d", "outside"));
    let (out, asked) = run(&delete(w.p("tree")), fs_);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(asked > 0, "the second file must be refused");
    w.assert_outside_untouched();
    // The unlink already under way hit the directory that was checked, not the link.
    assert_eq!(fs::read_dir(w.p("aside")).unwrap().count(), 1);
    assert!(fs::symlink_metadata(w.p("tree/d")).is_ok(), "left alone");
}

/// The selected directory itself, not something below it.
#[test]
fn delete_does_not_follow_the_selected_directory_swapped() {
    let w = World::new();
    // `read_dir` #1 counts the plan, #2 lists the selected `tree/d`.
    let fs_ = racing("read_dir", 2, w.swap("tree/d", "outside"));
    let (_, asked) = run(&delete(w.p("tree/d")), fs_);
    assert!(asked > 0);
    w.assert_outside_untouched();
}

/// Not a symlink: the directory is moved out of the selection while it is being emptied. What
/// has left the selected tree is not deleted.
#[test]
fn delete_stops_at_a_directory_that_was_moved_away() {
    let w = World::new();
    let (from, to) = (w.p("tree/d"), w.p("aside"));
    let fs_ = racing("unlink", 1, move || fs::rename(&from, &to).unwrap());
    let (out, _) = run(&delete(w.p("tree")), fs_);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(fs::read_dir(w.p("aside")).unwrap().count(), 1);
}

/// The folder holding the selected items is swapped between two of them.
#[test]
fn delete_does_not_follow_the_parent_of_the_selection() {
    let w = World::new();
    let fs_ = racing("unlink", 1, w.swap("tree/d", "outside"));
    let spec = JobSpec::Delete {
        paths: vec![w.p("tree/d/a"), w.p("tree/d/b")],
        to_trash: false,
    };
    let (_, asked) = run(&spec, fs_);
    assert!(asked > 0);
    w.assert_outside_untouched();
    assert_eq!(fs::read(w.p("aside/b")).unwrap(), b"mine b");
}

#[test]
fn delete_of_a_path_ending_in_dot_dot_is_refused() {
    let w = World::new();
    let (sink, _) = no_questions();
    let out = run_sync(&delete(w.p("tree/d/..")), Arc::new(TestFs::real()), sink);
    assert!(matches!(out, Outcome::Failed { .. }), "{out:?}");
    assert!(w.p("tree/d/a").exists());
}

/// The walk keeps a bounded number of handles; deeper trees than that are still deleted.
#[test]
fn delete_and_chmod_a_tree_deeper_than_the_handle_cache() {
    let t = TempDir::new().unwrap();
    let top = t.path().join("top");
    let mut p = top.clone();
    for i in 0..300 {
        p.push(format!("d{i}"));
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("f"), b"x").unwrap();
        fs::write(p.parent().unwrap().join("z"), b"x").unwrap();
    }
    let (sink, rec) = no_questions();
    let out = run_sync(&chmod_r(top.clone()), Arc::new(TestFs::real()), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(questions(&rec).is_empty());
    assert_eq!(perm(&p.join("f")) & 0o044, 0o044);
    let (sink, rec) = no_questions();
    let out = run_sync(&delete(top.clone()), Arc::new(TestFs::real()), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(questions(&rec).is_empty());
    assert_eq!(report(&out).warnings.len(), 0);
    assert!(!top.exists());
}

// -------------------------------------------------------------------------------------- chmod

/// A file replaced by a symlink between `lstat` and `chmod`. `set_mode` #1 is `tree/d`, #2 its
/// first file.
#[test]
fn chmod_does_not_follow_a_file_swapped_for_a_symlink() {
    let w = World::new();
    fs::remove_file(w.p("tree/d/b")).unwrap();
    let fs_ = racing("set_mode", 2, w.swap("tree/d/a", "outside/a"));
    let (out, _) = run(&chmod_r(w.p("tree/d")), fs_);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    w.assert_outside_untouched();
    assert_eq!(perm(&w.p("aside")), 0o644, "the checked file was changed");
}

/// The selected item itself.
#[test]
fn chmod_does_not_follow_the_selected_file_swapped_for_a_symlink() {
    let w = World::new();
    let fs_ = racing("set_mode", 1, w.swap("tree/d/a", "outside/a"));
    let (out, _) = run(&chmod_r(w.p("tree/d/a")), fs_);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    w.assert_outside_untouched();
}

/// A directory replaced between its own `chmod` and the listing. `read_dir` #1 lists `tree`,
/// #2 lists `tree/d`.
#[test]
fn recursive_chmod_does_not_follow_a_swapped_directory() {
    let w = World::new();
    let fs_ = racing("read_dir", 2, w.swap("tree/d", "outside"));
    let (out, asked) = run(&chmod_r(w.p("tree")), fs_);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(asked > 0);
    w.assert_outside_untouched();
}

// --------------------------------------------------------------------------------------- move

/// Cross-device move (the first rename answers `EXDEV`): `tree/d` swapped before its listing.
fn cross_device_move(w: &World, change: TestFs) -> (Outcome, usize) {
    let plan = FaultPlan::new().fail_nth(FaultOp::RenameNoreplace, 1, FaultErrno::Raw(EXDEV));
    let fs_ = FaultyFs::new(Arc::new(change), plan);
    run(&move_into(w.p("tree"), w.p("dest")), fs_)
}

#[test]
fn move_across_devices_does_not_remove_what_a_swapped_directory_points_to() {
    let w = World::new();
    // `read_dir` #1/#2 count the totals, #3 lists `tree`, #4 lists `tree/d`.
    let fs_ = racing("read_dir", 4, w.swap("tree/d", "outside"));
    let (_, asked) = cross_device_move(&w, fs_);
    assert!(asked > 0);
    w.assert_outside_untouched();
    // Nothing from outside was even copied.
    assert!(!w.p("dest/tree/d/a").exists());
    assert_eq!(fs::read(w.p("aside/a")).unwrap(), b"mine a");
}

/// Swapped between the identity check of a source and its removal.
#[test]
fn move_across_devices_removes_only_from_the_checked_directory() {
    let w = World::new();
    let fs_ = racing("unlink", 1, w.swap("tree/d", "outside"));
    let (_, asked) = cross_device_move(&w, fs_);
    assert!(asked > 0);
    w.assert_outside_untouched();
    // `a` was copied, then removed from the checked directory; `b` stays.
    assert_eq!(fs::read(w.p("dest/tree/d/a")).unwrap(), b"mine a");
    assert_eq!(fs::read(w.p("aside/b")).unwrap(), b"mine b");
}

/// Same device, merging into an existing folder: entries are renamed one by one.
#[test]
fn merging_move_does_not_move_out_of_a_swapped_directory() {
    let w = World::new();
    fs::create_dir(w.p("dest/d")).unwrap();
    let fs_ = racing("read_dir", 1, w.swap("tree/d", "outside"));
    let (_, asked) = run(&move_into(w.p("tree/d"), w.p("dest")), fs_);
    assert!(asked > 0);
    w.assert_outside_untouched();
    assert_eq!(fs::read_dir(w.p("dest/d")).unwrap().count(), 0);
}

/// A real `chown` through the handles (only groups can be changed without privilege): files,
/// folders and links get the group, a link's target outside does not.
#[test]
fn recursive_chown_changes_the_tree_and_no_link_target() {
    let w = World::new();
    symlink(w.p("outside/a"), w.p("tree/link")).unwrap();
    let gid_of = |rel: &str| fs::symlink_metadata(w.p(rel)).unwrap().gid();
    let before = gid_of("tree");
    let groups = rustix::process::getgroups().unwrap();
    let Some(group) = groups.iter().map(|g| g.as_raw()).find(|&g| g != before) else {
        eprintln!("SKIP: no supplementary group");
        return;
    };
    let spec = JobSpec::SetAttrs {
        paths: vec![w.p("tree")],
        mode: None,
        owner: Some(OwnerChange {
            uid: None,
            gid: Some(group),
        }),
        recursive: true,
    };
    let (sink, rec) = no_questions();
    let out = run_sync(&spec, Arc::new(TestFs::real()), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(questions(&rec).is_empty());
    for rel in ["tree", "tree/d", "tree/d/a", "tree/d/b", "tree/link"] {
        assert_eq!(gid_of(rel), group, "{rel}");
    }
    assert_eq!(gid_of("outside/a"), before);
}
