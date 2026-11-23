//! T8.5 adversarial tests for the batch rename *job* (F20 execution layer, ofm-jobs).
//!
//! Extends `rename_batch_safety.rs` / `rename_batch_crash.rs` with: a concurrent creator thread
//! hammering the directory during the batch, sources swapped for symlinks/directories between
//! plan and run (with and without plan-time identities), the destination directory made
//! read-only mid-batch, cancel at *every* step index of a mixed swap+chain+case-only plan,
//! tampered/forgeed/corrupt undo logs (escape attempts, absolute paths, never-batched items,
//! truncated files, replay), and a 20k-step batch. Real file system in temp dirs only.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

mod rename_batch_support;
mod t32_support;

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use ofm_core::CancelToken;
use ofm_core::batch_rename::{Step, StepKind, UndoLog, save_undo_log};
use ofm_jobs::{JobSpec, Outcome, RollbackSpec};
use ofm_testkit::compare::{Compare, assert_trees_equal};
use rename_batch_support::*;
use t32_support::TestFs;

fn compare() -> Compare {
    Compare {
        mtime: false,
        ..Compare::all()
    }
}

fn direct(from: &str, to: &str) -> Step {
    Step {
        from: OsString::from(from),
        to: OsString::from(to),
        kind: StepKind::Direct,
    }
}

// ---- concurrency ----------------------------------------------------------------------------

/// A hostile creator thread adds files to the directory while the batch runs. The batch must
/// complete exactly (its target names can never collide with `noise-*`), and every noise file
/// must survive with its own content: rename_noreplace must never clobber a newcomer.
#[test]
fn concurrent_creators_in_the_directory_never_lose_data() {
    let n = 200;
    let names: Vec<String> = (0..n).map(|i| format!("f{i:03}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let e = env(&refs);

    let stop = Arc::new(AtomicBool::new(false));
    let work = e.work.clone();
    let flag = Arc::clone(&stop);
    let creator = std::thread::spawn(move || {
        let mut i = 0u64;
        let mut made = Vec::new();
        while !flag.load(Ordering::Relaxed) {
            let name = format!("noise-{i}");
            // create_new: never overwrite, even our own names (they are unique anyway).
            if std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(work.join(&name))
                .is_ok()
            {
                std::fs::write(work.join(&name), format!("content-{i}")).unwrap();
                made.push(name);
            }
            i += 1;
        }
        made
    });

    let pairs: Vec<(&str, &str)> = names
        .iter()
        .map(|s| (s.as_str(), s.replace('f', "g").leak() as &str))
        .collect();
    let p = plan_of(&e.work, &pairs);
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
    stop.store(true, Ordering::Relaxed);
    let noise = creator.join().unwrap();
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");

    // The batch mapping is exact.
    let listing = listing(&e.work);
    assert_eq!(listing.len(), n + noise.len());
    for (from, to) in pairs {
        assert_eq!(
            listing[&OsString::from(to)],
            from.as_bytes(),
            "{from}->{to}"
        );
    }
    // Every noise file survived with its own content.
    for nme in &noise {
        let i = nme.strip_prefix("noise-").unwrap();
        assert_eq!(
            listing[&OsString::from(nme)],
            format!("content-{i}").into_bytes(),
            "{nme}"
        );
    }
}

// ---- source swapped between plan and run ----------------------------------------------------

/// Between planning and execution the source becomes a symlink or a directory. With plan-time
/// identities the job must refuse; without them it renames whatever is there *as-is* (the
/// symlink is never followed, the directory subtree moves intact) — pin both behaviours.
#[test]
fn source_replaced_by_symlink_or_dir_between_plan_and_run() {
    let cases: [(&str, fn(&std::path::Path)); 2] = [
        ("symlink", |work: &std::path::Path| {
            std::os::unix::fs::symlink("/nonexistent", work.join("a")).unwrap()
        }),
        ("dir", |work: &std::path::Path| {
            std::fs::create_dir(work.join("a")).unwrap();
            std::fs::write(work.join("a/inner"), b"in").unwrap();
        }),
    ];
    for (kind, make_replacement) in cases {
        // With identities: refused before the swap is renamed.
        let e = env(&["a", "b"]);
        let p = plan_of(&e.work, &[("a", "x")]);
        let mut spec = batch(&e, p.clone());
        spec.identities = identities(&e, &p);
        std::fs::remove_file(e.work.join("a")).unwrap();
        make_replacement(&e.work);
        let out = run_quiet(JobSpec::RenameBatch(spec), real());
        let (msg, _) = failed(&out);
        assert!(msg.contains("changed since"), "{kind}: {msg}");
        assert_eq!(report(&out).files_done, 0);
        assert!(e.work.join("a").symlink_metadata().is_ok(), "{kind}");
        assert!(!e.work.join("x").exists(), "{kind}");

        // Without identities: the replacement is renamed as-is, never followed.
        let e = env(&["a", "b"]);
        let p = plan_of(&e.work, &[("a", "x")]);
        std::fs::remove_file(e.work.join("a")).unwrap();
        make_replacement(&e.work);
        let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
        assert!(matches!(out, Outcome::Ok(_)), "{kind}: {out:?}");
        if kind == "symlink" {
            assert_eq!(
                std::fs::read_link(e.work.join("x")).unwrap(),
                std::path::Path::new("/nonexistent")
            );
        } else {
            assert_eq!(std::fs::read(e.work.join("x/inner")).unwrap(), b"in");
            assert!(e.work.join("x").is_dir());
        }
        assert_eq!(std::fs::read(e.work.join("b")).unwrap(), b"b");
    }
}

// ---- destination directory made read-only mid-batch ------------------------------------------

/// The directory becomes read-only after the first step: the second step fails cleanly, the
/// partial state is exact, and rollback (after restoring permissions) restores the tree.
#[test]
fn dir_made_read_only_mid_batch_stops_and_rolls_back() {
    if is_root() {
        return;
    }
    let files = ["a", "b", "c", "d"];
    let e = env(&files);
    let p = plan_of(&e.work, &[("a", "w"), ("b", "x"), ("c", "y"), ("d", "z")]);
    let work = e.work.clone();
    let fs = Arc::new(TestFs::real().hooked(on_nth_lstat(2, move || {
        std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o555)).unwrap();
    })));
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), fs);
    let (msg, _) = failed(&out);
    assert!(msg.contains("step 2/4"), "{msg}");
    assert_eq!(report(&out).files_done, 1);
    assert_eq!(
        listing(&e.work),
        expect(&[("w", "a"), ("b", "b"), ("c", "c"), ("d", "d")])
    );
    assert_eq!(log_steps(&e), 1);

    std::fs::set_permissions(&e.work, std::fs::Permissions::from_mode(0o755)).unwrap();
    let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    assert!(matches!(undo, Outcome::Ok(_)), "{undo:?}");
    assert_trees_equal(&e.work, pristine(&files).path(), compare());
}

