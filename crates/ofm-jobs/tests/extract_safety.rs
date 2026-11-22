//! T6.2 extract job: hostile archives. Zip-slip, lying readers (the job does not trust index
//! flags), symlink tricks, decompression bombs, corrupt entries, passwords, panics.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod extract_support;

use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use extract_support::*;
use ofm_jobs::{ConflictAnswer, ExtractOpts};
use ofm_vfs::archive::{ArchiveError, ArchiveLimits, EntryId};

/// `<tmp>/work/dst` is the destination; everything else below `<tmp>` must stay untouched.
struct Arena {
    tmp: Tmp,
    dst: PathBuf,
}

fn arena(tag: &str) -> Arena {
    let tmp = Tmp::new(tag);
    let dst = tmp.path().join("work/dst");
    std::fs::create_dir_all(&dst).unwrap();
    // A neighbour of the destination that traversal would hit first.
    std::fs::write(tmp.path().join("work/neighbour.txt"), b"neighbour").unwrap();
    Arena { tmp, dst }
}

impl Arena {
    /// Snapshot of everything outside the destination.
    fn outside(&self) -> Vec<(PathBuf, String)> {
        snapshot(self.tmp.path())
            .into_iter()
            .filter(|(p, _)| !p.starts_with("work/dst") && p != Path::new("work/dst"))
            .collect()
    }
}

#[test]
fn zip_slip_entries_flagged_by_the_index_leave_nothing_outside() {
    let a = arena("zs1");
    let abs_target = a.tmp.path().join("abs-evil");
    let mut ar = MemArchive::new(&a.tmp.archive_file("evil.zip", 20_000));
    ar.file("../../evil", b"x");
    ar.file("../evil2", b"x");
    ar.file(abs_target.to_str().unwrap(), b"x");
    ar.file("a/../../b", b"x");
    ar.file("ok/../fine", b"x");
    ar.file("..", b"x");
    ar.symlink("l", "/etc");
    ar.file("l/passwd", b"x");
    ar.file("good.txt", b"good");
    let before = a.outside();
    let mut r = ar.reader();
    let run = run_plain(&mut r, &[], &a.dst, &opts());
    run.ok();
    assert_eq!(
        a.outside(),
        before,
        "something was created outside the destination"
    );
    assert_absent(&abs_target);
    assert_eq!(read(&a.dst.join("good.txt")), b"good");
    let names: Vec<_> = snapshot(&a.dst).into_iter().map(|(p, _)| p).collect();
    // `l` itself was refused (absolute target), so `l/passwd` is just a file in a real directory.
    assert_eq!(
        names,
        [
            PathBuf::from("good.txt"),
            PathBuf::from("l"),
            PathBuf::from("l/passwd")
        ]
    );
    let w = run.warnings().join("\n");
    for needle in ["../../evil", "../evil2", "a/../../b", "ok/../fine"] {
        assert!(w.contains(needle), "no warning for {needle}: {w}");
    }
    assert_eq!(run.report.skipped, 7, "every unsafe entry counted");
    assert_no_part_files(&a.dst);
}

#[test]
fn the_job_does_not_trust_index_flags_when_a_reader_lies() {
    let a = arena("zs2");
    let abs_target = a.tmp.path().join("abs-evil");
    let mut ar = MemArchive::new(&a.tmp.archive_file("evil.zip", 20_000));
    ar.file("dummy.txt", b"payload");
    let mut r = ar.reader();
    let abs = abs_target.to_str().unwrap().to_owned();
    let nul = std::ffi::OsString::from_vec(b"nul\0byte".to_vec());
    r.tamper = Some(Box::new(move |e| {
        let mut evil: Vec<_> = [
            "../evil",
            "../../evil",
            "../work/dst/../neighbour.txt",
            "a/../../b",
            "a/..",
            "/etc/ofm-evil",
            "a//b",
            "a/./b",
            "./x",
            "x/",
            "",
            ".",
            "..",
        ]
        .into_iter()
        .map(|p| forged(e, p))
        .collect();
        evil.push(forged(e, &abs));
        evil.push(forged(e, &nul));
        evil.push(forged(e, "x".repeat(300)));
        let n = evil.len();
        evil.push(e.clone()); // and one honest entry
        assert!(n > 10);
        evil
    }));
    let before = a.outside();
    let run = run_plain(&mut r, &[], &a.dst, &opts());
    run.ok();
    assert_eq!(a.outside(), before, "a forged path escaped the destination");
    assert_absent(&abs_target);
    assert_eq!(read(&a.dst.join("dummy.txt")), b"payload");
    assert_eq!(snapshot(&a.dst).len(), 1);
    assert_eq!(run.report.warnings.len(), 16, "{:?}", run.warnings());
    assert_no_part_files(&a.dst);
}

