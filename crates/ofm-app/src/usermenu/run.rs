//! Running a chosen menu entry: panel snapshot -> placeholder expansion -> `%{input:..}` prompts
//! -> optional confirmation of the final command -> execution (see `exec.rs`).
//!
//! Every step that can fail ends in a message dialog and nothing runs: unsafe names
//! (`ExpandError::UnsafeName`), a missing cursor file, an unknown placeholder, a non-local panel.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::rc::Rc;

use ofm_config::menu::MenuItem;
use ofm_core::Location;
use ofm_core::menu_expand::{self, ExpandCtx, ExpandError, Expanded};

use super::exec::{self, Outcome};
use crate::context::AppContext;
use crate::dialogs::usermenu_input;
use crate::i18n::tr;
use crate::jobs::ui;
use crate::terminal;
use crate::wiring;

/// Owned copy of what [`ExpandCtx`] borrows: taken from the panels on the GTK thread (memory
/// only, no I/O) when the user picks an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Name under the cursor of the active panel (`None`: empty folder or the `..` row).
    pub cursor: Option<OsString>,
    /// Selected names of the active panel (without `..`).
    pub selection: Vec<OsString>,
    /// Active panel's folder (the command's working directory).
    pub dir: PathBuf,
    /// Other panel's folder (empty when that panel is not a local folder: `%D` then expands
    /// to `''`, which makes a copy/sync command fail visibly instead of hitting a wrong place).
    pub other_dir: PathBuf,
    /// Name under the cursor of the other panel.
    pub other_cursor: Option<OsString>,
}

impl Snapshot {
    /// Borrowed view for [`menu_expand::expand`].
    pub fn expand_ctx(&self) -> ExpandCtx<'_> {
        ExpandCtx {
            cursor: self.cursor.as_deref(),
            selection: &self.selection,
            dir: &self.dir,
            other_dir: &self.other_dir,
            other_cursor: self.other_cursor.as_deref(),
        }
    }
}

/// The synthetic parent row is never "the file under the cursor".
fn real_name(name: &OsStr) -> Option<OsString> {
    (name.as_bytes() != b"..").then(|| name.to_os_string())
}

/// Build a [`Snapshot`] from plain panel state. `Err` is the message for the user: commands
/// run in a local folder only.
pub fn snapshot_from(
    active: &Location,
    cursor: Option<&OsStr>,
    selection: &[OsString],
    other: &Location,
    other_cursor: Option<&OsStr>,
) -> Result<Snapshot, String> {
    let Some(dir) = active.local_path() else {
        return Err(tr("User menu commands run in a local folder; this panel shows something else (archive or file list).").to_owned());
    };
    Ok(Snapshot {
        cursor: cursor.and_then(real_name),
        selection: selection
            .iter()
            .filter(|n| n.as_bytes() != b"..")
            .cloned()
            .collect(),
        dir: dir.to_path_buf(),
        other_dir: other.local_path().map(PathBuf::from).unwrap_or_default(),
        other_cursor: other_cursor.and_then(real_name),
    })
}

/// Snapshot of the two panels.
pub fn snapshot(ctx: &AppContext) -> Result<Snapshot, String> {
    let (active, other) = (ctx.active_controller(), ctx.other_controller());
    let (cursor, selection) = {
        let m = active.model();
        (m.cursor_entry().map(|e| e.name.clone()), m.selected_names())
    };
    let other_cursor = other.model().cursor_entry().map(|e| e.name.clone());
    snapshot_from(
        &active.location(),
        cursor.as_deref(),
        &selection,
        &other.location(),
        other_cursor.as_deref(),
    )
}

/// Expand `item`'s command for `snap` (first step; prompts still pending).
pub fn expand(item: &MenuItem, snap: &Snapshot) -> Result<Expanded, ExpandError> {
    menu_expand::expand(&item.command, &snap.expand_ctx(), exec::quote_style())
}

/// Start running `item` for the current panels: expansion, prompts, confirmation, execution.
pub fn start(ctx: &Rc<AppContext>, item: MenuItem) {
    let title = item.title.clone();
    let snap = match snapshot(ctx) {
        Ok(s) => s,
        Err(msg) => return ui::message(ctx, &title, &msg),
    };
    let expanded = match expand(&item, &snap) {
        Ok(e) => e,
        Err(e) => return error(ctx, &title, &e),
    };
    let prompts = expanded.prompts().to_vec();
    let ctx2 = Rc::clone(ctx);
    ask_all(
        ctx,
        title.clone(),
        prompts,
        Vec::new(),
        Box::new(move |answers| {
            // `None`: the user cancelled a prompt (Esc cancels the whole run).
            let Some(answers) = answers else { return };
            let bytes = match expanded.fill_inputs(&answers) {
                Ok(b) => b,
                Err(e) => return error(&ctx2, &title, &e),
            };
            confirm_and_run(&ctx2, item, snap, bytes);
        }),
    );
}

