//! T8.3c: batch rename (F20) driven through `AppContext::execute` in-process (needs a display;
//! skips without one). The "editor" is a shell script that applies a sed program (rewritten by the
//! test between steps) to the temp file it is given, so the whole flow runs for real: temp file
//! on the pool -> detached editor -> Apply -> plan -> preview -> job -> undo.
//! One `#[test]` owns the GTK thread. `OFM_SHOT_DIR=dir` writes screenshots.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod fileops_support;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use fileops_support::*;
use gtk::prelude::*;
use ofm_app::batchrename::{self, BatchState, Phase};
use ofm_core::cmd;
use ofm_jobs::JobState;
use ofm_testkit::temp::TempDir;
use ofm_testkit::tree::{FileSpec, TreeSpec, raw_name};

struct Editor {
    _dir: TempDir,
    prog: PathBuf,
    runs: PathBuf,
    path_log: PathBuf,
}

impl Editor {
    /// A script `sed -i -f prog FILE`; every run is recorded in `runs` (after the edit) and the
    /// file path it was given in `path_log`.
    fn new() -> Editor {
        let dir = TempDir::new().unwrap();
        let (prog, runs, path_log) = (
            dir.path().join("prog.sed"),
            dir.path().join("runs"),
            dir.path().join("paths"),
        );
        let script = dir.path().join("editor.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{}'\nsed -i -f '{}' \"$1\"\necho done >> '{}'\n",
                path_log.display(),
                prog.display(),
                runs.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(&prog, "").unwrap();
        Editor {
            _dir: dir,
            prog,
            runs,
            path_log,
        }
    }

    fn command(&self) -> String {
        format!("{} %f", self._dir.path().join("editor.sh").display())
    }

    fn set_program(&self, sed: &str) {
        std::fs::write(&self.prog, sed).unwrap();
    }

    fn runs(&self) -> usize {
        std::fs::read_to_string(&self.runs).map_or(0, |s| s.lines().count())
    }

    fn last_file(&self) -> PathBuf {
        PathBuf::from(
            std::fs::read_to_string(&self.path_log)
                .unwrap()
                .lines()
                .last()
                .unwrap(),
        )
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn content(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap()
}

fn phase(fx: &Fixture) -> Phase {
    batchrename::state(&fx.ctx).unwrap().phase()
}

fn select_names(fx: &Fixture, wanted: &[&str]) {
    fx.ctx.active_controller().mutate(|m| {
        m.select_all(false);
        for i in 0..m.visible_len() {
            let hit = m
                .visible(i)
                .is_some_and(|e| wanted.iter().any(|w| e.name == *w));
            if hit {
                m.toggle_select(i);
            }
        }
    });
}

fn jobs_idle(fx: &Fixture) -> bool {
    let c = ofm_app::jobs::controller(&fx.ctx).unwrap();
    let jobs = c.jobs();
    !jobs.is_empty() && jobs.iter().all(|j| j.state == JobState::Finished)
}

fn cursor_name(fx: &Fixture) -> String {
    fx.ctx
        .active_controller()
        .model()
        .cursor_entry()
        .map(|e| e.name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn preview_text(fx: &Fixture) -> String {
    let view: gtk::TextView = fx.find("ofm-batchrename-preview").downcast().unwrap();
    let b = view.buffer();
    b.text(&b.start_iter(), &b.end_iter(), false).to_string()
}

#[test]
fn batch_rename_flow_swap_collision_and_undo() {
    if !init() {
        return;
    }
    let bad = raw_name(b"bad\xff.bin");
    let left = TreeSpec::new()
        .file("a.txt", FileSpec::content(b"A"))
        .file("b.txt", FileSpec::content(b"B"))
        .file("c.txt", FileSpec::content(b"C"))
        .file("z.txt", FileSpec::content(b"Z"))
        .file(bad.clone(), FileSpec::content(b"BAD"));
    let fx = Fixture::new(
        "io.github.mscoder.OfmBatchRenameTest",
        left,
        TreeSpec::new().file("x", FileSpec::content(b"x")),
        theme_from_env().as_deref(),
    );
    let st: std::rc::Rc<BatchState> = batchrename::state(&fx.ctx).unwrap();
    let ed = Editor::new();
    fx.ctx.config.borrow_mut().editor.command = ed.command();
    let dir = fx.left.path().to_owned();
    let original = names(&dir);

    // -- Nothing selected, cursor on `..`: refused with a message, nothing starts. -------------
    fx.run(cmd::FILE_BATCH_RENAME);
    assert!(st.last_notice().unwrap().contains("Select the files"));
    assert!(!st.active());
    fx.run(cmd::DIALOG_OK); // close the message
    pump_until("message closed", || fx.modal_depth() == 0);

    // -- A swap a <-> b (plus an untouched c) ---------------------------------------------------
    // The non-UTF-8 name is selected too: it is left out of the editor text and not renamed.
    select_names(&fx, &["a.txt", "b.txt", "c.txt"]);
    fx.ctx.active_controller().mutate(|m| {
        // `visible` names are bytes: select the non-UTF-8 entry by its real name.
        for i in 0..m.visible_len() {
            if m.visible(i).is_some_and(|e| e.name == bad) {
                m.toggle_select(i);
            }
        }
    });
    ed.set_program("s/^a\\.txt$/TMP/; s/^b\\.txt$/a.txt/; s/^TMP$/b.txt/\n");
    fx.run(cmd::FILE_BATCH_RENAME);
    pump_until("waiting for the editor", || phase(&fx) == Phase::Waiting);
    pump_until("editor ran", || ed.runs() == 1);
    // The editor text file: the unsupported name is not in it; the file is private.
    let tmp_file = ed.last_file();
    assert!(tmp_file.starts_with(batchrename::workspace::base_dir()));
    assert_eq!(
        std::fs::metadata(&tmp_file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(tmp_file.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::read_to_string(&tmp_file).unwrap(),
        "b.txt\na.txt\nc.txt\n"
    );
    fx.shot("batchrename-waiting");
    fx.run(cmd::DIALOG_OK); // Apply
    pump_until("preview", || matches!(phase(&fx), Phase::Preview(_)));
    let Phase::Preview(lines) = phase(&fx) else {
        unreachable!()
    };
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("a.txt") && l.ends_with("b.txt"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("b.txt") && l.ends_with("a.txt"))
    );
    let shown = preview_text(&fx);
    assert!(shown.contains("a.txt"), "{shown}");
    assert!(
        !shown.contains("TMP") && !shown.contains(".ofm-rename-"),
        "temp names must be hidden: {shown}"
    );
    fx.shot("batchrename-preview");
    // The temp directory is still there while the preview is shown, and removed afterwards.
    assert!(tmp_file.exists());
    fx.run(cmd::DIALOG_OK); // Enter = Apply
    pump_until("batch job finished", || jobs_idle(&fx));
    assert_eq!(content(&dir, "a.txt"), "B");
    assert_eq!(content(&dir, "b.txt"), "A");
    assert_eq!(content(&dir, "c.txt"), "C");
    assert_eq!(std::fs::read(dir.join(&bad)).unwrap(), b"BAD");
    assert_eq!(
        names(&dir).len(),
        original.len(),
        "no temp name left behind"
    );
    pump_until("temp directory removed", || !tmp_file.exists());
    assert!(!st.active());
    // The cursor moved from `..` onto a renamed item once the watcher refreshed the panel.
    pump_until("cursor on a renamed item", || {
        matches!(cursor_name(&fx).as_str(), "a.txt" | "b.txt")
    });
    // The undo information is kept under the state directory.
    let state_dir = fx.ctx.paths.state_dir.clone();
    let last = state_dir.join("batchrename/last.toml");
    pump_until("undo log kept", || last.exists());
    assert!(
        std::fs::read_dir(state_dir.join("batchrename"))
            .unwrap()
            .all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("pending-")),
        "pending log must have been promoted"
    );

    // -- A collision with a file that is not selected: a problem dialog, re-open fixes it. ------
    select_names(&fx, &["a.txt", "b.txt"]);
    ed.set_program("s/^a\\.txt$/z.txt/\n");
    fx.run(cmd::FILE_BATCH_RENAME);
    pump_until("waiting (2)", || phase(&fx) == Phase::Waiting);
    pump_until("editor ran (2)", || ed.runs() == 2);
    fx.run(cmd::DIALOG_OK);
    pump_until("problem", || matches!(phase(&fx), Phase::Problem(_)));
    let Phase::Problem(why) = phase(&fx) else {
        unreachable!()
    };
    assert!(
        why.contains("'z.txt'") && why.contains("already exists"),
        "{why}"
    );
    assert_eq!(
        names(&dir),
        original,
        "a refused batch must not touch anything"
    );
    fx.shot("batchrename-problem");
    // Re-open (Enter): the file still holds the edit; this time the editor turns z.txt into y.txt.
    ed.set_program("s/^z\\.txt$/y.txt/\n");
    fx.run(cmd::DIALOG_OK);
    pump_until("waiting (3)", || phase(&fx) == Phase::Waiting);
    pump_until("editor ran (3)", || ed.runs() == 3);
    fx.run(cmd::DIALOG_OK);
    pump_until("preview (2)", || matches!(phase(&fx), Phase::Preview(_)));
    let Phase::Preview(lines) = phase(&fx) else {
        unreachable!()
    };
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].ends_with("y.txt"), "{lines:?}");
    // The preview announces that applying replaces the previous undo information.
    let note: gtk::Label = fx.find("ofm-batchrename-note").downcast().unwrap();
    assert!(note.text().contains("replaces the undo"), "{}", note.text());
    // Esc cancels: nothing renamed, temp directory gone, next batch allowed.
    let tmp2 = ed.last_file();
    fx.run(cmd::DIALOG_CANCEL);
    pump_until("cancelled", || !st.active());
    pump_until("temp directory 2 removed", || !tmp2.exists());
    assert_eq!(names(&dir), original);
    assert!(last.exists(), "cancel must keep the previous undo log");

    // -- Waiting dialog: Esc cancels as well. --------------------------------------------------
    ed.set_program("");
    fx.run(cmd::FILE_BATCH_RENAME);
    pump_until("waiting (4)", || phase(&fx) == Phase::Waiting);
    fx.run(cmd::DIALOG_CANCEL);
    pump_until("cancelled (2)", || !st.active());
    let tmp3 = ed.last_file();
    pump_until("temp directory 3 removed", || !tmp3.exists());
    assert_eq!(names(&dir), original);

    // -- Undo of the swap: stale after the user changes something, fine otherwise. -------------
    std::fs::rename(dir.join("c.txt"), dir.join("c-moved.txt")).unwrap();
    // c.txt was not part of the swap (no change line), so the undo is still valid.
    fx.run(cmd::FILE_BATCH_RENAME_UNDO);
    pump_until("undo preview", || matches!(phase(&fx), Phase::Preview(_)));
    let Phase::Preview(lines) = phase(&fx) else {
        unreachable!()
    };
    assert_eq!(lines.len(), 2, "{lines:?}");
    fx.shot("batchrename-undo-preview");
    // A stale log is refused: move a.txt away first, then try undo again.
    fx.run(cmd::DIALOG_CANCEL);
    pump_until("undo cancelled", || phase(&fx) == Phase::Idle);
    std::fs::rename(dir.join("a.txt"), dir.join("a-moved.txt")).unwrap();
    fx.run(cmd::FILE_BATCH_RENAME_UNDO);
    pump_until("stale notice", || {
        st.last_notice().is_some_and(|n| n.contains("no longer"))
    });
    fx.run(cmd::DIALOG_OK);
    pump_until("notice closed", || fx.modal_depth() == 0);
    assert!(dir.join("a-moved.txt").exists() && dir.join("b.txt").exists());
    // Put things back and undo for real.
    std::fs::rename(dir.join("a-moved.txt"), dir.join("a.txt")).unwrap();
    fx.run(cmd::FILE_BATCH_RENAME_UNDO);
    pump_until("undo preview (2)", || {
        matches!(phase(&fx), Phase::Preview(_))
    });
    fx.run(cmd::DIALOG_OK);
    pump_until("undo job finished", || {
        let c = ofm_app::jobs::controller(&fx.ctx).unwrap();
        let jobs = c.jobs();
        jobs.len() >= 2 && jobs.iter().all(|j| j.state == JobState::Finished)
    });
    assert_eq!(content(&dir, "a.txt"), "A");
    assert_eq!(content(&dir, "b.txt"), "B");
    assert_eq!(std::fs::read(dir.join(&bad)).unwrap(), b"BAD");
    pump_until("undo log removed", || !last.exists());
    // Nothing left to undo.
    fx.run(cmd::FILE_BATCH_RENAME_UNDO);
    pump_until("nothing to undo", || {
        st.last_notice()
            .is_some_and(|n| n.contains("no batch rename to undo"))
    });
    fx.run(cmd::DIALOG_OK);
    pump_until("closed", || fx.modal_depth() == 0);
}
