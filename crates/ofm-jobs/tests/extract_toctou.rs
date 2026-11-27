//! FIX-M6M7 #5: a checked destination parent that is swapped for a symlink DURING extraction must
//! not redirect anything outside the destination. Deterministic: the swap happens from inside the
//! archive stream of an entry (`MemReader::on_read`), i.e. after the parent directory was checked
//! and created and before the following entries are written. Only the public job API is used, so
//! these tests also run (and fail) against the path-based code they replace.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod extract_support;

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use extract_support::*;
use ofm_core::CancelToken;
use ofm_jobs::{ConflictAnswer, JobCtx, RealFs, ScriptedSink};

/// `<tmp>/dst` is the destination, `<tmp>/outside` is where the attacker points the symlink.
struct Arena {
    tmp: Tmp,
    dst: PathBuf,
    outside: PathBuf,
}

fn arena(tag: &str) -> Arena {
    let tmp = Tmp::new(tag);
    let dst = tmp.dst();
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o700)).unwrap();
    Arena { tmp, dst, outside }
}

impl Arena {
    fn archive(&self) -> MemArchive {
        MemArchive::new(&self.tmp.archive_file("a.zip", 20_000))
    }
}

/// Move `dir` aside to `<dir>.moved` and put a symlink to `to` in its place.
fn swap_for_symlink(dir: &Path, to: &Path) {
    let mut aside = dir.as_os_str().to_owned();
    aside.push(".moved");
    std::fs::rename(dir, PathBuf::from(aside)).unwrap();
    symlink(to, dir).unwrap();
}

/// Run the swap exactly once, before the first read of any entry's data.
fn swap_on_first_read(r: &mut MemReader, dir: PathBuf, to: PathBuf) -> Arc<AtomicBool> {
    let done = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&done);
    r.on_read = Some(Arc::new(move |_| {
        if !flag.swap(true, Ordering::SeqCst) {
            swap_for_symlink(&dir, &to);
        }
    }));
    done
}

fn run_swapped(a: &Arena, ar: &MemArchive, swap: &str, c: ConflictAnswer) -> Run {
    let mut r = ar.reader();
    let done = swap_on_first_read(&mut r, a.dst.join(swap), a.outside.clone());
    let run = run_policy(&mut r, &[], &a.dst, &opts(), c);
    assert!(done.load(Ordering::SeqCst), "the swap never happened");
    run
}

#[test]
fn files_after_the_swap_stay_in_the_checked_directory() {
    let a = arena("tt-file");
    let mut ar = a.archive();
    ar.dir("d");
    ar.file("d/first", b"one");
    ar.file("d/second", b"two");
    ar.file("d/third", b"three");
    let run = run_swapped(&a, &ar, "d", ConflictAnswer::Skip);
    run.ok();
    assert!(
        snapshot(&a.outside).is_empty(),
        "redirected outside: {:?}",
        snapshot(&a.outside)
    );
    let moved = a.dst.join("d.moved");
    assert_eq!(read(&moved.join("first")), b"one");
    assert_eq!(read(&moved.join("second")), b"two");
    assert_eq!(read(&moved.join("third")), b"three");
    assert_no_part_files(a.tmp.path());
}

#[test]
fn an_overwrite_cannot_be_redirected_onto_a_file_outside() {
    let a = arena("tt-over");
    std::fs::create_dir(a.dst.join("d")).unwrap();
    std::fs::write(a.dst.join("d/second"), b"old").unwrap();
    std::fs::write(a.outside.join("second"), b"precious").unwrap();
    let mut ar = a.archive();
    ar.dir("d");
    ar.file("d/first", b"one");
    ar.file("d/second", b"new");
    let run = run_swapped(&a, &ar, "d", ConflictAnswer::Overwrite);
    run.ok();
    assert_eq!(
        read(&a.outside.join("second")),
        b"precious",
        "a file outside the destination was overwritten"
    );
    assert_eq!(snapshot(&a.outside).len(), 1);
    assert_eq!(read(&a.dst.join("d.moved/second")), b"new");
    assert_no_part_files(a.tmp.path());
}

#[test]
fn a_symlink_entry_after_the_swap_is_not_created_outside() {
    let a = arena("tt-sym");
    let mut ar = a.archive();
    ar.dir("d");
    ar.file("d/first", b"one");
    ar.symlink("d/link", "first");
    let run = run_swapped(&a, &ar, "d", ConflictAnswer::Skip);
    run.ok();
    assert!(
        snapshot(&a.outside).is_empty(),
        "redirected outside: {:?}",
        snapshot(&a.outside)
    );
    assert_eq!(
        std::fs::read_link(a.dst.join("d.moved/link")).unwrap(),
        Path::new("first")
    );
    assert_no_part_files(a.tmp.path());
}