#[test]
fn forged_directory_and_link_entries_cannot_escape_either() {
    let a = arena("zs3");
    let mut ar = MemArchive::new(&a.tmp.archive_file("evil.zip", 20_000));
    ar.file("dummy.txt", b"payload");
    let mut r = ar.reader();
    r.tamper = Some(Box::new(|e| {
        let mut out = Vec::new();
        for (path, kind, target) in [
            ("../dir", ofm_vfs::archive::ArchiveEntryKind::Dir, None),
            (
                "lnk1",
                ofm_vfs::archive::ArchiveEntryKind::Symlink,
                Some("/etc"),
            ),
            (
                "lnk2",
                ofm_vfs::archive::ArchiveEntryKind::Symlink,
                Some("../../etc"),
            ),
            (
                "lnk3",
                ofm_vfs::archive::ArchiveEntryKind::Symlink,
                Some("a/../../x"),
            ),
            (
                "lnk4",
                ofm_vfs::archive::ArchiveEntryKind::Symlink,
                Some(""),
            ),
            (
                "../lnk5",
                ofm_vfs::archive::ArchiveEntryKind::Symlink,
                Some("x"),
            ),
            (
                "hl",
                ofm_vfs::archive::ArchiveEntryKind::Hardlink,
                Some("/etc/passwd"),
            ),
            ("dev", ofm_vfs::archive::ArchiveEntryKind::Other, None),
        ] {
            let mut f = forged(e, path);
            f.kind = kind;
            f.link_target = target.map(PathBuf::from);
            out.push(f);
        }
        out
    }));
    let before = a.outside();
    let run = run_plain(&mut r, &[], &a.dst, &opts());
    run.ok();
    assert_eq!(a.outside(), before);
    assert!(snapshot(&a.dst).is_empty(), "{:?}", snapshot(&a.dst));
    assert_eq!(run.report.warnings.len(), 8, "{:?}", run.warnings());
}

#[test]
fn an_entry_the_reader_was_not_asked_for_is_skipped() {
    let a = arena("zs4");
    let mut ar = MemArchive::new(&a.tmp.archive_file("a.zip", 20_000));
    ar.file("wanted.txt", b"w");
    ar.file("other.txt", b"o");
    let mut r = ar.reader();
    r.tamper = Some(Box::new(|e| {
        let mut f = e.clone();
        f.id = EntryId(777);
        vec![f]
    }));
    let run = run_plain(&mut r, &["wanted.txt"], &a.dst, &opts());
    run.ok();
    assert!(snapshot(&a.dst).is_empty());
    assert_eq!(run.report.warnings.len(), 1);
}

#[test]
fn a_selection_that_climbs_is_an_invalid_request() {
    let a = arena("zs5");
    let mut ar = MemArchive::new(&a.tmp.archive_file("a.zip", 20_000));
    ar.file("x", b"x");
    let mut r = ar.reader();
    for bad in ["../x", "/abs", "a/../../b"] {
        let run = run_plain(&mut r, &[bad], &a.dst, &opts());
        assert!(
            matches!(run.result, Err(ofm_jobs::JobError::Invalid(_))),
            "{bad}: {:?}",
            run.result
        );
    }
    assert!(snapshot(&a.dst).is_empty());
}

