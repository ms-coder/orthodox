//! T3.4 torture suite — copy (spec F17, ADR-0004). Every test is ignored until T3.1 lands; the
//! reason string names the task that must make it pass.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod torture_common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ofm_jobs::ScriptedSink;
use ofm_jobs::event::{ConflictAnswer, ErrorAction, Outcome};
use ofm_jobs::fs::RealFs;
use ofm_jobs::spec::CopyOpts;
use ofm_testkit::tree::{DirSpec, FileSpec, TreeSpec, raw_name};

use torture_common::{
    Rng, assert_full_tree_equal, assert_no_part_files, copy_spec, random_tree, report_of,
    run_policy, running_as_root, sole_entry, src_dst, weird_names, write_file, xattrs_supported,
};

/// RealFs as `Arc<dyn Fs>` (what every non-faulted run uses).
fn real() -> Arc<dyn ofm_jobs::Fs> {
    Arc::new(RealFs)
}

/// Copy the direct children of `src` into `dst` (multi-source copy), refusing sink.
fn copy_children(src: &Path, dst: &Path, opts: CopyOpts) -> Outcome {
    let mut sources: Vec<PathBuf> = std::fs::read_dir(src)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    sources.sort();
    let (sink, _rec) = ScriptedSink::refusing();
    ofm_jobs::run_sync(&copy_spec(&sources, dst, opts), real(), sink)
}

#[test]
#[ignore = "T3.1"]
fn copy_roundtrip_preserves_content_mode_mtime_and_created_list() {
    let (src, dst) = src_dst("copy-basic");
    let m = |days: i128| ofm_testkit::tree::DEFAULT_MTIME_NS + days * 86_400_000_000_000;
    let spec = TreeSpec::new()
        .file(
            "readme.md",
            FileSpec::content(b"# ofm\n").mode(0o644).mtime_ns(m(1)),
        )
        .file(
            "run.sh",
            FileSpec::content(b"#!/bin/sh\nexit 0\n")
                .mode(0o755)
                .mtime_ns(m(2)),
        )
        .file(
            "locked",
            FileSpec::content(b"read-only but copyable")
                .mode(0o400)
                .mtime_ns(m(3)),
        )
        .file("empty", FileSpec::content(b"").mtime_ns(m(4)))
        .dir(
            "sub",
            DirSpec::new(TreeSpec::new().file("data.bin", FileSpec::size(300_003)))
                .mode(0o750)
                .mtime_ns(m(5)),
        )
        .dir("empty-dir", TreeSpec::new());
    spec.build(src.path()).unwrap();
    std::fs::create_dir_all(dst.path()).unwrap();

    let out = copy_children(src.path(), dst.path(), CopyOpts::default());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_full_tree_equal(src.path(), dst.path());

    // The report names every final destination path.
    let rep = report_of(&out, "copy-basic");
    let mut created = rep.created.clone();
    created.sort();
    let mut expected: Vec<PathBuf> = std::fs::read_dir(dst.path())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    expected.sort();
    assert_eq!(created, expected, "report.created must name the results");
}

#[test]
#[ignore = "T3.1"]
fn copy_preserves_user_xattrs() {
    let (src, dst) = src_dst("copy-xattr");
    if !xattrs_supported(src.path()) {
        eprintln!("SKIP: user.* xattrs not supported on the temp filesystem");
        return;
    }
    let spec = TreeSpec::new()
        .file(
            "with-xattrs",
            FileSpec::content(b"x")
                .xattr("user.ofm.test", b"value-1")
                .xattr("user.ofm.second", b"\x00\x01\xff"),
        )
        .dir(
            "dir-x",
            DirSpec::new(TreeSpec::new()).xattr("user.ofm.dir", b"d"),
        );
    spec.build(src.path()).unwrap();
    std::fs::create_dir_all(dst.path()).unwrap();

    let out = copy_children(src.path(), dst.path(), CopyOpts::default());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_full_tree_equal(src.path(), dst.path());
}

#[test]
#[ignore = "T3.1"]
fn copy_handles_weird_names() {
    let (src, dst) = src_dst("copy-names");
    let mut spec = TreeSpec::new();
    for (i, name) in weird_names().iter().enumerate() {
        let content = format!("content-{i}").into_bytes();
        spec = spec.file(name.clone(), FileSpec::content(&content));
    }
    spec = spec.dir(
        raw_name(b"dir-with-\xff-inside"),
        TreeSpec::new().file(raw_name(b"nested\nbad"), FileSpec::content(b"nested")),
    );
    spec.build(src.path()).unwrap();
    std::fs::create_dir_all(dst.path()).unwrap();

    let out = copy_children(src.path(), dst.path(), CopyOpts::default());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_full_tree_equal(src.path(), dst.path());
    assert_no_part_files(dst.path());
}