#[test]
fn directory_entries_after_the_swap_are_not_created_outside() {
    let a = arena("tt-dir");
    let mut ar = a.archive();
    ar.dir("d");
    ar.file("d/first", b"one");
    ar.dir("d/sub");
    ar.dir("d/sub/deeper");
    ar.file("d/sub/deeper/x", b"x");
    let run = run_swapped(&a, &ar, "d", ConflictAnswer::Skip);
    run.ok();
    assert!(
        snapshot(&a.outside).is_empty(),
        "redirected outside: {:?}",
        snapshot(&a.outside)
    );
    assert_eq!(read(&a.dst.join("d.moved/sub/deeper/x")), b"x");
    assert_no_part_files(a.tmp.path());
}

#[test]
fn a_hard_link_entry_after_the_swap_is_not_created_outside() {
    let a = arena("tt-hl");
    let mut ar = a.archive();
    ar.file("orig", b"shared");
    ar.dir("d");
    ar.file("d/first", b"one");
    ar.hardlink("d/hl", "orig");
    // Swap during d/first (the second stream; "orig" is read before `d` exists).
    let mut r = ar.reader();
    let (dir, to) = (a.dst.join("d"), a.outside.clone());
    let done = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&done);
    r.on_read = Some(Arc::new(move |_| {
        if dir.is_dir() && !flag.swap(true, Ordering::SeqCst) {
            swap_for_symlink(&dir, &to);
        }
    }));
    let run = run_plain(&mut r, &[], &a.dst, &opts());
    run.ok();
    assert!(done.load(Ordering::SeqCst));
    assert!(
        snapshot(&a.outside).is_empty(),
        "redirected outside: {:?}",
        snapshot(&a.outside)
    );
    assert_eq!(read(&a.dst.join("d.moved/hl")), b"shared");
    assert_no_part_files(a.tmp.path());
}

#[test]
fn swapping_a_grandparent_does_not_redirect_a_deeper_directory() {
    let a = arena("tt-grand");
    // A decoy with the same shape outside: a path-based walk would find `e` there.
    std::fs::create_dir(a.outside.join("e")).unwrap();
    let mut ar = a.archive();
    ar.dir("d").dir("d/e");
    ar.file("d/e/first", b"one");
    ar.file("d/e/second", b"two");
    let run = run_swapped(&a, &ar, "d", ConflictAnswer::Skip);
    run.ok();
    assert_eq!(snapshot(&a.outside).len(), 1, "{:?}", snapshot(&a.outside));
    assert_eq!(read(&a.dst.join("d.moved/e/second")), b"two");
    assert_no_part_files(a.tmp.path());
}

#[test]
fn final_directory_metadata_does_not_follow_the_swap() {
    let a = arena("tt-meta");
    let before_mtime = mtime_ns(&a.outside);
    let mut ar = a.archive();
    ar.dir("d").mode(0o755).mtime(MTIME);
    ar.file("d/only", b"one");
    let run = run_swapped(&a, &ar, "d", ConflictAnswer::Skip);
    run.ok();
    assert_eq!(perm(&a.outside), 0o700, "chmod followed the symlink");
    assert_eq!(mtime_ns(&a.outside), before_mtime);
    // The directory that was actually filled gets the archive's metadata.
    assert_eq!(perm(&a.dst.join("d.moved")), 0o755);
    assert_eq!(mtime_ns(&a.dst.join("d.moved")), MTIME);
    assert_no_part_files(a.tmp.path());
}

/// More directories than the handle cache holds: the swapped directory's handle is dropped and
/// has to be opened again, which must refuse the symlink.
#[test]
fn a_swapped_parent_opened_again_later_is_refused_not_followed() {
    let a = arena("tt-evict");
    let mut ar = a.archive();
    ar.dir("d");
    ar.file("d/first", b"one");
    for i in 0..80 {
        ar.dir(&format!("x{i:02}"));
        ar.file(format!("x{i:02}/f"), b"x");
    }
    ar.file("d/late", b"late");
    ar.symlink("d/late-link", "first");
    ar.dir("d/late-dir");
    ar.file("end", b"end");
    let run = run_swapped(&a, &ar, "d", ConflictAnswer::Skip);
    let rep = run.ok();
    assert!(
        snapshot(&a.outside).is_empty(),
        "redirected outside: {:?}",
        snapshot(&a.outside)
    );
    assert_eq!(perm(&a.outside), 0o700);
    assert_eq!(read(&a.dst.join("d.moved/first")), b"one");
    assert_absent(&a.dst.join("d.moved/late"));
    assert!(rep.skipped >= 3, "{rep:?}");
    assert_eq!(read(&a.dst.join("end")), b"end");
    assert_no_part_files(a.tmp.path());
}

