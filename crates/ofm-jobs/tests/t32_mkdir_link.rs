//! T3.2: mkdir -p and symlink/hardlink creation (F13, F18).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod t32_support;

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::ops::link::relative_target;
use ofm_jobs::{ErrorAction, JobSpec, LinkKind, Outcome, Question, run_sync};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::temp::TempDir;
use ofm_testkit::tree::raw_name;
use t32_support::{TestFs, error_policy, no_questions, questions, report};

fn mkdir(path: &Path) -> JobSpec {
    JobSpec::Mkdir { path: path.into() }
}

fn link(target: &Path, link: &Path, kind: LinkKind) -> JobSpec {
    JobSpec::Link {
        target: target.into(),
        link: link.into(),
        kind,
    }
}

#[test]
fn creates_parents_and_accepts_existing_dir() {
    let t = TempDir::new().unwrap();
    let deep = t.path().join("a/b/c");
    let (sink, rec) = no_questions();
    let out = run_sync(&mkdir(&deep), Arc::new(TestFs::real()), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(deep.is_dir());
    assert_eq!(report(&out).created, vec![deep.clone()]);
    assert_eq!(report(&out).files_done, 3);
    assert!(questions(&rec).is_empty());

    // Again: success, nothing created.
    let (sink, _) = no_questions();
    let out = run_sync(&mkdir(&deep), Arc::new(TestFs::real()), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(report(&out).files_done, 0);

    // Directory mode: 0777 minus umask (never wider than 0777, never setuid).
    let mode = fs::metadata(&deep).unwrap().mode() & 0o7777;
    assert_eq!(mode & !0o777, 0);
}

#[test]
fn symlink_to_a_directory_counts_as_existing_directory() {
    let t = TempDir::new().unwrap();
    fs::create_dir(t.path().join("real")).unwrap();
    std::os::unix::fs::symlink(t.path().join("real"), t.path().join("l")).unwrap();
    let (sink, _) = no_questions();
    let out = run_sync(
        &mkdir(&t.path().join("l/sub")),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(t.path().join("real/sub").is_dir());
}

#[test]
fn existing_file_is_an_error() {
    let t = TempDir::new().unwrap();
    fs::write(t.path().join("f"), b"x").unwrap();

    let (sink, rec) = no_questions();
    let out = run_sync(&mkdir(&t.path().join("f")), Arc::new(TestFs::real()), sink);
    match &out {
        Outcome::Failed { error, path, .. } => {
            assert_eq!(path.as_deref(), Some(t.path().join("f").as_path()));
            assert!(error.contains("not a directory"), "{error}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fs::read(t.path().join("f")).unwrap(), b"x");

    // A file in the middle of the path.
    let (sink, _) = no_questions();
    let out = run_sync(
        &mkdir(&t.path().join("f/sub/deeper")),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(
        matches!(&out, Outcome::Failed { path: Some(p), .. } if p == &t.path().join("f/sub/deeper") || p == &t.path().join("f")),
        "{out:?}"
    );
    assert!(
        questions(&rec).is_empty(),
        "unrecoverable errors do not ask"
    );

    // A dangling symlink at the path is not a directory either.
    std::os::unix::fs::symlink("/nonexistent/zzz", t.path().join("dangling")).unwrap();
    let (sink, _) = error_policy(ErrorAction::Skip);
    let out = run_sync(
        &mkdir(&t.path().join("dangling")),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(!matches!(out, Outcome::Cancelled(_)));
    assert!(!t.path().join("dangling").is_dir());
}

#[test]
fn relative_path_is_invalid() {
    let (sink, _) = no_questions();
    let out = run_sync(&mkdir(Path::new("rel/dir")), Arc::new(TestFs::real()), sink);
    assert!(
        matches!(&out, Outcome::Failed { error, .. } if error.contains("absolute")),
        "{out:?}"
    );
}

#[test]
fn odd_names_work() {
    let t = TempDir::new().unwrap();
    for name in [
        raw_name(b"bad-\xff"),
        "new\nline".into(),
        "-dash".into(),
        "with space ".into(),
        "back\\slash".into(),
    ] {
        let p = t.path().join("p").join(&name).join("child");
        let (sink, _) = no_questions();
        let out = run_sync(&mkdir(&p), Arc::new(TestFs::real()), sink);
        assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
        assert!(p.is_dir());
    }
}

#[test]
fn mkdir_errors_use_retry_skip_abort() {
    let t = TempDir::new().unwrap();
    let target = t.path().join("a/b");
    let faulty = || {
        Arc::new(FaultyFs::new(
            Arc::new(TestFs::real()),
            FaultPlan::new().fail_nth(FaultOp::Mkdir, 1, FaultErrno::PermissionDenied),
        ))
    };

    let (sink, rec) = error_policy(ErrorAction::Skip);
    let out = run_sync(&mkdir(&target), faulty(), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(!t.path().join("a").exists());
    assert!(report(&out).created.is_empty());
    assert!(matches!(
        questions(&rec).as_slice(),
        [Question::Error {
            os_error: Some(13),
            ..
        }]
    ));

    let (sink, _) = error_policy(ErrorAction::Retry);
    let out = run_sync(&mkdir(&target), faulty(), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(target.is_dir());

    let other = t.path().join("x/y");
    let (sink, _) = error_policy(ErrorAction::Abort);
    let out = run_sync(&mkdir(&other), faulty(), sink);
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert!(!t.path().join("x").exists());
}

// ---- links -----------------------------------------------------------------------------------

#[test]
fn symlink_and_hardlink_creation() {
    let t = TempDir::new().unwrap();
    let target = t.path().join("target.txt");
    fs::write(&target, b"hello").unwrap();

    // Absolute symlink, text stored verbatim.
    let abs = t.path().join("abs");
    let (sink, _) = no_questions();
    let out = run_sync(
        &link(&target, &abs, LinkKind::Symbolic),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(fs::read_link(&abs).unwrap(), target);
    assert_eq!(fs::read(&abs).unwrap(), b"hello");
    assert_eq!(report(&out).created, vec![abs.clone()]);

    // Relative symlink computed by the helper, in another directory.
    fs::create_dir(t.path().join("sub")).unwrap();
    let rel = t.path().join("sub/rel");
    let text = relative_target(&rel, &target);
    assert_eq!(text, PathBuf::from("../target.txt"));
    let (sink, _) = no_questions();
    let out = run_sync(
        &link(&text, &rel, LinkKind::Symbolic),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(fs::read_link(&rel).unwrap(), PathBuf::from("../target.txt"));
    assert_eq!(fs::read(&rel).unwrap(), b"hello");

    // Dangling symlinks are allowed.
    let dangling = t.path().join("dangling");
    let (sink, _) = no_questions();
    let out = run_sync(
        &link(Path::new("does/not/exist"), &dangling, LinkKind::Symbolic),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(fs::symlink_metadata(&dangling).is_ok());

    // Hard link: same inode, link count 2, survives deleting the original name.
    let hard = t.path().join("hard");
    let (sink, _) = no_questions();
    let out = run_sync(
        &link(&target, &hard, LinkKind::Hard),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    let (a, b) = (fs::metadata(&target).unwrap(), fs::metadata(&hard).unwrap());
    assert_eq!((a.ino(), a.dev()), (b.ino(), b.dev()));
    assert_eq!(a.nlink(), 2);
    assert_eq!(report(&out).created, vec![hard]);
}

#[test]
fn hardlink_of_a_symlink_links_the_symlink_itself() {
    let t = TempDir::new().unwrap();
    std::os::unix::fs::symlink("somewhere", t.path().join("sl")).unwrap();
    let (sink, _) = no_questions();
    let out = run_sync(
        &link(&t.path().join("sl"), &t.path().join("hl"), LinkKind::Hard),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        fs::read_link(t.path().join("hl")).unwrap(),
        PathBuf::from("somewhere")
    );
}

#[test]
fn existing_link_path_is_an_error() {
    let t = TempDir::new().unwrap();
    fs::write(t.path().join("t"), b"t").unwrap();
    fs::write(t.path().join("occupied"), b"mine").unwrap();
    std::os::unix::fs::symlink("elsewhere", t.path().join("oldlink")).unwrap();

    for kind in [LinkKind::Symbolic, LinkKind::Hard] {
        for name in ["occupied", "oldlink"] {
            let (sink, rec) = no_questions();
            let out = run_sync(
                &link(&t.path().join("t"), &t.path().join(name), kind),
                Arc::new(TestFs::real()),
                sink,
            );
            match &out {
                Outcome::Failed { path, error, .. } => {
                    assert_eq!(path.as_deref(), Some(t.path().join(name).as_path()));
                    assert!(error.contains(name), "{error}");
                }
                other => panic!("{kind:?} {name}: {other:?}"),
            }
            assert!(questions(&rec).is_empty());
            assert!(report(&out).created.is_empty());
        }
    }
    assert_eq!(fs::read(t.path().join("occupied")).unwrap(), b"mine");
    assert_eq!(
        fs::read_link(t.path().join("oldlink")).unwrap(),
        PathBuf::from("elsewhere")
    );
}

#[test]
fn hardlinks_to_directories_and_bad_input_are_refused() {
    let t = TempDir::new().unwrap();
    fs::create_dir(t.path().join("d")).unwrap();
    let (sink, _) = no_questions();
    let out = run_sync(
        &link(&t.path().join("d"), &t.path().join("dl"), LinkKind::Hard),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(
        matches!(&out, Outcome::Failed { error, path: Some(p), .. }
            if error.contains("directories") && p == &t.path().join("d")),
        "{out:?}"
    );
    assert!(!t.path().join("dl").exists());

    let (sink, _) = no_questions();
    let out = run_sync(
        &link(Path::new(""), &t.path().join("empty"), LinkKind::Symbolic),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Failed { .. }), "{out:?}");
    let (sink, _) = no_questions();
    let out = run_sync(
        &link(
            Path::new("/x"),
            Path::new("relative/link"),
            LinkKind::Symbolic,
        ),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Failed { .. }), "{out:?}");
    let (sink, _) = no_questions();
    let out = run_sync(
        &link(Path::new("rel"), &t.path().join("h"), LinkKind::Hard),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Failed { .. }), "{out:?}");
}

#[test]
fn missing_hardlink_target_and_io_errors_ask() {
    let t = TempDir::new().unwrap();
    // Missing hard link target: lstat error -> question -> skip = nothing created, job ok.
    let (sink, rec) = error_policy(ErrorAction::Skip);
    let out = run_sync(
        &link(&t.path().join("nope"), &t.path().join("h"), LinkKind::Hard),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(questions(&rec).len(), 1);
    assert!(!t.path().join("h").exists());

    // symlink() fails with EACCES once: retry works, abort cancels.
    let faulty = || {
        Arc::new(FaultyFs::new(
            Arc::new(TestFs::real()),
            FaultPlan::new().fail_nth(FaultOp::Symlink, 1, FaultErrno::PermissionDenied),
        ))
    };
    let (sink, _) = error_policy(ErrorAction::Retry);
    let out = run_sync(
        &link(Path::new("x"), &t.path().join("s1"), LinkKind::Symbolic),
        faulty(),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(fs::symlink_metadata(t.path().join("s1")).is_ok());

    let (sink, _) = error_policy(ErrorAction::Abort);
    let out = run_sync(
        &link(Path::new("x"), &t.path().join("s2"), LinkKind::Symbolic),
        faulty(),
        sink,
    );
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert!(fs::symlink_metadata(t.path().join("s2")).is_err());

    let (sink, _) = error_policy(ErrorAction::Skip);
    let out = run_sync(
        &link(Path::new("x"), &t.path().join("s3"), LinkKind::Symbolic),
        faulty(),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(report(&out).created.is_empty());
}

#[test]
fn odd_link_names_and_targets() {
    let t = TempDir::new().unwrap();
    let target = t.path().join(raw_name(b"t\xff\nx"));
    fs::write(&target, b"1").unwrap();
    let l = t.path().join(raw_name(b"-l\xfe"));
    let text = relative_target(&l, &target);
    let (sink, _) = no_questions();
    let out = run_sync(
        &link(&text, &l, LinkKind::Symbolic),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(fs::read(&l).unwrap(), b"1");
}