#[test]
#[ignore = "T3.1"]
fn copy_max_length_names() {
    // 255 bytes = NAME_MAX. The engine's temp name `.<name>.ofm-part-<suffix>` would exceed it,
    // so a correct implementation must truncate the temp name, not the final one.
    let (src, dst) = src_dst("copy-255");
    let long_file: String = "f".repeat(255);
    let long_dir: String = "d".repeat(250);
    let spec = TreeSpec::new()
        .file(long_file.clone(), FileSpec::content(b"long name"))
        .dir(
            long_dir.clone(),
            TreeSpec::new().file("x", FileSpec::content(b"x")),
        );
    spec.build(src.path()).unwrap();
    std::fs::create_dir_all(dst.path()).unwrap();

    let out = copy_children(src.path(), dst.path(), CopyOpts::default());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(dst.path().join(&long_file).is_file());
    assert!(dst.path().join(&long_dir).join("x").is_file());
    assert_no_part_files(dst.path());
}

#[test]
#[ignore = "T3.1"]
fn copy_200_level_deep_tree() {
    let (src, dst) = src_dst("copy-deep");
    let mut inner = TreeSpec::new().file("bottom.txt", FileSpec::content(b"deep"));
    for i in 0..200 {
        inner = TreeSpec::new().dir(format!("n{}", i % 7), inner);
    }
    TreeSpec::new()
        .dir("deep", DirSpec::new(inner).mode(0o700))
        .build(src.path())
        .unwrap();
    std::fs::create_dir_all(dst.path()).unwrap();

    let out = copy_children(src.path(), dst.path(), CopyOpts::default());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_full_tree_equal(src.path(), dst.path());
}

#[test]
#[ignore = "T3.1"]
fn copy_10k_small_files() {
    let (src, dst) = src_dst("copy-10k");
    let mut spec = TreeSpec::new();
    for d in 0..10 {
        let mut sub = TreeSpec::new();
        for f in 0..1000u32 {
            sub = sub.file(
                format!("f{f:04}"),
                FileSpec::content(format!("{d}-{f}").as_bytes()),
            );
        }
        spec = spec.dir(format!("d{d}"), sub);
    }
    spec.build(src.path()).unwrap();
    std::fs::create_dir_all(dst.path()).unwrap();

    let out = copy_children(src.path(), dst.path(), CopyOpts::default());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_full_tree_equal(src.path(), dst.path());
}