#[test]
fn a_destination_that_goes_through_a_symlink_keeps_working() {
    let a = arena("tt-legit");
    let real = a.tmp.path().join("real/inner");
    std::fs::create_dir_all(&real).unwrap();
    symlink(a.tmp.path().join("real"), a.tmp.path().join("via")).unwrap();
    let dest = a.tmp.path().join("via/inner");
    let mut ar = a.archive();
    ar.dir("d");
    ar.file("d/f", b"data");
    ar.file("top", b"top");
    ar.symlink("d/l", "f");
    ar.hardlink("d/h", "d/f");
    let mut r = ar.reader();
    let run = run_plain(&mut r, &[], &dest, &opts());
    run.ok();
    assert_eq!(read(&real.join("d/f")), b"data");
    assert_eq!(read(&real.join("top")), b"top");
    assert_eq!(read(&real.join("d/h")), b"data");
    assert_eq!(
        std::fs::read_link(real.join("d/l")).unwrap(),
        Path::new("f")
    );
    assert_no_part_files(&real);

    // A destination that is itself a symlink to a directory, not yet containing the subfolder.
    let dest = a.tmp.path().join("via/new/sub");
    let mut r = ar.reader();
    run_plain(&mut r, &[], &dest, &opts()).ok();
    assert_eq!(read(&a.tmp.path().join("real/new/sub/d/f")), b"data");
}

/// The destination path is resolved once: redirecting the user's own symlink mid-job does not
/// move the rest of the output elsewhere.
#[test]
fn redirecting_the_destination_symlink_mid_job_has_no_effect() {
    let a = arena("tt-root");
    let real = a.tmp.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let via = a.tmp.path().join("via");
    symlink(&real, &via).unwrap();
    let mut ar = a.archive();
    ar.file("first", b"one");
    ar.file("second", b"two");
    ar.dir("d");
    let mut r = ar.reader();
    let (link, to) = (via.clone(), a.outside.clone());
    let done = AtomicBool::new(false);
    r.on_read = Some(Arc::new(move |_| {
        if !done.swap(true, Ordering::SeqCst) {
            std::fs::remove_file(&link).unwrap();
            symlink(&to, &link).unwrap();
        }
    }));
    run_plain(&mut r, &[], &via, &opts()).ok();
    assert!(
        snapshot(&a.outside).is_empty(),
        "{:?}",
        snapshot(&a.outside)
    );
    assert_eq!(read(&real.join("first")), b"one");
    assert_eq!(read(&real.join("second")), b"two");
    assert!(real.join("d").is_dir());
}

// ------------------------------------------------------------------------ identity checks

/// More directories than the handle cache holds, then a REAL directory is put where the job's
/// `d` was. The final mode and time of the archive must not land on it.
#[test]
fn final_metadata_is_not_applied_to_a_substituted_real_directory() {
    let a = arena("tt-subst");
    let victim = a.outside.join("victim");
    std::fs::create_dir(&victim).unwrap();
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(victim.join("secret"), b"secret").unwrap();
    let victim_mtime = mtime_ns(&victim);
    let mut ar = a.archive();
    ar.dir("d").mode(0o777).mtime(MTIME);
    ar.file("d/first", b"one");
    for i in 0..80 {
        ar.dir(&format!("x{i:02}"));
        ar.file(format!("x{i:02}/f"), b"x");
    }
    let mut r = ar.reader();
    let (d, moved, from) = (a.dst.join("d"), a.dst.join("d.moved"), victim.clone());
    let marker = a.dst.join("x79");
    let done = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&done);
    r.on_read = Some(Arc::new(move |_| {
        if marker.is_dir() && !flag.swap(true, Ordering::SeqCst) {
            std::fs::rename(&d, &moved).unwrap();
            std::fs::rename(&from, &d).unwrap();
        }
    }));
    let run = run_plain(&mut r, &[], &a.dst, &opts());
    let rep = run.ok();
    assert!(done.load(Ordering::SeqCst), "the swap never happened");
    let substituted = a.dst.join("d");
    assert_eq!(read(&substituted.join("secret")), b"secret");
    assert_eq!(perm(&substituted), 0o700, "chmod hit a foreign directory");
    assert_eq!(mtime_ns(&substituted), victim_mtime);
    assert!(
        rep.warnings
            .iter()
            .any(|w| w.path == substituted && w.message.contains("replaced")),
        "{:?}",
        rep.warnings
    );
    // The directories that are still the job's own got their metadata.
    assert_eq!(perm(&a.dst.join("x00")), 0o755);
    assert_eq!(mtime_ns(&a.dst.join("x00")), MTIME);
}

