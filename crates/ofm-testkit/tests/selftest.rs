//! Self-tests for the test kit (T1.4): the kit verifies itself the way
//! downstream crates (ofm-vfs, ofm-jobs) will use it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::time::UNIX_EPOCH;

use ofm_testkit::compare::{Compare, TreeDiff, assert_trees_equal, compare_trees};
use ofm_testkit::crossdev::{st_dev, two_filesystems};
use ofm_testkit::temp::TempDir;
use ofm_testkit::tree::{DirSpec, FileSpec, TreeSpec, raw_name};

/// A fixed timestamp well in the past: 2000-01-01T00:00:00Z.
const T2000_NS: i128 = 946_684_800_000_000_000;

fn kitchen_sink_spec() -> TreeSpec {
    TreeSpec::new()
        .file("readme.txt", FileSpec::content(b"hello\n"))
        .file(
            "run.sh",
            FileSpec::content(b"#!/bin/sh\nexit 0\n")
                .mode(0o755)
                .mtime_ns(T2000_NS)
                .xattr("user.ofm.test", b"yes"),
        )
        .file("zeros.bin", FileSpec::size(300_000))
        .file("hole.bin", FileSpec::sparse(64 << 20))
        .file(
            "patched.bin",
            FileSpec::sparse_ranges(8 << 20, &[(1 << 20, b"DATA-AT-1M"), (4 << 20, b"AT-4M")]),
        )
        .file(
            raw_name(b"invalid-utf8-\xff\xfe.txt"),
            FileSpec::content(b"bytes"),
        )
        .dir(
            "sub",
            DirSpec::new(TreeSpec::new().file("nested.txt", FileSpec::content(b"deep")))
                .mode(0o750)
                .mtime_ns(T2000_NS)
                .xattr("user.ofm.dir", b"1"),
        )
        .symlink("rel-link", "readme.txt")
        .symlink("broken-link", "does-not-exist")
        .hardlink("twin.txt", "readme.txt")
}

fn mtime_ns_of(path: &Path) -> i128 {
    let t = fs::symlink_metadata(path).unwrap().modified().unwrap();
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
}