#[test]
fn a_link_to_an_ancestor_cannot_be_used_to_climb_out_through_another_link() {
    // `d/e/s -> ../..` is inside the tree (it points at the destination itself). A second link
    // `l -> d/e/s/../../..` would leave it if followed physically; the job refuses the `..` that
    // follows a name, so the second link is never created.
    let a = arena("zs6");
    let mut ar = MemArchive::new(&a.tmp.archive_file("a.zip", 20_000));
    ar.dir("d").dir("d/e");
    ar.symlink("d/e/s", "../..");
    ar.symlink("l", "d/e/s/../../..");
    ar.symlink("m", "d/e/s");
    let before = a.outside();
    let mut r = ar.reader();
    let run = run_plain(&mut r, &[], &a.dst, &opts());
    run.ok();
    assert_absent(&a.dst.join("l"));
    assert!(
        std::fs::symlink_metadata(a.dst.join("m")).is_ok(),
        "m -> d/e/s is fine"
    );
    assert_eq!(a.outside(), before);
}

// ------------------------------------------------------------------------------------ bombs

fn small_limits() -> ArchiveLimits {
    ArchiveLimits {
        max_ratio: 10,
        ratio_min_size: 1024,
        warn_total_bytes: 1 << 20,
        ..ArchiveLimits::default()
    }
}

#[test]
fn a_ratio_bomb_stops_the_job_and_leaves_no_partial_file() {
    let a = arena("b1");
    let mut ar = MemArchive::new(&a.tmp.archive_file("bomb.zip", 20_000));
    ar.file("first.txt", b"fine");
    ar.file_with("bomb.bin", 800_000, Data::Zeros(800_000))
        .packed(100);
    ar.file("never.txt", b"not reached");
    let mut r = ar.reader();
    let o = ExtractOpts {
        limits: small_limits(),
        ..opts()
    };
    let run = run_plain(&mut r, &[], &a.dst, &o);
    let msg = fail_text(&run);
    assert!(
        msg.contains("expands more than 10:1") && msg.contains("bomb.bin"),
        "{msg}"
    );
    assert_eq!(read(&a.dst.join("first.txt")), b"fine");
    assert_absent(&a.dst.join("bomb.bin"));
    assert_absent(&a.dst.join("never.txt"));
    assert_no_part_files(&a.dst);
}

#[test]
fn a_solid_archive_bomb_is_caught_by_the_overall_ratio() {
    let a = arena("b2");
    // No per-entry packed size (tar.gz, solid 7z): the whole-archive ratio decides.
    let mut ar = MemArchive::new(&a.tmp.archive_file("bomb.tar.gz", 1024));
    ar.file_with("zeros.bin", 3 << 20, Data::Zeros(3 << 20));
    ar.file("after", b"x");
    let mut r = ar.reader();
    let run = run_plain(&mut r, &[], &a.dst, &opts());
    assert!(fail_text(&run).contains("archive expands more than 1000:1"));
    assert_absent(&a.dst.join("zeros.bin"));
    assert_absent(&a.dst.join("after"));
    assert_no_part_files(&a.dst);
}

#[test]
fn a_declared_total_over_the_limit_is_refused_before_anything_is_written() {
    let a = arena("b3");
    let mut ar = MemArchive::new(&a.tmp.archive_file("big.zip", 20_000));
    for i in 0..5 {
        ar.file_with(&format!("f{i}"), 300_000, Data::Zeros(300_000));
    }
    let mut r = ar.reader();
    let o = ExtractOpts {
        limits: ArchiveLimits {
            warn_total_bytes: 1_000_000,
            ..ArchiveLimits::default()
        },
        ..opts()
    };
    let run = run_plain(&mut r, &[], &a.dst, &o);
    assert!(fail_text(&run).contains("exceeds the limit"));
    assert!(snapshot(&a.dst).is_empty());
}

#[test]
fn bytes_actually_written_are_counted_against_the_total_limit() {
    // The visited entry claims 10 MiB (so it passes the declared-size check) and delivers them:
    // the streaming total limit still stops it at 1 MiB.
    let a = arena("b4");
    let mut ar = MemArchive::new(&a.tmp.archive_file("big.zip", 50_000_000));
    ar.file_with("a.bin", 1000, Data::Zeros(10 << 20));
    let mut r = ar.reader();
    r.chunk = 1 << 16;
    r.tamper = Some(Box::new(|e| {
        let mut f = e.clone();
        f.size = 10 << 20;
        vec![f]
    }));
    let o = ExtractOpts {
        limits: ArchiveLimits {
            warn_total_bytes: 1 << 20,
            ..ArchiveLimits::default()
        },
        ..opts()
    };
    let run = run_plain(&mut r, &[], &a.dst, &o);
    assert!(fail_text(&run).contains("more than 1048576 bytes extracted"));
    assert_absent(&a.dst.join("a.bin"));
    assert_no_part_files(&a.dst);
}

