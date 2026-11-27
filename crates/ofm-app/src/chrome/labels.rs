//! Short F-key bar labels. The bar shows whatever command the *current keymap* binds to each F-key
//! (with the held modifiers), so rebinding changes the bar; this table only provides short names
//! for the common commands, everything else falls back to the registry title.

use ofm_core::{CommandId, cmd};

use crate::context::AppContext;
use crate::i18n::tr;

/// English title of `id` as menus, Help and accessible names show it (pass it to `tr`): the
/// registry title, except where a setting changes what the command does. With `[jobs] trash =
/// false`, `file.delete` does not go to the Trash.
pub fn command_title(ctx: &AppContext, id: CommandId) -> &'static str {
    if id == cmd::FILE_DELETE && !ctx.config.borrow().jobs.trash {
        return "Delete (permanently)";
    }
    id.info().map_or(id.as_str(), |i| i.title)
}

/// Short label of `id` for the F-key bar.
pub fn short_label(id: CommandId) -> String {
    let short: Option<&'static str> = match id {
        x if x == cmd::APP_HELP => Some("Help"),
        x if x == cmd::APP_MENU => Some("Menu"),
        x if x == cmd::APP_QUIT => Some("Quit"),
        x if x == cmd::APP_PREFERENCES => Some("Prefs"),
        x if x == cmd::APP_THEME_NEXT => Some("Theme"),
        x if x == cmd::MENU_USER => Some("UserMenu"),
        x if x == cmd::FILE_VIEW => Some("View"),
        x if x == cmd::FILE_EDIT => Some("Edit"),
        x if x == cmd::FILE_NEW_FILE => Some("NewFile"),
        x if x == cmd::FILE_COPY => Some("Copy"),
        x if x == cmd::FILE_MOVE => Some("Move"),
        x if x == cmd::FILE_RENAME => Some("Rename"),
        x if x == cmd::FILE_MKDIR => Some("MkDir"),
        x if x == cmd::FILE_DELETE => Some("Delete"),
        x if x == cmd::FILE_DELETE_PERMANENT => Some("Wipe"),
        x if x == cmd::FILE_LINK => Some("Link"),
        x if x == cmd::FILE_PROPERTIES => Some("Attr"),
        x if x == cmd::SEARCH_FIND => Some("Find"),
        x if x == cmd::PANEL_SORT_NAME => Some("ByName"),
        x if x == cmd::PANEL_SORT_EXT => Some("ByExt"),
        x if x == cmd::PANEL_SORT_MTIME => Some("ByTime"),
        x if x == cmd::PANEL_SORT_SIZE => Some("BySize"),
        x if x == cmd::PANEL_SORT_REVERSE => Some("Reverse"),
        x if x == cmd::PANEL_TOGGLE_HIDDEN => Some("Hidden"),
        x if x == cmd::PANEL_DRIVE_MENU_LEFT || x == cmd::PANEL_DRIVE_MENU_RIGHT => Some("Drives"),
        x if x == cmd::PANEL_COMPARE_DIRS => Some("Compare"),
        x if x == cmd::PANEL_CONTEXT_MENU => Some("Context"),
        x if x == cmd::PANEL_HISTORY_LIST => Some("History"),
        x if x == cmd::CMDLINE_HISTORY_LIST => Some("CmdHist"),
        x if x == cmd::WINDOW_FULLSCREEN => Some("Fullscr"),
        _ => None,
    };
    match short {
        Some(s) => tr(s).to_owned(),
        None => id
            .info()
            .map_or_else(|| id.to_string(), |i| tr(i.title).to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_and_fallback_labels() {
        assert_eq!(short_label(cmd::FILE_COPY), "Copy");
        assert_eq!(short_label(cmd::PANEL_CURSOR_UP), "Cursor up");
    }
}