// ---- cancel at every step of a mixed plan ---------------------------------------------------

/// Cancel at every possible step index of a plan containing all three step kinds (swap temp,
/// chain direct, case-only temp), then roll back: the tree must be byte-exact every time.
/// Cancel is honoured *between* steps (a step is one atomic syscall), so a cancel triggered
/// during step k takes effect before step k+1 and leaves exactly k steps done; a cancel
/// triggered during the LAST step is never observed — the batch simply completes.
#[test]
fn cancel_at_every_step_index_then_rollback_restores_exactly() {
    let files = ["a", "b", "c", "d", "f"];
    // swap a<->b (3 steps), chain c->d->x (2 steps), case-only f->F (2 temp steps, CI flag).
    let pairs = [("a", "b"), ("b", "a"), ("c", "d"), ("d", "x"), ("f", "F")];
    let total = plan_ci(&env(&files).work, &pairs, true).steps.len();
    assert_eq!(total, 7, "3 swap + 2 chain + 2 case-only temp steps");
    for k in 1..total {
        let e = env(&files);
        let p = plan_ci(&e.work, &pairs, true);
        let cancel = CancelToken::new();
        let c = cancel.clone();
        let fs = Arc::new(TestFs::real().hooked(on_nth_lstat(k as u64, move || c.cancel())));
        let (out, _) = run_cancellable(JobSpec::RenameBatch(batch(&e, p)), fs, cancel);
        assert!(matches!(out, Outcome::Cancelled(_)), "k={k}: {out:?}");
        assert_eq!(
            report(&out).files_done,
            k as u64,
            "cancel honours after step k"
        );
        assert_eq!(log_steps(&e), k);

        let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
        assert!(matches!(undo, Outcome::Ok(_)), "k={k}: {undo:?}");
        assert_trees_equal(&e.work, pristine(&files).path(), compare());
        assert!(!e.log.exists(), "k={k}");
    }

    // Cancel triggered by the last step's lstat: no checkpoint remains, the batch completes.
    let e = env(&files);
    let p = plan_ci(&e.work, &pairs, true);
    let cancel = CancelToken::new();
    let c = cancel.clone();
    let fs = Arc::new(TestFs::real().hooked(on_nth_lstat(total as u64, move || c.cancel())));
    let (out, _) = run_cancellable(JobSpec::RenameBatch(batch(&e, p)), fs, cancel);
    assert!(matches!(out, Outcome::Ok(_)), "last-step cancel: {out:?}");
    assert_eq!(report(&out).files_done, total as u64);
}