#[test]
fn a_lying_header_is_stopped_while_streaming() {
    let a = arena("b5");
    let mut ar = MemArchive::new(&a.tmp.archive_file("lie.zip", 20_000));
    ar.file_with("liar.bin", 1000, Data::Bytes(bytes(1, 5000)));
    ar.file("next", b"n");
    let mut r = ar.reader();
    let run = run_plain(&mut r, &[], &a.dst, &opts());
    assert!(fail_text(&run).contains("more data than its declared size of 1000"));
    assert_absent(&a.dst.join("liar.bin"));
    assert_no_part_files(&a.dst);
}

#[test]
fn a_backend_limit_error_from_the_stream_aborts_the_job() {
    let a = arena("b6");
    let mut ar = MemArchive::new(&a.tmp.archive_file("lie.zip", 20_000));
    ar.file_with(
        "x.bin",
        10_000,
        Data::Fatal(vec![1; 3000], FatalKind::Limit),
    );
    ar.file("next", b"n");
    let mut r = ar.reader();
    let run = run_plain(&mut r, &[], &a.dst, &opts());
    assert!(
        fail_text(&run).contains("limit exceeded"),
        "{:?}",
        run.result
    );
    assert_absent(&a.dst.join("x.bin"));
    assert_absent(&a.dst.join("next"));
    assert_no_part_files(&a.dst);
}

// ----------------------------------------------------------------------- corrupt and passwords

#[test]
fn a_corrupt_entry_is_a_warning_and_the_job_continues() {
    let a = arena("e1");
    let mut ar = MemArchive::new(&a.tmp.archive_file("c.zip", 20_000));
    ar.file("before", b"b");
    ar.file_with("bad.bin", 9000, Data::Corrupt(vec![7; 5000]));
    ar.file_with("short.bin", 9000, Data::Bytes(vec![7; 5000]));
    ar.file("after", b"a");
    let mut r = ar.reader();
    let run = run_plain(&mut r, &[], &a.dst, &opts());
    let rep = run.ok();
    assert_eq!(read(&a.dst.join("before")), b"b");
    assert_eq!(read(&a.dst.join("after")), b"a");
    assert_absent(&a.dst.join("bad.bin"));
    assert_absent(&a.dst.join("short.bin"));
    assert_no_part_files(&a.dst);
    let w = run.warnings().join("\n");
    assert!(
        w.contains("could not read the entry's data") && w.contains("truncated"),
        "{w}"
    );
    assert_eq!(rep.skipped, 2);
    assert_eq!(rep.files_done, 2);
}

#[test]
fn a_missing_or_wrong_password_is_a_clear_failure() {
    let a = arena("e2");
    let mut ar = MemArchive::new(&a.tmp.archive_file("enc.zip", 20_000));
    ar.file("secret.txt", b"s");
    for (kind, text) in [
        (FatalKind::NeedsPassword, "password required"),
        (FatalKind::WrongPassword, "wrong password"),
    ] {
        // From the visit itself (headers or the first entry) ...
        let mut r = ar.reader();
        r.visit_error = Some(Box::new(move || match kind {
            FatalKind::NeedsPassword => ArchiveError::NeedsPassword("/enc.zip".into()),
            _ => ArchiveError::WrongPassword("/enc.zip".into()),
        }));
        let run = run_plain(&mut r, &[], &a.dst, &opts());
        let msg = fail_text(&run);
        assert!(msg.contains(text), "{msg}");
        assert!(matches!(run.result, Err(ofm_jobs::JobError::Archive(_))));
    }
    // ... and from the middle of an entry's stream.
    let mut ar = MemArchive::new(&a.tmp.archive_file("enc2.zip", 20_000));
    ar.file_with(
        "s.txt",
        5000,
        Data::Fatal(vec![1; 100], FatalKind::NeedsPassword),
    );
    let mut r = ar.reader();
    let run = run_plain(&mut r, &[], &a.dst, &opts());
    assert!(fail_text(&run).contains("password required"));
    assert_absent(&a.dst.join("s.txt"));
    assert_no_part_files(&a.dst);
    assert!(snapshot(&a.dst).is_empty());
}