/// Ask the `%{input:..}` prompts one by one; `done(None)` when one is cancelled.
fn ask_all(
    ctx: &Rc<AppContext>,
    title: String,
    mut remaining: Vec<String>,
    mut answers: Vec<String>,
    done: Box<dyn FnOnce(Option<Vec<String>>)>,
) {
    if remaining.is_empty() {
        return done(Some(answers));
    }
    let prompt = remaining.remove(0);
    let ctx2 = Rc::clone(ctx);
    let title2 = title.clone();
    usermenu_input::ask(ctx, &title, &prompt, move |answer| match answer {
        Some(a) => {
            answers.push(a);
            ask_all(&ctx2, title2, remaining, answers, done);
        }
        None => done(None),
    });
}

fn confirm_and_run(ctx: &Rc<AppContext>, item: MenuItem, snap: Snapshot, command: Vec<u8>) {
    if !item.confirm {
        return execute(ctx, &item, snap.dir, command);
    }
    let shown = String::from_utf8_lossy(&command).into_owned();
    let ctx2 = Rc::clone(ctx);
    let title = item.title.clone();
    usermenu_input::confirm(ctx, &title, &shown, move |yes| {
        if yes {
            execute(&ctx2, &item, snap.dir, command);
        }
    });
}

fn error(ctx: &Rc<AppContext>, title: &str, e: &ExpandError) {
    ui::message(ctx, title, &format!("{}: {e}", tr("Cannot run this entry")));
}

/// Run the finished `command` for `item` in `dir`.
pub fn execute(ctx: &Rc<AppContext>, item: &MenuItem, dir: PathBuf, command: Vec<u8>) {
    tracing::debug!(
        title = %item.title,
        terminal = item.terminal,
        command = %String::from_utf8_lossy(&command),
        "user menu: run"
    );
    if item.terminal {
        return run_in_terminal(ctx, &item.title, &dir, &command);
    }
    let rx = exec::spawn_background(command, dir);
    let got = Rc::new(std::cell::Cell::new(false));
    let (ctx2, title, got2) = (Rc::clone(ctx), item.title.clone(), Rc::clone(&got));
    let (ctx3, title3) = (Rc::clone(ctx), item.title.clone());
    wiring::attach(
        rx,
        move |outcome| {
            got2.set(true);
            finished(&ctx2, &title, &outcome);
            std::ops::ControlFlow::Break(())
        },
        move || {
            if !got.get() {
                ui::message(
                    &ctx3,
                    &title3,
                    tr("The command's worker stopped unexpectedly."),
                );
            }
        },
    );
}

fn run_in_terminal(ctx: &Rc<AppContext>, title: &str, dir: &std::path::Path, command: &[u8]) {
    let Some(handle) = terminal::handle(ctx) else {
        return ui::message(ctx, title, tr("The terminal is not available."));
    };
    let weak = Rc::downgrade(ctx);
    let title_owned = title.to_owned();
    let result = handle.run_program(&exec::argv(command), Some(dir), move |msg| {
        if let Some(ctx) = weak.upgrade() {
            ui::message(&ctx, &title_owned, &msg);
        }
    });
    if let Err(msg) = result {
        ui::message(ctx, title, &msg);
    }
}

/// Text of the message for a finished background command (`None`: nothing to say).
pub fn outcome_message(outcome: &Outcome) -> Option<String> {
    match outcome {
        Outcome::Success { stdout } if stdout.is_empty() => None,
        Outcome::Success { stdout } => {
            Some(format!("{}\n\n{stdout}", tr("Finished (exit status 0).")))
        }
        Outcome::Failed {
            code,
            signal,
            stderr,
        } => {
            let head = match (code, signal) {
                (Some(c), _) => format!("{} {c}.", tr("The command failed with exit status")),
                (None, Some(s)) => format!("{} {s}.", tr("The command was terminated by signal")),
                (None, None) => tr("The command failed.").to_owned(),
            };
            Some(if stderr.is_empty() {
                head
            } else {
                format!("{head}\n\n{stderr}")
            })
        }
        Outcome::SpawnFailed(e) => Some(format!("{}: {e}", tr("Cannot start the command"))),
    }
}

