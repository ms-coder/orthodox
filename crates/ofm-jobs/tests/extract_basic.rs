//! T6.2 extract job: the happy paths. Plain trees, selections, metadata, names, links, performance.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod extract_support;

use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use extract_support::*;
use ofm_testkit::compare::{Compare, assert_trees_equal};
use ofm_testkit::tree::{FileSpec, TreeSpec, raw_name};

#[test]
fn plain_tree_matches_the_expected_fixture_exactly() {
    let tmp = Tmp::new("plain");
    let dst = tmp.dst();
    let big = bytes(7, 3 * 1024 * 1024 + 17);
    let mut a = MemArchive::new(&tmp.archive_file("a.zip", 20_000));
    a.dir("docs").file("docs/readme.txt", b"hello\n");
    a.file("docs/empty", b"");
    a.dir("docs/deep").dir("docs/deep/er");
    a.file("docs/deep/er/data.bin", &big).mode(0o600);
    a.file("top.sh", b"#!/bin/sh\n").mode(0o755);
    let mut r = a.reader();
    let run = run_plain(&mut r, &[], &dst, &opts());
    let report = run.ok();

    let exp = tmp.path().join("exp");
    TreeSpec::new()
        .dir(
            "docs",
            TreeSpec::new()
                .file("readme.txt", FileSpec::content(b"hello\n"))
                .file("empty", FileSpec::content(b""))
                .dir(
                    "deep",
                    TreeSpec::new().dir(
                        "er",
                        TreeSpec::new().file("data.bin", FileSpec::content(&big).mode(0o600)),
                    ),
                ),
        )
        .file("top.sh", FileSpec::content(b"#!/bin/sh\n").mode(0o755))
        .build(&exp)
        .unwrap();
    assert_trees_equal(&exp, &dst, Compare::all());
    assert_no_part_files(&dst);
    assert_eq!(run.questions(), 0);
    assert_eq!(report.files_done, 4);
    assert_eq!(report.bytes_done, 6 + 10 + big.len() as u64);
    let mut created = report.created.clone();
    created.sort();
    assert_eq!(created, vec![dst.join("docs"), dst.join("top.sh")]);
}

#[test]
fn selecting_a_subdirectory_extracts_only_that_subtree() {
    let tmp = Tmp::new("sel");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 10));
    a.dir("a").dir("a/b").dir("a/c");
    a.file("a/b/one.txt", b"1");
    a.file("a/b/two.txt", b"2");
    a.file("a/c/three.txt", b"3");
    a.file("a/four.txt", b"4");
    a.file("five.txt", b"5");
    let mut r = a.reader();
    let run = run_plain(&mut r, &["a/b"], &dst, &opts());
    let report = run.ok();
    let snap: Vec<String> = snapshot(&dst)
        .into_iter()
        .map(|(p, _)| p.display().to_string())
        .collect();
    assert_eq!(snap, ["b", "b/one.txt", "b/two.txt"]);
    assert_eq!(report.created, vec![dst.join("b")]);
    assert_eq!(read(&dst.join("b/two.txt")), b"2");

    // Several selections, including a single file, land side by side.
    let dst2 = tmp.path().join("dst2");
    let mut r = a.reader();
    let run = run_plain(&mut r, &["a/c", "five.txt", "a/four.txt"], &dst2, &opts());
    let mut created = run.ok().created.clone();
    created.sort();
    assert_eq!(
        created,
        vec![dst2.join("c"), dst2.join("five.txt"), dst2.join("four.txt")],
        "the destination directory is created on demand"
    );
    assert_eq!(read(&dst2.join("c/three.txt")), b"3");
}

#[test]
fn missing_parent_directories_are_synthesized() {
    let tmp = Tmp::new("synth");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar.gz", 10));
    a.file("x/y/z.txt", b"deep");
    let mut r = a.reader();
    run_plain(&mut r, &[], &dst, &opts()).ok();
    assert_eq!(read(&dst.join("x/y/z.txt")), b"deep");
    assert_eq!(
        perm(&dst.join("x")),
        0o755,
        "synthetic directories get 0755"
    );
    assert_no_part_files(&dst);
}

#[test]
fn mode_and_mtime_are_restored_and_special_bits_dropped() {
    let tmp = Tmp::new("meta");
    let dst = tmp.dst();
    let t1 = 1_234_567_890_123_456_789i128;
    let t2 = 1_500_000_000_000_000_001i128;
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 10));
    a.dir("d").mode(0o750).mtime(t2);
    a.file("d/f", b"x").mode(0o640).mtime(t1);
    a.file("suid", b"x").mode(0o4755);
    a.file("sgid", b"x").mode(0o2775);
    a.file("sticky", b"x").mode(0o1777);
    a.file("all", b"x").mode(0o7777);
    let mut r = a.reader();
    run_plain(&mut r, &[], &dst, &opts()).ok();
    assert_eq!(perm(&dst.join("d/f")), 0o640);
    assert_eq!(mtime_ns(&dst.join("d/f")), t1, "nanosecond mtime restored");
    assert_eq!(perm(&dst.join("d")), 0o750);
    assert_eq!(
        mtime_ns(&dst.join("d")),
        t2,
        "directory mtime is set after its contents were written"
    );
    assert_eq!(perm(&dst.join("suid")), 0o755);
    assert_eq!(perm(&dst.join("sgid")), 0o775);
    assert_eq!(perm(&dst.join("sticky")), 0o777);
    assert_eq!(perm(&dst.join("all")), 0o777);
}

