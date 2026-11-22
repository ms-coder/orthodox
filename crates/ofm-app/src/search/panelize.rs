//! Panelize (F25): show search results or the output of a shell command as a panel whose rows are
//! real files (`Location::List`, ADR-0009). Lists are built on worker threads; the main thread
//! only creates the location and navigates the active panel to it, so Backspace / Alt+Left return
//! to where the panel came from (the navigation history).

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, PoisonError, RwLock};

use ofm_core::panelize::{Delimiter, PanelizeEvent, PanelizeSpec, PanelizeSummary, run_panelize};
use ofm_core::{CancelToken, Entry, ListId, Location};
use ofm_search::Hit;
use ofm_vfs::list::PanelList;

use crate::context::AppContext;
use crate::i18n::tr;
use crate::navigation::goto::navigate;
use crate::wiring;

/// What a finished panelize produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Rows in the new list.
    pub rows: usize,
    /// Items that were not real local files and are not in the list (hits inside archives) or no
    /// longer exist (command output lines).
    pub left_out: usize,
}

/// Message for the status line after a panelize of search hits.
pub fn hits_message(o: &Outcome) -> String {
    if o.rows == 0 {
        return tr("Nothing to panelize: no result is a local file.").to_owned();
    }
    if o.left_out == 0 {
        format!("{} {}", o.rows, tr("results shown in the panel."))
    } else {
        format!(
            "{} {} ({} {})",
            o.rows,
            tr("results shown in the panel"),
            o.left_out,
            tr("inside archives left out).")
        )
    }
}

/// Directory a command runs in: the panel's directory (the archive's folder for archives, home
/// for lists).
pub fn command_cwd(loc: &Location) -> PathBuf {
    match loc {
        Location::Local(p) => p.clone(),
        Location::Archive { archive, .. } => archive
            .parent()
            .map_or_else(gtk::glib::home_dir, Path::to_path_buf),
        Location::List { .. } => gtk::glib::home_dir(),
    }
}

/// Every list id some tab shows or can go back/forward to.
fn live_lists(ctx: &AppContext) -> HashSet<ListId> {
    let mut ids = HashSet::new();
    for host in &ctx.panels {
        for tab in host.tabs() {
            let history = tab.history.borrow();
            for loc in history.entries().iter().chain([&tab.location()]) {
                if let Location::List { id } = loc {
                    ids.insert(*id);
                }
            }
        }
    }
    ids
}

/// Forget lists nobody refers to any more.
pub fn collect_garbage(ctx: &AppContext) {
    let keep = live_lists(ctx);
    let dropped = ctx.services.lists.retain(&keep);
    if dropped > 0 {
        tracing::debug!(dropped, "dropped unused panel lists");
    }
}

/// Make the active panel show `list`.
pub fn show_list(ctx: &Rc<AppContext>, list: PanelList) -> Location {
    let loc = ctx.services.lists.create(list);
    navigate(ctx, &ctx.active_controller(), loc.clone(), None);
    collect_garbage(ctx);
    loc
}

// ---- search results ----------------------------------------------------------------------------

/// Put hits into the active panel. `only` selects hit indices (all hits when `None`). Rows are
/// built on a worker; `done` runs on the main thread afterwards with the outcome.
pub fn panelize_hits(
    ctx: &Rc<AppContext>,
    hits: Arc<RwLock<Vec<Hit>>>,
    only: Option<Vec<usize>>,
    title: String,
    base: Option<PathBuf>,
    done: impl FnOnce(Outcome) + 'static,
) {
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let (entries, left_out) = {
            let hits = hits.read().unwrap_or_else(PoisonError::into_inner);
            let mut entries: Vec<Entry> = Vec::new();
            let mut left_out = 0usize;
            let mut take = |h: &Hit| match h.to_list_entry() {
                Some(e) => entries.push(e),
                None => left_out += 1,
            };
            match &only {
                Some(idx) => idx.iter().filter_map(|i| hits.get(*i)).for_each(&mut take),
                None => hits.iter().for_each(&mut take),
            }
            (entries, left_out)
        };
        let list = PanelList::from_entries(title, base, entries);
        let _ = tx.send_blocking((list, left_out));
    });
    let weak = Rc::downgrade(ctx);
    let mut done = Some(done);
    wiring::attach(
        rx,
        move |(list, left_out)| {
            let Some(ctx) = weak.upgrade() else {
                return ControlFlow::Break(());
            };
            let outcome = Outcome {
                rows: list.entries.len(),
                left_out,
            };
            if outcome.rows > 0 {
                show_list(&ctx, list);
            }
            if let Some(done) = done.take() {
                done(outcome);
            }
            ControlFlow::Break(())
        },
        || {},
    );
}

