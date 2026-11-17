//! Desktop integration: the external editor (F4, Shift+F4; T5.2b). `file.open_with` and friends
//! join this module in a later milestone.

pub mod editor;
pub mod newfile;

use crate::dispatch::CommandRegistry;

/// Register `file.edit` and `file.new_file`.
pub fn install_commands(reg: &mut CommandRegistry) {
    editor::install_commands(reg);
    newfile::install_commands(reg);
}
