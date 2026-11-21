//! Desktop integration (F31): Enter opens/runs files, the file clipboard, drag and drop; the
//! external editor (F4, Shift+F4; T5.2b). Owner of the F31 files: T8.4.

pub mod clip;
pub mod clipboard;
pub mod dnd;
pub mod editor;
pub mod newfile;
pub mod open;
pub mod transfer;

use std::rc::Rc;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::window::PanelView;

/// Register `file.edit`, `file.new_file`, `file.open_with` and the clipboard commands.
pub fn install_commands(reg: &mut CommandRegistry) {
    editor::install_commands(reg);
    newfile::install_commands(reg);
    open::install_commands(reg);
    clipboard::install_commands(reg);
}

/// Per panel view: accept file drops (hook in `hooks.rs`).
pub fn install_panel_view(ctx: &Rc<AppContext>, view: &PanelView) {
    dnd::install_panel_view(ctx, view);
}