// ---- panelize command --------------------------------------------------------------------------

/// What a command run reports to the UI.
#[derive(Debug)]
pub enum CommandMsg {
    /// Paths read so far.
    Count(usize),
    /// The run ended.
    Done(Result<CommandDone, String>),
}

/// A finished command run.
#[derive(Debug)]
pub struct CommandDone {
    /// The rows that exist.
    pub list: PanelList,
    /// Output lines that name nothing that exists.
    pub missing: usize,
    /// Exit code, stderr, cancel/truncation.
    pub summary: PanelizeSummary,
}

/// Run `command` in `cwd` on a pool worker: stream the paths, `lstat` them, build the list.
/// Messages arrive on the returned channel; `cancel` stops the command (process group killed).
pub fn spawn_command(
    ctx: &AppContext,
    cwd: PathBuf,
    command: String,
    nul: bool,
    title: String,
    cancel: CancelToken,
) -> async_channel::Receiver<CommandMsg> {
    wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let spec = PanelizeSpec {
            cwd: cwd.clone(),
            command,
            shell: None,
            delimiter: if nul {
                Delimiter::Nul
            } else {
                Delimiter::Newline
            },
        };
        let mut paths: Vec<PathBuf> = Vec::new();
        let mut summary = PanelizeSummary::default();
        let run = run_panelize(&spec, &cancel, &mut |ev| match ev {
            PanelizeEvent::Paths(p) => {
                paths.extend(p);
                let _ = tx.send_blocking(CommandMsg::Count(paths.len()));
            }
            PanelizeEvent::Done(s) => summary = s,
        });
        let result = match run {
            Err(e) => Err(e.to_string()),
            Ok(()) => PanelList::from_paths(title, Some(cwd), paths, &cancel)
                .map(|(list, missing)| CommandDone {
                    list,
                    missing,
                    summary,
                })
                .map_err(|e| e.to_string()),
        };
        let _ = tx.send_blocking(CommandMsg::Done(result));
    })
}

/// Status-line text after a command run.
pub fn command_message(rows: usize, missing: usize, summary: &PanelizeSummary) -> String {
    let mut s = format!("{rows} {}", tr("files shown in the panel"));
    if missing > 0 {
        s.push_str(&format!(", {missing} {}", tr("lines ignored (no such file)")));
    }
    if summary.truncated {
        s.push_str(&format!(", {}", tr("output truncated")));
    }
    match summary.exit_code {
        Some(0) | None => {}
        Some(code) => s.push_str(&format!(", {} {code}", tr("command exit code"))),
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_messages() {
        assert!(hits_message(&Outcome { rows: 0, left_out: 3 }).starts_with("Nothing"));
        assert_eq!(
            hits_message(&Outcome { rows: 5, left_out: 0 }),
            "5 results shown in the panel."
        );
        assert!(hits_message(&Outcome { rows: 5, left_out: 2 }).contains("2 inside archives"));
    }

    #[test]
    fn command_messages_mention_ignored_lines_and_exit_codes() {
        let ok = PanelizeSummary::default();
        assert_eq!(command_message(3, 0, &ok), "3 files shown in the panel");
        let bad = PanelizeSummary {
            exit_code: Some(2),
            truncated: true,
            ..PanelizeSummary::default()
        };
        let m = command_message(3, 4, &bad);
        assert!(m.contains("4 lines ignored") && m.contains("truncated") && m.contains("exit code 2"));
    }

    #[test]
    fn archive_command_runs_next_to_the_archive() {
        let loc = Location::Archive {
            archive: "/d/x.zip".into(),
            inner: "a".into(),
        };
        assert_eq!(command_cwd(&loc), Path::new("/d"));
        assert_eq!(command_cwd(&Location::Local("/w".into())), Path::new("/w"));
    }
}