#[test]
fn builds_every_node_kind() {
    let dir = kitchen_sink_spec().build_temp().unwrap();
    let root = dir.path();

    assert_eq!(fs::read(root.join("readme.txt")).unwrap(), b"hello\n");

    let run = root.join("run.sh");
    assert_eq!(
        fs::metadata(&run).unwrap().permissions().mode() & 0o7777,
        0o755
    );
    assert_eq!(mtime_ns_of(&run), T2000_NS);

    // zeros: real size on disk
    assert_eq!(fs::metadata(root.join("zeros.bin")).unwrap().len(), 300_000);

    // sparse: logical size, few blocks
    let md = fs::metadata(root.join("hole.bin")).unwrap();
    assert_eq!(md.len(), 64 << 20);
    if md.blocks() * 512 >= md.len() {
        eprintln!(
            "note: fs reports no holes for len={} (blocks={}); skipping sparse assertion",
            md.len(),
            md.blocks()
        );
    } else {
        assert!(md.blocks() * 512 < md.len() / 2, "expected a sparse file");
    }

    // sparse with data ranges: bytes at the right offsets
    let patched = fs::read(root.join("patched.bin")).unwrap();
    assert_eq!(patched.len(), 8 << 20);
    assert_eq!(&patched[1 << 20..(1 << 20) + 10], b"DATA-AT-1M");
    assert_eq!(&patched[4 << 20..(4 << 20) + 5], b"AT-4M");
    assert!(patched[..1 << 20].iter().all(|&b| b == 0));

    // non-UTF-8 name exists, byte-exact
    let bad = raw_name(b"invalid-utf8-\xff\xfe.txt");
    assert!(fs::symlink_metadata(root.join(&bad)).is_ok());
    assert_eq!(fs::read(root.join(&bad)).unwrap(), b"bytes");

    // directory: final mode/mtime/xattr applied post-order, child present
    let sub = root.join("sub");
    assert_eq!(
        fs::metadata(&sub).unwrap().permissions().mode() & 0o7777,
        0o750
    );
    assert_eq!(mtime_ns_of(&sub), T2000_NS);
    assert_eq!(fs::read(sub.join("nested.txt")).unwrap(), b"deep");
    assert!(
        fs::read_dir(&sub)
            .unwrap()
            .any(|e| e.unwrap().file_name() == "nested.txt")
    );

    // symlinks: relative + dangling
    assert_eq!(
        fs::read_link(root.join("rel-link")).unwrap(),
        Path::new("readme.txt")
    );
    assert_eq!(
        fs::read_link(root.join("broken-link")).unwrap(),
        Path::new("does-not-exist")
    );
    assert!(
        fs::symlink_metadata(root.join("broken-link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );

    // hardlink: same file (dev + ino)
    let (a, b) = (
        fs::metadata(root.join("readme.txt")).unwrap(),
        fs::metadata(root.join("twin.txt")).unwrap(),
    );
    assert_eq!((a.dev(), a.ino()), (b.dev(), b.ino()));

    // entry count is recursive; empty spec is empty
    assert_eq!(kitchen_sink_spec().len(), 11);
    assert!(TreeSpec::new().is_empty());
}

#[test]
fn fixed_mtimes_survive() {
    let dir = TreeSpec::new()
        .file("f", FileSpec::content(b"x").mtime_ns(T2000_NS))
        .dir("d", DirSpec::new(TreeSpec::new()).mtime_ns(T2000_NS - 1))
        .build_temp()
        .unwrap();
    assert_eq!(mtime_ns_of(&dir.path().join("f")), T2000_NS);
    assert_eq!(mtime_ns_of(&dir.path().join("d")), T2000_NS - 1);
}

#[test]
fn xattrs_are_set() {
    let dir = TreeSpec::new()
        .file("f", FileSpec::content(b"x").xattr("user.ofm.test", b"v1"))
        .build_temp()
        .unwrap();
    let v = xattr::get(dir.path().join("f"), "user.ofm.test")
        .unwrap()
        .unwrap();
    assert_eq!(v, b"v1");
}

#[test]
fn same_spec_builds_equal_trees() {
    let spec = kitchen_sink_spec();
    let a = spec.build_temp().unwrap();
    let b = spec.build_temp().unwrap();
    assert_trees_equal(a.path(), b.path(), Compare::all());
}

#[test]
fn compare_reports_each_kind_of_difference() {
    // Per-file specs so each check can vary exactly one aspect (the DSL
    // rejects duplicate names, so mutations must be real spec variants).
    let same_def = || FileSpec::content(b"aaaa");
    let mode_def = || FileSpec::content(b"m").mode(0o600);
    let time_def = || FileSpec::content(b"t").mtime_ns(T2000_NS);
    let x_def = || {
        FileSpec::content(b"x")
            .xattr("user.ofm.a", b"1")
            .xattr("user.ofm.b", b"2")
    };

    let base_spec = |same: FileSpec, mode_f: FileSpec, time_f: FileSpec, x: FileSpec| {
        TreeSpec::new()
            .file("same.txt", same)
            .file("mode.txt", mode_f)
            .file("time.txt", time_f)
            .file("x.txt", x)
            .symlink("lnk", "same.txt")
            .dir("d", TreeSpec::new().file("inner", FileSpec::content(b"i")))
    };
    let mk = |same: FileSpec, mode_f: FileSpec, time_f: FileSpec, x: FileSpec| {
        base_spec(same, mode_f, time_f, x).build_temp().unwrap()
    };
    let plain = || mk(same_def(), mode_def(), time_def(), x_def());

    // extra / missing entries
    let a = base_spec(same_def(), mode_def(), time_def(), x_def())
        .file("extra.txt", FileSpec::content(b"e"))
        .build_temp()
        .unwrap();
    let b = plain();
    let d = compare_trees(a.path(), b.path(), Compare::all());
    assert!(
        matches!(d.as_slice(), [TreeDiff::OnlyInA { path }] if path == Path::new("extra.txt")),
        "{d:?}"
    );
    let d = compare_trees(b.path(), a.path(), Compare::all());
    assert!(matches!(d.as_slice(), [TreeDiff::OnlyInB { .. }]), "{d:?}");

    // content: first differing byte offset
    let a = plain();
    let b = mk(FileSpec::content(b"aaba"), mode_def(), time_def(), x_def());
    let d = compare_trees(a.path(), b.path(), Compare::all());
    assert!(
        matches!(d.as_slice(), [TreeDiff::Content { offset: 2, .. }]),
        "{d:?}"
    );

    // size (implies content, reported once as Size)
    let b = mk(
        FileSpec::content(b"aaaaaa"),
        mode_def(),
        time_def(),
        x_def(),
    );
    let d = compare_trees(a.path(), b.path(), Compare::all());
    assert!(
        matches!(d.as_slice(), [TreeDiff::Size { a: 4, b: 6, .. }]),
        "{d:?}"
    );

    // mode
    let b = mk(
        same_def(),
        FileSpec::content(b"m").mode(0o644),
        time_def(),
        x_def(),
    );
    let d = compare_trees(a.path(), b.path(), Compare::all());
    assert!(
        matches!(
            d.as_slice(),
            [TreeDiff::Mode {
                a: 0o600,
                b: 0o644,
                ..
            }]
        ),
        "{d:?}"
    );

    // mtime
    let b = mk(
        same_def(),
        mode_def(),
        FileSpec::content(b"t").mtime_ns(T2000_NS + 1),
        x_def(),
    );
    let d = compare_trees(a.path(), b.path(), Compare::all());
    assert!(
        matches!(d.as_slice(), [TreeDiff::Mtime { a_ns, b_ns, .. }] if *a_ns == T2000_NS && *b_ns == T2000_NS + 1),
        "{d:?}"
    );

    // xattr missing on one side / differing value
    let b = mk(
        same_def(),
        mode_def(),
        time_def(),
        FileSpec::content(b"x").xattr("user.ofm.a", b"1"),
    );
    let d = compare_trees(a.path(), b.path(), Compare::all());
    assert!(
        matches!(d.as_slice(), [TreeDiff::XattrMissing { in_b: true, name, .. }] if name.to_string_lossy() == "user.ofm.b"),
        "{d:?}"
    );
    let b = mk(
        same_def(),
        mode_def(),
        time_def(),
        FileSpec::content(b"x")
            .xattr("user.ofm.a", b"1")
            .xattr("user.ofm.b", b"changed"),
    );
    let d = compare_trees(a.path(), b.path(), Compare::all());
    assert!(
        matches!(d.as_slice(), [TreeDiff::XattrValue { .. }]),
        "{d:?}"
    );

    // symlink target (always compared); retarget directly on disk
    let b = plain();
    let p = b.path().join("lnk");
    fs::remove_file(&p).unwrap();
    std::os::unix::fs::symlink("mode.txt", &p).unwrap();
    let d = compare_trees(a.path(), b.path(), Compare::all());
    assert!(
        matches!(d.as_slice(), [TreeDiff::LinkTarget { .. }]),
        "{d:?}"
    );

    // kind mismatch: file vs dir; no crash from the child below it
    // (cannot be expressed via the DSL — a duplicate name would be a second
    // node — so mutate the built tree directly)
    let b = plain();
    let p = b.path().join("same.txt");
    fs::remove_file(&p).unwrap();
    fs::create_dir(&p).unwrap();
    fs::write(p.join("child"), b"").unwrap();
    let d = compare_trees(a.path(), b.path(), Compare::all());
    assert!(matches!(d.as_slice(), [TreeDiff::Kind { .. }]), "{d:?}");
}

#[test]
fn compare_flags_gate_their_aspects() {
    let a = TreeSpec::new()
        .file(
            "f",
            FileSpec::content(b"one").mode(0o600).mtime_ns(T2000_NS),
        )
        .build_temp()
        .unwrap();
    let b = TreeSpec::new()
        .file(
            "f",
            FileSpec::content(b"two").mode(0o644).mtime_ns(T2000_NS + 5),
        )
        .build_temp()
        .unwrap();

    // everything on: content + mode + mtime = 3 diffs
    assert_eq!(compare_trees(a.path(), b.path(), Compare::all()).len(), 3);

    // names + kinds + symlink targets only: passes
    assert_trees_equal(a.path(), b.path(), Compare::names_only());

    // each single flag catches exactly its own difference
    let one = |cmp: Compare| {
        let d = compare_trees(a.path(), b.path(), cmp);
        assert_eq!(d.len(), 1, "{d:?}");
    };
    one(Compare {
        content: true,
        mode: false,
        mtime: false,
        xattrs: false,
    });
    one(Compare {
        content: false,
        mode: true,
        mtime: false,
        xattrs: false,
    });
    one(Compare {
        content: false,
        mode: false,
        mtime: true,
        xattrs: false,
    });
    // xattrs alone: nothing to find, trees are "equal" w.r.t. that aspect
    assert_trees_equal(
        a.path(),
        b.path(),
        Compare {
            content: false,
            mode: false,
            mtime: false,
            xattrs: true,
        },
    );
}

#[test]
fn compare_walks_deep_nesting() {
    let mut deepest = TreeSpec::new().file("bottom", FileSpec::content(b"b"));
    for i in 0..200 {
        deepest = TreeSpec::new().dir(format!("level{i:03}"), DirSpec::new(deepest));
    }
    let a = deepest.build_temp().unwrap();
    let b = deepest.build_temp().unwrap();
    assert_trees_equal(a.path(), b.path(), Compare::all());

    // a difference 200 levels down is still found (outermost level is 199)
    let mut p = b.path().to_path_buf();
    for i in (0..200).rev() {
        p.push(format!("level{i:03}"));
    }
    p.push("bottom");
    fs::write(&p, b"B").unwrap();
    // the rewrite also bumps mtime — check content only
    let d = compare_trees(
        a.path(),
        b.path(),
        Compare {
            content: true,
            ..Compare::names_only()
        },
    );
    assert!(matches!(d.as_slice(), [TreeDiff::Content { .. }]), "{d:?}");
}

#[test]
fn compare_reports_unreadable_dir_as_io_diff() {
    let mk = || {
        TreeSpec::new()
            .dir(
                "secret",
                DirSpec::new(TreeSpec::new().file("inner", FileSpec::content(b"x"))).mode(0o000),
            )
            .build_temp()
            .unwrap()
    };
    let a = mk();
    // root sees through 0o000; detect and skip instead of failing
    if fs::read_dir(a.path().join("secret")).is_ok() {
        eprintln!("SKIP: running as root, the 0o000 dir is still readable");
        return;
    }
    let b = mk();
    let d = compare_trees(a.path(), b.path(), Compare::all());
    // the unreadable directory becomes an Io diff, never a panic
    assert!(
        !d.is_empty() && d.iter().all(|x| matches!(x, TreeDiff::Io { .. })),
        "expected only Io diffs, got {d:?}"
    );
}

#[test]
fn tempdir_cleans_up() {
    let path;
    {
        let dir = TempDir::with_name("ofm-selftest").unwrap();
        fs::write(dir.path().join("f"), b"x").unwrap();
        path = dir.path().to_path_buf();
    }
    assert!(!path.exists());
}

#[test]
fn tempdirs_are_distinct() {
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    assert_ne!(a.path(), b.path());
    assert_ne!(st_dev(a.path()).unwrap(), 0);
}

#[test]
fn two_filesystems_pair_has_different_devs() {
    match two_filesystems() {
        Some(pair) => {
            assert_ne!(pair.first_dev, pair.second_dev);
            assert_ne!(
                st_dev(pair.first.path()).unwrap(),
                st_dev(pair.second.path()).unwrap()
            );
        }
        None => eprintln!("SKIP: only one filesystem available here"),
    }
}

#[test]
fn zeros_really_write_and_sparse_really_holes() {
    let dir = TempDir::new().unwrap();
    TreeSpec::new()
        .file("z", FileSpec::size(32 << 20))
        .file("s", FileSpec::sparse(32 << 20))
        .build(dir.path())
        .unwrap();
    let mz = fs::metadata(dir.path().join("z")).unwrap();
    let ms = fs::metadata(dir.path().join("s")).unwrap();
    assert_eq!(mz.len(), ms.len());
    if ms.blocks() * 512 >= ms.len() {
        eprintln!("note: fs without sparse support; skipping block comparison");
    } else {
        assert!(
            mz.blocks() > ms.blocks() * 4,
            "zeros={} blocks, sparse={} blocks",
            mz.blocks(),
            ms.blocks()
        );
    }
}