/// The hard link's source is replaced while the user decides about the destination: the wrong file
/// must not be published over the file the user agreed to overwrite.
#[test]
fn a_hard_link_source_replaced_during_the_question_is_not_published() {
    for symlink_source in [false, true] {
        let a = arena("tt-hlsrc");
        std::fs::write(a.dst.join("hard"), b"mine").unwrap();
        let evil = a.outside.join("evil");
        if symlink_source {
            symlink(a.outside.join("target"), &evil).unwrap();
        } else {
            std::fs::write(&evil, b"evil!!").unwrap();
        }
        let mut ar = a.archive();
        ar.file("orig", b"shared");
        ar.hardlink("hard", "orig");
        ar.file("after", b"a");
        let mut r = ar.reader();
        let orig = a.dst.join("orig");
        let (sink, rec) = ScriptedSink::new(move |_| {
            std::fs::rename(&evil, &orig).unwrap();
            conflict(ConflictAnswer::Overwrite, false)
        });
        let ctx = JobCtx::simple(Arc::new(RealFs), sink);
        let run = run_with_ctx(ctx, rec, &mut r, &[], &a.dst, &opts());
        let rep = run.ok();
        assert_eq!(run.questions(), 1);
        assert_eq!(read(&a.dst.join("hard")), b"mine", "{symlink_source}");
        assert!(!a.dst.join("hard").is_symlink());
        assert_eq!(rep.skipped, 1);
        assert!(
            rep.warnings
                .iter()
                .any(|w| w.message.contains("target changed")),
            "{:?}",
            rep.warnings
        );
        assert_eq!(read(&a.dst.join("after")), b"a");
        assert_no_part_files(a.tmp.path());
    }
}

// ------------------------------------------------------------------- cancel under a swap

/// Cancel in the middle of a file, right after its parent was swapped: the temp file is removed
/// from the directory it was created in, and nothing appears outside.
#[test]
fn a_cancel_under_a_swap_leaves_no_temp_file_anywhere() {
    let a = arena("tt-cancel");
    let mut ar = a.archive();
    ar.dir("d");
    ar.file("d/first", b"one");
    ar.file("d/second", &bytes(2, 50_000));
    ar.file("d/third", b"three");
    let mut r = ar.reader();
    let cancel = CancelToken::new();
    let token = cancel.clone();
    let (d, to) = (a.dst.join("d"), a.outside.clone());
    let temps = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&temps);
    let done = AtomicBool::new(false);
    r.on_read = Some(Arc::new(move |delivered| {
        if delivered >= 10_000 && !done.swap(true, Ordering::SeqCst) {
            seen.store(part_files_in(&d).len(), Ordering::SeqCst);
            swap_for_symlink(&d, &to);
            token.cancel();
        }
    }));
    let run = run_cancellable(&cancel, &mut r, &[], &a.dst, &opts());
    assert!(is_cancelled(&run), "{:?}", run.result);
    assert_eq!(temps.load(Ordering::SeqCst), 1, "no temp file at the swap");
    assert!(
        snapshot(&a.outside).is_empty(),
        "{:?}",
        snapshot(&a.outside)
    );
    assert_eq!(perm(&a.outside), 0o700);
    let moved = a.dst.join("d.moved");
    assert_eq!(read(&moved.join("first")), b"one");
    assert_absent(&moved.join("second"));
    assert_absent(&moved.join("third"));
    assert_no_part_files(a.tmp.path());
}

/// "Abort" answered to a conflict that appears after the swap.
#[test]
fn an_abort_under_a_swap_changes_nothing_further() {
    let a = arena("tt-abort");
    std::fs::create_dir(a.dst.join("d")).unwrap();
    std::fs::write(a.dst.join("d/second"), b"old").unwrap();
    std::fs::write(a.outside.join("second"), b"precious").unwrap();
    let mut ar = a.archive();
    ar.dir("d");
    ar.file("d/first", b"one");
    ar.file("d/second", b"new");
    let run = run_swapped(&a, &ar, "d", ConflictAnswer::Abort);
    assert!(is_cancelled(&run), "{:?}", run.result);
    assert_eq!(read(&a.outside.join("second")), b"precious");
    assert_eq!(snapshot(&a.outside).len(), 1);
    assert_eq!(read(&a.dst.join("d.moved/second")), b"old");
    assert_no_part_files(a.tmp.path());
}