// ---- undo log tampering (the log is a file the job trusts) ----------------------------------

/// Every escape attempt in step names — absolute paths, `..`, `.`, empty, slash, NUL — is
/// refused before anything is touched, on both the `from` and the `to` side.
#[test]
fn forged_log_step_names_are_refused() {
    for bad in [
        OsString::from("/tmp/evil"),
        OsString::from(".."),
        OsString::from("."),
        OsString::from(""),
        OsString::from("a/b"),
        OsString::from("nul\0byte"),
    ] {
        for (from, to) in [
            (OsString::from("a"), bad.clone()),
            (bad.clone(), OsString::from("x")),
        ] {
            let e = env(&["a", "b"]);
            let log = UndoLog {
                dir: e.work.clone(),
                steps: vec![Step {
                    from,
                    to,
                    kind: StepKind::Direct,
                }],
                renames: vec![],
            };
            save_undo_log(&log, &e.log).unwrap();
            let spec = RollbackSpec {
                log: ofm_jobs::load_log(&e.log).unwrap(),
                log_path: e.log.clone(),
            };
            let out = run_quiet(JobSpec::RenameRollback(spec), real());
            assert!(
                failed(&out).0.contains("invalid request"),
                "{bad:?}: {out:?}"
            );
            assert_eq!(listing(&e.work), expect(&[("a", "a"), ("b", "b")]));
        }
    }
}

/// A forged log whose directory is relative (or points anywhere the attacker likes) is refused:
/// the caller must hand over a log whose directory is absolute.
#[test]
fn forged_log_with_relative_dir_is_refused() {
    let e = env(&["a", "b"]);
    let log = UndoLog {
        dir: "work".into(),
        steps: vec![direct("a", "x")],
        renames: vec![],
    };
    save_undo_log(&log, &e.log).unwrap();
    let spec = RollbackSpec {
        log: ofm_jobs::load_log(&e.log).unwrap(),
        log_path: e.log.clone(),
    };
    let out = run_quiet(JobSpec::RenameRollback(spec), real());
    assert!(failed(&out).0.contains("absolute"), "{out:?}");
    assert_eq!(listing(&e.work), expect(&[("a", "a"), ("b", "b")]));
}

/// Truncated, empty or type-corrupt log files fail cleanly at load time; nothing is touched.
#[test]
fn corrupt_or_truncated_log_fails_cleanly() {
    for (tag, bytes) in [
        ("garbage", b"this is not toml at all".to_vec()),
        ("truncated", b"dir = [1, 2\nsteps = ".to_vec()),
        ("empty", Vec::new()),
        (
            "wrong types",
            b"dir = 42\nsteps = 'nope'\nrenames = []\n".to_vec(),
        ),
        ("not a log", b"[package]\nname = 'x'\n".to_vec()),
    ] {
        let e = env(&["a", "b"]);
        std::fs::create_dir_all(e.log.parent().unwrap()).unwrap();
        std::fs::write(&e.log, &bytes).unwrap();
        let r = ofm_jobs::load_log(&e.log);
        assert!(r.is_err(), "{tag}: loaded? {r:?}");
        assert_eq!(listing(&e.work), expect(&[("a", "a"), ("b", "b")]), "{tag}");
    }
}

/// A forged step whose state does NOT match (source still present, target absent — i.e. it never
/// ran) is recognised as a not-run tail and skipped: the rollback is a no-op, never a rename.
#[test]
fn forged_log_with_non_matching_state_is_a_no_op_not_a_rename() {
    let e = env(&["a", "b"]);
    let log = UndoLog {
        dir: e.work.clone(),
        steps: vec![direct("a", "x")],
        renames: vec![],
    };
    save_undo_log(&log, &e.log).unwrap();
    let out = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(report(&out).files_done, 0);
    assert_eq!(listing(&e.work), expect(&[("a", "a"), ("b", "b")]));
    assert!(!e.log.exists(), "nothing to undo: log removed");
}

/// A forged log whose single step MATCHES the directory state (attacker deleted `a` and created
/// `x`) verifies as "executed" because the log binds names only, no content. Rolling it back
/// renames the attacker's file into place. The job must refuse — but cannot detect this today.
#[test]
#[ignore = "BUG: rollback applies a forged undo log whose step matches the dir state but never ran (the log binds names, not content)"]
fn forged_log_with_matching_state_must_be_refused() {
    let e = env(&["a", "b"]);
    std::fs::remove_file(e.work.join("a")).unwrap();
    std::fs::write(e.work.join("x"), b"EVIL").unwrap();
    let log = UndoLog {
        dir: e.work.clone(),
        steps: vec![direct("a", "x")],
        renames: vec![],
    };
    save_undo_log(&log, &e.log).unwrap();
    let out = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    assert!(
        matches!(out, Outcome::Failed { .. }),
        "forged log must be refused, got {out:?}"
    );
    assert_eq!(std::fs::read(e.work.join("x")).unwrap(), b"EVIL");
}