fn finished(ctx: &Rc<AppContext>, title: &str, outcome: &Outcome) {
    tracing::debug!(?outcome, "user menu: finished");
    // The directory watcher usually shows the changes; reload anyway (cheap, keeps the cursor).
    for side in [crate::panel::LEFT, crate::panel::RIGHT] {
        ctx.host(side).active().reload();
    }
    if let Some(msg) = outcome_message(outcome) {
        ui::message(ctx, title, &msg);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::Path;

    fn local(p: &str) -> Location {
        Location::Local(PathBuf::from(p))
    }

    fn names(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    fn item(command: &str) -> MenuItem {
        MenuItem {
            title: "t".into(),
            command: command.into(),
            ..MenuItem::default()
        }
    }

    #[test]
    fn snapshot_takes_cursor_selection_and_both_dirs() {
        let s = snapshot_from(
            &local("/a"),
            Some(OsStr::new("c.txt")),
            &names(&["x", "y z"]),
            &local("/b"),
            Some(OsStr::new("o.bin")),
        )
        .unwrap();
        assert_eq!(s.cursor.as_deref(), Some(OsStr::new("c.txt")));
        assert_eq!(s.selection, names(&["x", "y z"]));
        assert_eq!(s.dir, Path::new("/a"));
        assert_eq!(s.other_dir, Path::new("/b"));
        assert_eq!(s.other_cursor.as_deref(), Some(OsStr::new("o.bin")));
        let e = expand(&item("cp %s %D # %f %F %d"), &s).unwrap();
        assert_eq!(
            e.finish().unwrap(),
            b"cp 'x' 'y z' '/b' # 'c.txt' 'o.bin' '/a'".to_vec()
        );
    }

    #[test]
    fn parent_row_is_not_a_file() {
        let s = snapshot_from(
            &local("/a"),
            Some(OsStr::new("..")),
            &names(&["..", "k"]),
            &local("/b"),
            Some(OsStr::new("..")),
        )
        .unwrap();
        assert_eq!(s.cursor, None);
        assert_eq!(s.selection, names(&["k"]));
        assert_eq!(s.other_cursor, None);
        assert!(matches!(
            expand(&item("rm %f"), &s),
            Err(ExpandError::MissingCursor { placeholder: 'f' })
        ));
    }

    #[test]
    fn selection_falls_back_to_the_cursor_file() {
        let s = snapshot_from(
            &local("/a"),
            Some(OsStr::new("only")),
            &[],
            &local("/b"),
            None,
        )
        .unwrap();
        assert_eq!(
            expand(&item("ls %s"), &s).unwrap().finish().unwrap(),
            b"ls 'only'".to_vec()
        );
    }

    #[test]
    fn non_local_panels() {
        let archive = Location::List {
            id: ofm_core::location::ListId(1),
        };
        assert!(snapshot_from(&archive, None, &[], &local("/b"), None).is_err());
        let s = snapshot_from(&local("/a"), Some(OsStr::new("f")), &[], &archive, None).unwrap();
        assert!(s.other_dir.as_os_str().is_empty());
        assert_eq!(
            expand(&item("cp %f %D"), &s).unwrap().finish().unwrap(),
            b"cp 'f' ''".to_vec()
        );
    }

    #[test]
    fn control_byte_names_are_refused_not_run() {
        let s = snapshot_from(
            &local("/a"),
            Some(OsStr::new("bad\u{1b}[31mname")),
            &[],
            &local("/b"),
            None,
        )
        .unwrap();
        let e = expand(&item("cat %f"), &s).unwrap_err();
        assert!(matches!(
            e,
            ExpandError::UnsafeName {
                placeholder: 'f',
                ..
            }
        ));
        assert!(e.to_string().contains("control bytes"));
    }

    #[test]
    fn prompts_are_collected_in_order() {
        let s = snapshot_from(&local("/a"), None, &[], &local("/b"), None).unwrap();
        let e = expand(&item("tar -czf %{input:Name}.tgz %{input:Extra}"), &s).unwrap();
        assert_eq!(e.prompts(), ["Name", "Extra"]);
        assert_eq!(
            e.fill_inputs(&["a b".into(), "c".into()]).unwrap(),
            b"tar -czf a b.tgz c".to_vec()
        );
    }

    #[test]
    fn outcome_messages() {
        assert_eq!(
            outcome_message(&Outcome::Success {
                stdout: String::new()
            }),
            None
        );
        let m = outcome_message(&Outcome::Failed {
            code: Some(2),
            signal: None,
            stderr: "boom".into(),
        })
        .unwrap();
        assert!(m.contains("exit status 2") && m.ends_with("boom"));
        let m = outcome_message(&Outcome::Failed {
            code: None,
            signal: Some(9),
            stderr: String::new(),
        })
        .unwrap();
        assert!(m.contains("signal 9"));
    }
}