#[test]
#[ignore = "T3.1"]
fn copy_recreates_symlinks_without_following() {
    let (src, dst) = src_dst("copy-links");
    let abs_target = src.path().join("readme.md");
    let spec = TreeSpec::new()
        .file("readme.md", FileSpec::content(b"target"))
        .dir(
            "sub",
            TreeSpec::new().file("in-dir", FileSpec::content(b"s")),
        )
        .symlink("rel-link", "readme.md")
        .symlink("abs-link", &abs_target)
        .symlink("broken", "does-not-exist")
        .symlink("loop-a", "loop-b")
        .symlink("loop-b", "loop-a")
        .symlink("dir-link", "sub");
    spec.build(src.path()).unwrap();
    std::fs::create_dir_all(dst.path()).unwrap();

    let (sink, rec) = ScriptedSink::refusing();
    let sources = [src.path().join("readme.md"), src.path().join("sub")]
        .into_iter()
        .chain(
            [
                "rel-link", "abs-link", "broken", "loop-a", "loop-b", "dir-link",
            ]
            .iter()
            .map(|n| src.path().join(n)),
        )
        .collect::<Vec<_>>();
    let out = ofm_jobs::run_sync(
        &copy_spec(&sources, dst.path(), CopyOpts::default()),
        real(),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(
        rec.lock().unwrap().questions.is_empty(),
        "no question expected: {:?}",
        rec.lock().unwrap().questions
    );
    assert_full_tree_equal(src.path(), dst.path());
    // The loop pair must exist as links, not as followed content.
    assert!(
        std::fs::symlink_metadata(dst.path().join("loop-a"))
            .unwrap()
            .is_symlink()
    );
}

#[test]
#[ignore = "T3.1"]
fn copy_follow_symlinks_option_copies_targets() {
    let (src, dst) = src_dst("copy-follow");
    let spec = TreeSpec::new()
        .file("real.bin", FileSpec::content(b"REAL-DATA"))
        .symlink("to-real", "real.bin")
        .symlink("to-broken", "gone");
    spec.build(src.path()).unwrap();
    std::fs::create_dir_all(dst.path()).unwrap();

    let opts = CopyOpts {
        follow_symlinks: true,
        ..CopyOpts::default()
    };
    let sources = [src.path().join("to-real"), src.path().join("to-broken")];
    let (out, _rec) = run_policy(
        &copy_spec(&sources, dst.path(), opts),
        real(),
        ConflictAnswer::Skip,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    // The followed link became a regular file with the target's content.
    assert_eq!(
        std::fs::read(dst.path().join("to-real")).unwrap(),
        b"REAL-DATA"
    );
    assert!(
        !std::fs::symlink_metadata(dst.path().join("to-real"))
            .unwrap()
            .is_symlink()
    );
    // The broken link could not be followed: skipped, never recreated as a link.
    assert!(!dst.path().join("to-broken").exists());
    assert!(report_of(&out, "follow").skipped >= 1);
    assert_no_part_files(dst.path());
}

#[test]
#[ignore = "T3.1"]
fn copy_hardlinks_become_separate_files() {
    let (src, dst) = src_dst("copy-hardlink");
    let spec = TreeSpec::new()
        .file("original.txt", FileSpec::content(b"shared inode"))
        .hardlink("twin.txt", "original.txt");
    spec.build(src.path()).unwrap();
    std::fs::create_dir_all(dst.path()).unwrap();

    let out = copy_children(src.path(), dst.path(), CopyOpts::default());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_full_tree_equal(src.path(), dst.path());
    // Documented behavior: links inside the set are copied as independent files.
    use std::os::unix::fs::MetadataExt;
    let a = std::fs::metadata(dst.path().join("original.txt"))
        .unwrap()
        .nlink();
    let b = std::fs::metadata(dst.path().join("twin.txt"))
        .unwrap()
        .nlink();
    assert_eq!((a, b), (1, 1), "copied files must not share an inode");
}

#[test]
#[ignore = "T3.1"]
fn copy_2g_sparse_file_stays_sparse() {
    let (src, dst) = src_dst("copy-sparse2g");
    const SIZE: u64 = 2 << 30;
    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let spec = TreeSpec::new().file(
        "huge.sparse",
        FileSpec::sparse_ranges(SIZE, &[(1 << 30, &payload)]),
    );
    spec.build(src.path()).unwrap();
    std::fs::create_dir_all(dst.path()).unwrap();

    let out = copy_children(src.path(), dst.path(), CopyOpts::default());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");

    let dest = dst.path().join("huge.sparse");
    let md = std::fs::symlink_metadata(&dest).unwrap();
    use std::os::unix::fs::MetadataExt as _;
    assert_eq!(md.len(), SIZE, "logical size must be 2 GiB");
    assert!(
        md.blocks() * 512 < SIZE,
        "copy must stay sparse: {} allocated bytes for a 2 GiB file",
        md.blocks() * 512
    );
    // Data where the source had data, zeros where it had holes (sampled).
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(&dest).unwrap();
    let mut buf = vec![0u8; 4096];
    f.seek(SeekFrom::Start(1 << 30)).unwrap();
    f.read_exact(&mut buf).unwrap();
    assert_eq!(buf, payload, "data region must match");
    for offset in [0u64, 1 << 20, SIZE - 16] {
        let mut probe = [0u8; 16];
        f.seek(SeekFrom::Start(offset)).unwrap();
        f.read_exact(&mut probe).unwrap();
        assert!(
            probe.iter().all(|&b| b == 0),
            "hole at {offset} must stay zeros"
        );
    }
    assert_no_part_files(dst.path());
}

#[test]
#[ignore = "T3.1"]
fn copy_sparse_ranges_match_exactly() {
    let (src, dst) = src_dst("copy-sparse-ranges");
    let a: Vec<u8> = (0..4096u32).map(|i| (i % 7) as u8).collect();
    let b: Vec<u8> = (0..8192u32).map(|i| (i % 13) as u8).collect();
    let tail: [u8; 3] = [1, 2, 3];
    let spec = TreeSpec::new().file(
        "ranges.bin",
        FileSpec::sparse_ranges(
            64 << 20,
            &[(0, &a), (5 << 20, &b), ((64 << 20) - 10, &tail)],
        ),
    );
    spec.build(src.path()).unwrap();
    std::fs::create_dir_all(dst.path()).unwrap();

    let out = copy_children(src.path(), dst.path(), CopyOpts::default());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_full_tree_equal(src.path(), dst.path());
}

#[test]
#[ignore = "T3.1"]
fn copy_skips_unreadable_file_and_copies_the_rest() {
    if running_as_root() {
        eprintln!("SKIP: running as root, mode 0o000 does not block reads");
        return;
    }
    let (src, dst) = src_dst("copy-unreadable");
    write_file(&src.path().join("good-1.txt"), b"one", 0o644, 0);
    write_file(&src.path().join("secret.bin"), b"cannot read me", 0o000, 0);
    write_file(&src.path().join("good-2.txt"), b"two", 0o644, 0);
    std::fs::create_dir_all(dst.path()).unwrap();

    let (out, _rec) = run_policy(
        &copy_spec(
            &[
                src.path().join("good-1.txt"),
                src.path().join("secret.bin"),
                src.path().join("good-2.txt"),
            ],
            dst.path(),
            CopyOpts::default(),
        ),
        real(),
        ConflictAnswer::Skip,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        std::fs::read(dst.path().join("good-1.txt")).unwrap(),
        b"one"
    );
    assert_eq!(
        std::fs::read(dst.path().join("good-2.txt")).unwrap(),
        b"two"
    );
    assert!(
        !dst.path().join("secret.bin").exists(),
        "skipped file must not appear"
    );
    assert_eq!(report_of(&out, "unreadable").skipped, 1);
    assert_no_part_files(dst.path());
}

#[test]
#[ignore = "T3.1"]
fn copy_into_read_only_dest_dir_fails_cleanly() {
    if running_as_root() {
        eprintln!("SKIP: running as root, mode 0o500 does not block writes");
        return;
    }
    let (src, dst) = src_dst("copy-ro-dest");
    write_file(&src.path().join("a.txt"), b"content", 0o644, 0);
    write_file(&dst.path().join("sentinel"), b"keep me", 0o644, 0);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dst.path(), std::fs::Permissions::from_mode(0o500)).unwrap();

    let (out, _rec) = run_policy(
        &copy_spec(&[src.path().join("a.txt")], dst.path(), CopyOpts::default()),
        real(),
        ConflictAnswer::Skip,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        std::fs::read(dst.path().join("sentinel")).unwrap(),
        b"keep me"
    );
    assert!(!dst.path().join("a.txt").exists());
    assert_eq!(report_of(&out, "ro-dest").skipped, 1);

    // Restore so the TempDir cleanup can remove the tree.
    std::fs::set_permissions(dst.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
#[ignore = "T3.1"]
fn copy_creates_missing_dest_dir() {
    let (src, dst) = src_dst("copy-mkdir");
    write_file(&src.path().join("a.txt"), b"deep dest", 0o644, 0);
    let dest = dst.path().join("new/deeper/still");

    let (sink, _) = ScriptedSink::refusing();
    let out = ofm_jobs::run_sync(
        &copy_spec(&[src.path().join("a.txt")], &dest, CopyOpts::default()),
        real(),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"deep dest");
}

#[test]
#[ignore = "T3.1"]
fn copy_destination_inside_source_is_refused() {
    let (src, _dst) = src_dst("copy-inside");
    let spec = TreeSpec::new()
        .file("f.txt", FileSpec::content(b"x"))
        .dir("sub", TreeSpec::new());
    spec.build(src.path()).unwrap();

    let (sink, _) = ScriptedSink::refusing();
    let out = ofm_jobs::run_sync(
        &copy_spec(
            &[src.path().to_path_buf()],
            &src.path().join("sub"),
            CopyOpts::default(),
        ),
        real(),
        sink,
    );
    match &out {
        Outcome::Failed { error, .. } => assert!(
            error.to_lowercase().contains("inside"),
            "expected a dest-inside-source error, got: {error}"
        ),
        other => panic!("copy into itself must fail, got {other:?}"),
    }
    // Nothing was written into the destination and no temp survived.
    let sub = src.path().join("sub");
    assert_eq!(std::fs::read_dir(&sub).unwrap().count(), 0);
    assert_no_part_files(src.path());
}

#[test]
#[ignore = "T3.1"]
fn copy_rejects_invalid_requests_before_io() {
    let (src, dst) = src_dst("copy-invalid");

    let (sink, _) = ScriptedSink::refusing();
    let out = ofm_jobs::run_sync(
        &copy_spec(&[], dst.path(), CopyOpts::default()),
        real(),
        sink,
    );
    assert!(
        matches!(&out, Outcome::Failed { error, .. } if error.contains("invalid")),
        "empty source list: {out:?}"
    );

    let (sink, _) = ScriptedSink::refusing();
    // Unterminated class: rejected by RenameMask::parse.
    let opts = CopyOpts {
        rename_mask: Some("[".into()),
        ..CopyOpts::default()
    };
    let out = ofm_jobs::run_sync(
        &copy_spec(&[src.path().join("whatever")], dst.path(), opts),
        real(),
        sink,
    );
    assert!(
        matches!(&out, Outcome::Failed { error, .. } if error.contains("invalid")),
        "bad rename mask: {out:?}"
    );
    assert!(!dst.path().join("whatever").exists());
}

#[test]
#[ignore = "T3.1"]
fn copy_applies_rename_masks() {
    let (src, dst) = src_dst("copy-mask");
    let spec = TreeSpec::new()
        .file("a.txt", FileSpec::content(b"a"))
        .file("notes.md", FileSpec::content(b"n"));
    spec.build(src.path()).unwrap();
    std::fs::create_dir_all(dst.path()).unwrap();

    let opts = CopyOpts {
        rename_mask: Some("*.bak".into()),
        ..CopyOpts::default()
    };
    let out = copy_children(src.path(), dst.path(), opts);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(std::fs::read(dst.path().join("a.bak")).unwrap(), b"a");
    assert_eq!(std::fs::read(dst.path().join("notes.bak")).unwrap(), b"n");
    assert!(!dst.path().join("a.txt").exists());
}

#[test]
#[ignore = "T3.1"]
fn copy_literal_mask_renames_a_single_file() {
    let (src, dst) = src_dst("copy-literal");
    write_file(&src.path().join("f.txt"), b"renamed copy", 0o644, 0);

    let opts = CopyOpts {
        rename_mask: Some("renamed.dat".into()),
        ..CopyOpts::default()
    };
    let (sink, _) = ScriptedSink::refusing();
    let out = ofm_jobs::run_sync(
        &copy_spec(&[src.path().join("f.txt")], dst.path(), opts),
        real(),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        std::fs::read(dst.path().join("renamed.dat")).unwrap(),
        b"renamed copy"
    );
    assert!(src.path().join("f.txt").exists(), "copy keeps the source");
}

#[test]
#[ignore = "T3.1"]
fn copy_random_trees_match_exactly() {
    for seed in 1..=8u64 {
        let (src, dst) = src_dst(&format!("copy-rnd-{seed}"));
        let mut rng = Rng::new(seed);
        let spec = random_tree(&mut rng, src.path(), 0);
        spec.build(src.path()).unwrap();
        std::fs::create_dir_all(dst.path()).unwrap();

        let out = copy_children(src.path(), dst.path(), CopyOpts::default());
        assert!(matches!(out, Outcome::Ok(_)), "seed {seed}: {out:?}");
        assert_full_tree_equal(src.path(), dst.path());
        assert_no_part_files(dst.path());
    }
}

#[test]
#[ignore = "T3.1"]
fn copy_source_tree_of_one_dir() {
    // Copying the source DIRECTORY itself (not its children): result sits under dst/<name>/.
    let (src, dst) = src_dst("copy-dirtree");
    let spec = TreeSpec::new().dir(
        "tree",
        DirSpec::new(TreeSpec::new().file("leaf", FileSpec::content(b"leaf")))
            .mode(0o755)
            .mtime_ns(ofm_testkit::tree::DEFAULT_MTIME_NS + 7_777_000_000_000),
    );
    spec.build(src.path()).unwrap();

    let (sink, _) = ScriptedSink::refusing();
    let out = ofm_jobs::run_sync(
        &copy_spec(&[src.path().join("tree")], dst.path(), CopyOpts::default()),
        real(),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    let copied = sole_entry(dst.path());
    assert!(copied.ends_with("tree"), "{copied:?}");
    assert_full_tree_equal(&src.path().join("tree"), &copied);
}