#[test]
fn ownership_is_never_restored() {
    let tmp = Tmp::new("owner");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 10));
    a.file("f", b"x").with(|r| {
        r.uid = Some(0);
        r.gid = Some(0);
    });
    let mut r = a.reader();
    run_plain(&mut r, &[], &dst, &opts()).ok();
    let md = std::fs::metadata(dst.join("f")).unwrap();
    let me = std::fs::metadata(&dst).unwrap();
    assert_eq!((md.uid(), md.gid()), (me.uid(), me.gid()));
}

#[test]
fn read_only_directory_entries_do_not_lock_the_job_out() {
    let tmp = Tmp::new("ro");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 10));
    // The directory entry comes FIRST and is read-only; its children follow.
    a.dir("ro").mode(0o555);
    a.dir("ro/sub").mode(0o500);
    a.file("ro/sub/f", b"inside");
    a.file("ro/g", b"also");
    a.dir("noaccess").mode(0o000);
    a.file("noaccess/h", b"hidden");
    let mut r = a.reader();
    let run = run_plain(&mut r, &[], &dst, &opts());
    run.ok();
    assert!(run.report.warnings.is_empty(), "{:?}", run.warnings());
    assert_eq!(
        perm(&dst.join("ro")),
        0o555,
        "final mode applied after the contents"
    );
    assert_eq!(perm(&dst.join("ro/sub")), 0o500);
    assert_eq!(perm(&dst.join("noaccess")), 0o000);
    assert_eq!(read(&dst.join("ro/sub/f")), b"inside");
    assert_eq!(read(&dst.join("ro/g")), b"also");
    assert_no_part_files(&dst.join("ro"));
}

#[test]
fn awkward_names_work_byte_for_byte() {
    let tmp = Tmp::new("names");
    let dst = tmp.dst();
    let non_utf8 = raw_name(&[b'n', 0xff, 0xfe, b'.', b't', b'x', b't']);
    let non_utf8_dir = raw_name(&[b'd', 0x80, b'r']);
    let long = "l".repeat(255);
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 10));
    a.file("new\nline.txt", b"nl");
    a.file("-rf", b"dash");
    a.file("--help", b"dash2");
    a.file(&non_utf8, b"bytes");
    let nested: PathBuf = [non_utf8_dir.clone(), OsString::from("in.txt")]
        .iter()
        .collect();
    a.file(nested.as_os_str(), b"nested");
    a.file(&long, b"long");
    a.file("sp ace  and 'quote' \"dq\" $x `y` ;.txt", b"shell");
    let mut r = a.reader();
    let run = run_plain(&mut r, &[], &dst, &opts());
    run.ok();
    assert_eq!(read(&dst.join("new\nline.txt")), b"nl");
    assert_eq!(read(&dst.join("-rf")), b"dash");
    assert_eq!(read(&dst.join("--help")), b"dash2");
    assert_eq!(read(&dst.join(&non_utf8)), b"bytes");
    assert_eq!(read(&dst.join(&non_utf8_dir).join("in.txt")), b"nested");
    assert_eq!(read(&dst.join(&long)), b"long");
    assert_eq!(
        read(&dst.join("sp ace  and 'quote' \"dq\" $x `y` ;.txt")),
        b"shell"
    );
    assert_no_part_files(&dst);
    // A selection by non-UTF-8 name works too.
    let dst2 = tmp.path().join("dst2");
    let mut r = a.reader();
    let run = run_paths(
        &mut r,
        &[PathBuf::from(non_utf8_dir.clone())],
        &dst2,
        &opts(),
    );
    run.ok();
    assert_eq!(read(&dst2.join(&non_utf8_dir).join("in.txt")), b"nested");
}

#[test]
fn safe_symlinks_are_recreated_and_escaping_ones_skipped() {
    let tmp = Tmp::new("links");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 10));
    a.file("real.txt", b"r");
    a.dir("d");
    a.symlink("rel", "real.txt");
    a.symlink("d/up", "../real.txt");
    a.symlink("dirlink", "d");
    a.symlink("dangling", "nothing-here");
    a.symlink("d/climb", "../../outside"); // above the destination
    a.symlink("abs", "/etc/passwd"); // absolute
    a.symlink("dotdot", "d/../real.txt"); // `..` after a name
    let mut r = a.reader();
    let run = run_plain(&mut r, &[], &dst, &opts());
    run.ok();
    let link = |p: &str| std::fs::read_link(dst.join(p)).unwrap();
    assert_eq!(link("rel"), Path::new("real.txt"));
    assert_eq!(link("d/up"), Path::new("../real.txt"));
    assert_eq!(link("dirlink"), Path::new("d"));
    assert_eq!(link("dangling"), Path::new("nothing-here"));
    assert_eq!(read(&dst.join("d/up")), b"r", "the link works");
    for gone in ["d/climb", "abs", "dotdot"] {
        assert_absent(&dst.join(gone));
    }
    let w = run.warnings().join("\n");
    assert!(
        w.contains("climb") && w.contains("abs") && w.contains("dotdot"),
        "{w}"
    );
    assert_no_part_files(&dst);
}

