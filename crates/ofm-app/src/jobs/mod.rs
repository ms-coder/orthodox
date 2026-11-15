//! File operations on the UI side (M3, owner T3.3): the [`JobsController`], the jobs view and the
//! commands `file.copy/move/rename/mkdir/delete/delete_permanent/link/properties/calc_size`,
//! `app.jobs` and `jobs.*`. T3.0 froze the skeleton below; T3.3 owns every file in this directory
//! and the dialog files named in `OWNERSHIP.md` (and nothing else).
//!
//! Wiring (all through the frozen hooks in `hooks.rs`, so no shared file is edited):
//! - [`install_commands`]: register the commands above (each calls a `dialogs::*::present` or
//!   submits directly through [`JobsController::submit`]);
//! - [`install_state`]: `ctx.ext.insert(Rc::new(JobsController::new()))` (done; the engine itself
//!   is created lazily on the first submit, so startup stays cheap);
//! - [`install_ui`]: jobs view (Ctrl+J), progress in the status line, quit-with-running-jobs prompt.
//!
//! Data flow: command -> dialog -> `JobsController::submit(ctx, JobSpec, RunMode)` ->
//! `ofm_jobs::JobEngine` (worker threads) -> `JobEvent`s -> `wiring::attach` pump ->
//! [`JobsController::handle_event`] (main thread): updates the job table, emits
//! [`JobsEvent`]s, shows question dialogs, and on `Finished` refreshes the panels
//! (`PanelController::focus_name` on the first `Report::created` item).

mod controller;

pub use controller::{JobEntry, JobsController, JobsEvent};

use std::rc::Rc;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::window::MainWindow;

/// Register this module's commands (T3.3 fills in; each command id is registered exactly once,
/// by this function): `file.copy`, `file.move`, `file.rename`, `file.mkdir`, `file.delete`,
/// `file.delete_permanent`, `file.link`, `file.properties`, `file.calc_size`, `app.jobs`,
/// `jobs.pause_resume`, `jobs.cancel`, `jobs.clear_finished`.
pub fn install_commands(_reg: &mut CommandRegistry) {}

/// Create the [`JobsController`] and store it in `ctx.ext`.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(JobsController::new()));
}

/// Build the jobs view and status-line progress once the window exists (T3.3).
pub fn install_ui(_ctx: &Rc<AppContext>, _win: &MainWindow) {}

/// The context's [`JobsController`] (`None` only before `install_state` ran).
pub fn controller(ctx: &AppContext) -> Option<Rc<JobsController>> {
    ctx.ext.get::<JobsController>()
}
