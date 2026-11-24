//! Batch rename via the editor (F20, T8.3c): `file.batch_rename` (Ctrl+Shift+M) and
//! `file.batch_rename_undo` (Ctrl+Alt+Z).
//!
//! Flow (every file access runs on the I/O pool; the GTK thread only builds dialogs):
//! 1. [`flow::start`]: the selection (or the item under the cursor) of the active local panel
//!    becomes editor text; names that cannot round trip through a text editor are left out.
//! 2. The text goes into a private 0700 temp directory ([`workspace`]) and opens in the configured
//!    editor (`desktop::editor::choose`). A terminal editor runs in the embedded terminal and the
//!    flow continues when it exits; a GUI editor is detached, so a "Waiting for your editor"
//!    dialog offers Apply (read the file now) / Cancel.
//! 3. The edited text is parsed and planned against the directory (`ofm_core::batch_rename`).
//!    Problems (line count, duplicates, collisions, bad names) open a dialog with "Re-open
//!    editor" / "Cancel"; the edited file is kept for the re-open.
//! 4. A preview lists `old -> new` (temp-name steps are only mentioned); Enter applies, Esc
//!    cancels. Apply submits `JobSpec::RenameBatch`; its undo log is written under the XDG state
//!    directory (`batchrename/`), where the log of the last finished batch is kept as `last.toml`.
//! 5. A failed or cancelled batch offers "Roll back what was done" (`JobSpec::RenameRollback`).
//!    [`undo::start`] verifies `last.toml` against the directory (a stale log is explained, never
//!    guessed), shows the reverse preview and submits the rollback.
//!
//! Layout: [`glue`] (pure, unit-tested), [`workspace`] (temp dir, I/O), [`flow`] (the forward
//! flow), [`undo`]; dialogs are in `dialogs/batchrename_*.rs`.

pub mod flow;
pub mod glue;
pub mod logs;
pub mod undo;
pub mod workspace;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ofm_core::cmd;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;

/// Where the flow currently is (state for tests and for refusing a second concurrent batch).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Phase {
    /// No batch rename in progress.
    #[default]
    Idle,
    /// The editor text is being written / the editor is running.
    Editing,
    /// A detached editor is open and the "Waiting for your editor" dialog is shown.
    Waiting,
    /// The edited file is being read and planned.
    Planning,
    /// A problem dialog is shown; the text is the explanation.
    Problem(String),
    /// The preview is shown; the lines are what it lists.
    Preview(Vec<String>),
    /// The job was submitted (the session itself is over).
    Applying,
}

/// Module state in `ctx.ext`.
#[derive(Default)]
pub struct BatchState {
    active: Cell<bool>,
    phase: RefCell<Phase>,
    seq: Cell<u64>,
    notice: RefCell<Option<String>>,
}

impl BatchState {
    /// The current phase.
    pub fn phase(&self) -> Phase {
        self.phase.borrow().clone()
    }

    /// Whether a batch rename session is in progress.
    pub fn active(&self) -> bool {
        self.active.get()
    }

    /// The last message shown in a plain message dialog (nothing selected, undo not possible, ...).
    pub fn last_notice(&self) -> Option<String> {
        self.notice.borrow().clone()
    }

    pub(crate) fn set_phase(&self, p: Phase) {
        *self.phase.borrow_mut() = p;
    }

    pub(crate) fn set_active(&self, on: bool) {
        self.active.set(on);
    }

    pub(crate) fn next_seq(&self) -> u64 {
        let n = self.seq.get();
        self.seq.set(n + 1);
        n
    }

    pub(crate) fn set_notice(&self, text: &str) {
        *self.notice.borrow_mut() = Some(text.to_owned());
    }
}

/// Register `file.batch_rename` and `file.batch_rename_undo`.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::FILE_BATCH_RENAME, flow::start);
    reg.register(cmd::FILE_BATCH_RENAME_UNDO, undo::start);
}

/// Create [`BatchState`] in `ctx.ext`.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(BatchState::default()));
}

/// The module state (`None` only before [`install_state`]).
pub fn state(ctx: &AppContext) -> Option<Rc<BatchState>> {
    ctx.ext.get::<BatchState>()
}