#[test]
fn hard_links_only_to_files_extracted_by_this_job() {
    let tmp = Tmp::new("hard");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 10));
    a.file("orig", b"shared");
    a.file("other", b"unrelated");
    a.hardlink("copy", "orig");
    a.hardlink("stray", "not-in-this-job");
    let mut r = a.reader();
    let run = run_plain(&mut r, &["orig", "copy", "stray"], &dst, &opts());
    run.ok();
    let (o, c) = (
        std::fs::metadata(dst.join("orig")).unwrap(),
        std::fs::metadata(dst.join("copy")).unwrap(),
    );
    assert_eq!(o.ino(), c.ino(), "one inode");
    assert_eq!(o.nlink(), 2);
    assert_eq!(read(&dst.join("copy")), b"shared");
    assert_absent(&dst.join("stray"));
    assert_absent(&dst.join("other"));
    assert!(run.warnings().iter().any(|w| w.contains("stray")));
    assert_no_part_files(&dst);

    // Selecting only the link: its target is not part of this job, so nothing is created.
    let dst2 = tmp.path().join("dst2");
    let mut r = a.reader();
    let run = run_plain(&mut r, &["copy"], &dst2, &opts());
    run.ok();
    assert_absent(&dst2.join("copy"));
    assert!(
        run.warnings()
            .iter()
            .any(|w| w.contains("target was not extracted"))
    );
}

#[test]
fn devices_fifos_and_sockets_are_never_created() {
    let tmp = Tmp::new("special");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 10));
    a.other("null").other("pipe").file("normal", b"n");
    let mut r = a.reader();
    let run = run_plain(&mut r, &[], &dst, &opts());
    run.ok();
    assert_absent(&dst.join("null"));
    assert_absent(&dst.join("pipe"));
    assert_eq!(read(&dst.join("normal")), b"n");
    assert_eq!(run.report.warnings.len(), 2);
}

#[test]
fn many_small_entries_stay_fast() {
    let tmp = Tmp::new("perf");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 10));
    for d in 0..100 {
        a.dir(&format!("d{d:03}"));
        for f in 0..100 {
            a.file(
                format!("d{d:03}/f{f:03}.txt"),
                format!("{d}-{f}").as_bytes(),
            );
        }
    }
    let mut r = a.reader();
    let t = std::time::Instant::now();
    let run = run_plain(&mut r, &[], &dst, &opts());
    let elapsed = t.elapsed();
    assert_eq!(run.ok().files_done, 10_000);
    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "10k entries took {elapsed:?}"
    );
    eprintln!("10k entries: {elapsed:?}");
    assert_eq!(read(&dst.join("d042/f077.txt")), b"42-77");
    assert_no_part_files(&dst);
}

#[test]
fn nothing_selected_is_a_warning_not_a_failure() {
    let tmp = Tmp::new("none");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 10));
    a.file("x", b"x");
    let mut r = a.reader();
    let run = run_plain(&mut r, &["no/such/path"], &dst, &opts());
    run.ok();
    assert!(snapshot(&dst).is_empty());
    assert_eq!(run.report.warnings.len(), 1);
}

#[test]
fn progress_reports_totals_and_the_current_entry() {
    let tmp = Tmp::new("progress");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 10));
    a.file("one", &bytes(1, 5000)).file("two", &bytes(2, 7000));
    let mut r = a.reader();
    let run = run_plain(&mut r, &[], &dst, &opts());
    run.ok();
    let rec = run.rec.lock().unwrap();
    let last = rec.progress.last().unwrap();
    assert_eq!((last.files_total, last.bytes_total), (2, 12_000));
    assert_eq!((last.files_done, last.bytes_done), (2, 12_000));
    assert!(!last.planning);
    assert!(
        rec.progress.iter().any(|p| p
            .current
            .as_deref()
            .is_some_and(|c| c.ends_with("one") || c.ends_with("two")))
            || rec.progress.len() == 1,
        "the current entry is reported"
    );
}

#[test]
fn an_entry_named_like_the_archive_itself_never_replaces_it() {
    let tmp = Tmp::new("selfname");
    // Extract into the directory that holds the archive; the archive holds a file of its own name.
    let arch = tmp.archive_file("same.zip", 10);
    let before = std::fs::read(&arch).unwrap();
    let mut a = MemArchive::new(&arch);
    a.file("same.zip", b"evil replacement");
    let mut r = a.reader();
    // Even a silent "overwrite" policy must not replace the archive (SameFile conflict).
    let run = run_policy(
        &mut r,
        &[],
        tmp.path(),
        &opts(),
        ofm_jobs::ConflictAnswer::Overwrite,
    );
    run.ok();
    assert_eq!(std::fs::read(&arch).unwrap(), before);
    assert_no_part_files(tmp.path());
}