#[test]
fn a_decoder_panic_leaves_no_temp_file_and_is_catchable() {
    let a = arena("e3");
    let mut ar = MemArchive::new(&a.tmp.archive_file("p.zip", 20_000));
    ar.file("before", b"b");
    ar.file_with("boom.bin", 9000, Data::PanicAfter(vec![5; 5000]));
    let mut r = ar.reader();
    let dst = a.dst.clone();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_plain(&mut r, &[], &dst, &opts())
    }));
    assert!(
        caught.is_err(),
        "the panic propagates to the runner, which turns it into Failed"
    );
    assert_eq!(read(&a.dst.join("before")), b"b");
    assert_absent(&a.dst.join("boom.bin"));
    assert_no_part_files(&a.dst);
}

#[test]
fn the_runner_turns_a_panic_into_a_failed_outcome() {
    // Through `run_job`: the real entry point catches the panic (invariant 7).
    use ofm_jobs::{JobSpec, Outcome, ScriptedSink, run_sync};
    let tmp = Tmp::new("e4");
    // A "zip" with a valid signature but garbage after it: the backend must report an error
    // (never panic, never crash). Whatever it says, the outcome is Failed or Ok, never an abort.
    let arch = tmp.path().join("junk.zip");
    std::fs::write(&arch, b"PK\x03\x04 this is not a zip file at all").unwrap();
    let (sink, _) = ScriptedSink::refusing();
    let out = run_sync(
        &JobSpec::Extract {
            archive: arch,
            entries: vec![],
            dest_dir: tmp.dst(),
            opts: opts(),
        },
        Arc::new(ofm_jobs::RealFs),
        sink,
    );
    assert!(matches!(out, Outcome::Failed { .. }), "{out:?}");
    assert_no_part_files(tmp.path());
}

#[test]
fn a_destination_that_rejects_everything_stops_after_a_streak_of_failures() {
    if running_as_root() {
        return; // root ignores directory permissions
    }
    let a = arena("e5");
    let mut ar = MemArchive::new(&a.tmp.archive_file("many.zip", 20_000));
    for i in 0..200 {
        ar.file(format!("f{i:03}"), b"x");
    }
    std::fs::set_permissions(&a.dst, std::os::unix::fs::PermissionsExt::from_mode(0o500)).unwrap();
    let mut r = ar.reader();
    let run = run_plain(&mut r, &[], &a.dst, &opts());
    std::fs::set_permissions(&a.dst, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    assert!(
        fail_text(&run).contains("too many entries failed"),
        "{:?}",
        run.result
    );
    assert!(
        run.report.warnings.len() <= 60,
        "{} warnings",
        run.report.warnings.len()
    );
    assert!(snapshot(&a.dst).is_empty());
}

fn running_as_root() -> bool {
    std::fs::metadata("/proc/self").is_ok_and(|m| std::os::unix::fs::MetadataExt::uid(&m) == 0)
}

#[test]
fn policy_used_by_the_runner_is_the_one_in_the_options() {
    // Through `run_job`/`dispatch`: `opts.on_conflict` is applied without a sink answer.
    use ofm_jobs::{JobSpec, Outcome, ScriptedSink, run_sync};
    let tmp = Tmp::new("e6");
    let dst = tmp.dst();
    std::fs::write(dst.join("x"), b"old").unwrap();
    // An unreadable archive still goes through the dispatch: the job fails, nothing changes.
    let (sink, rec) = ScriptedSink::refusing();
    let out = run_sync(
        &JobSpec::Extract {
            archive: tmp.path().join("missing.zip"),
            entries: vec![],
            dest_dir: dst.clone(),
            opts: ExtractOpts {
                on_conflict: Some(ConflictAnswer::Skip),
                ..opts()
            },
        },
        Arc::new(ofm_jobs::RealFs),
        sink,
    );
    assert!(matches!(out, Outcome::Failed { .. }));
    assert!(rec.lock().unwrap().questions.is_empty());
    assert_eq!(read(&dst.join("x")), b"old");
}