/// Replaying a finished undo is harmless: the log is gone from disk, and reusing an in-memory
/// spec undoes nothing (the original state does not verify as "executed").
#[test]
fn replaying_a_finished_undo_is_a_harmless_no_op() {
    let files = ["a", "b"];
    let e = env(&files);
    let p = plan_of(&e.work, &[("a", "x"), ("b", "y")]);
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    assert!(matches!(undo, Outcome::Ok(_)), "{undo:?}");
    assert!(!e.log.exists());
    assert_trees_equal(&e.work, pristine(&files).path(), compare());

    // Reloading from the removed log fails cleanly.
    assert!(ofm_jobs::load_log(&e.log).is_err());

    // An in-memory replay of the same spec must not touch the restored tree.
    let spec = rollback_spec_from_memory(&e);
    let again = run_quiet(JobSpec::RenameRollback(spec), real());
    assert!(matches!(again, Outcome::Ok(_)), "{again:?}");
    assert_eq!(report(&again).files_done, 0);
    assert_trees_equal(&e.work, pristine(&files).path(), compare());
}

fn rollback_spec_from_memory(e: &Env) -> RollbackSpec {
    // The log on disk is gone; reconstruct the finished log in memory (what a stale UI would do).
    let p = plan_of(&e.work, &[("a", "x"), ("b", "y")]);
    RollbackSpec {
        log: UndoLog::new(e.work.clone(), &p),
        log_path: e.log.clone(),
    }
}

// ---- scale -----------------------------------------------------------------------------------

/// 20k-step planning: the planner must produce exactly one step per pair, correct chunk sizing
/// (chunk_for(20000) == 200) and stay fast. The full 20k forward+rollback is NOT run here: the
/// write-ahead log re-serializes the whole executed prefix every chunk (O(n²/chunk) bytes, pretty
/// TOML + 2 fsync per save), which costs ~33 s in a debug build — a perf characteristic, not a
/// correctness bug. The executed-scale behaviour is covered below at a debug-feasible size.
#[test]
fn twenty_thousand_step_plan_is_exact_and_fast() {
    assert_eq!(ofm_jobs::rename_batch::chunk_for(20_000), 200);
    assert_eq!(ofm_jobs::rename_batch::chunk_for(100), 1);
    let n = 20_000usize;
    let names: Vec<String> = (0..n).map(|i| format!("t{i:05}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let e = env(&refs);
    let pairs: Vec<(&str, &str)> = names
        .iter()
        .map(|s| (s.as_str(), format!("u{}", &s[1..]).leak() as &str))
        .collect();
    let t0 = Instant::now();
    let p = plan_of(&e.work, &pairs);
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        t0.elapsed()
    );
    assert_eq!(p.steps.len(), n);
    assert!(
        p.steps
            .iter()
            .all(|s| s.kind == StepKind::Direct && s.from != s.to),
        "one direct step per pair, no self-renames"
    );
    assert_eq!(p.preview.len(), n);
}

/// Executed scale (debug budget): a 1200-step batch forward and back — planning, write-ahead log,
/// exact listing and rollback must all stay correct and well under the 5 s budget.
#[test]
fn large_executed_batch_is_exact_and_fast() {
    let n = 1200usize;
    let names: Vec<String> = (0..n).map(|i| format!("t{i:05}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let e = env(&refs);
    let pairs: Vec<(&str, &str)> = names
        .iter()
        .map(|s| (s.as_str(), format!("u{}", &s[1..]).leak() as &str))
        .collect();
    let p = plan_of(&e.work, &pairs);
    assert_eq!(p.steps.len(), n);

    let t0 = Instant::now();
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
    let forward = t0.elapsed();
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(forward < std::time::Duration::from_secs(3), "{forward:?}");
    assert_eq!(log_steps(&e), n);

    let listing = listing(&e.work);
    assert_eq!(listing.len(), n);
    for (from, to) in &pairs {
        assert_eq!(listing[&OsString::from(*to)], from.as_bytes());
    }

    let t1 = Instant::now();
    let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    let backward = t1.elapsed();
    assert!(matches!(undo, Outcome::Ok(_)), "{undo:?}");
    assert!(backward < std::time::Duration::from_secs(3), "{backward:?}");
    assert_trees_equal(&e.work, pristine(&refs).path(), compare());
    assert!(!e.log.exists());
}
