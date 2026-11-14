//! Structure of the F9 menu bar: which registry commands appear under which title. Titles of the
//! entries and their key hints are not stored here: they come from the command registry
//! (`ofm_core::commands::ALL`) and the active keymap when the menu opens.

use ofm_core::{CommandId, cmd};

/// One row of a drop-down.
#[derive(Debug, Clone, Copy)]
pub enum Item {
    /// A registry command.
    Cmd(CommandId),
    /// A separator line.
    Sep,
}

/// One drop-down.
#[derive(Debug, Clone, Copy)]
pub struct MenuDef {
    /// Title in the bar (English; wrapped in `tr()` when built).
    pub title: &'static str,
    /// Rows.
    pub items: &'static [Item],
}

use Item::{Cmd, Sep};

/// All menus, left to right.
pub const MENUS: &[MenuDef] = &[
    MenuDef {
        title: "Files",
        items: &[
            Cmd(cmd::FILE_VIEW),
            Cmd(cmd::FILE_EDIT),
            Cmd(cmd::FILE_NEW_FILE),
            Sep,
            Cmd(cmd::FILE_COPY),
            Cmd(cmd::FILE_MOVE),
            Cmd(cmd::FILE_RENAME),
            Cmd(cmd::FILE_MKDIR),
            Cmd(cmd::FILE_DELETE),
            Cmd(cmd::FILE_DELETE_PERMANENT),
            Sep,
            Cmd(cmd::FILE_LINK),
            Cmd(cmd::FILE_PROPERTIES),
            Cmd(cmd::FILE_CALC_SIZE),
            Cmd(cmd::FILE_BATCH_RENAME),
            Cmd(cmd::FILE_OPEN_WITH),
            Sep,
            Cmd(cmd::FILE_CLIPBOARD_COPY),
            Cmd(cmd::FILE_CLIPBOARD_CUT),
            Cmd(cmd::FILE_CLIPBOARD_PASTE),
            Cmd(cmd::FILE_COPY_PATH),
            Sep,
            Cmd(cmd::APP_QUIT),
        ],
    },
    MenuDef {
        title: "Mark",
        items: &[
            Cmd(cmd::SELECT_TOGGLE),
            Cmd(cmd::SELECT_BY_MASK),
            Cmd(cmd::SELECT_UNMASK),
            Cmd(cmd::SELECT_INVERT),
            Cmd(cmd::SELECT_ALL),
            Cmd(cmd::SELECT_NONE),
            Cmd(cmd::SELECT_RESTORE),
        ],
    },
    MenuDef {
        title: "Panel",
        items: &[
            Cmd(cmd::PANEL_VIEW_BRIEF),
            Cmd(cmd::PANEL_VIEW_FULL),
            Cmd(cmd::PANEL_VIEW_CUSTOM),
            Sep,
            Cmd(cmd::PANEL_SORT_NAME),
            Cmd(cmd::PANEL_SORT_EXT),
            Cmd(cmd::PANEL_SORT_MTIME),
            Cmd(cmd::PANEL_SORT_SIZE),
            Cmd(cmd::PANEL_SORT_REVERSE),
            Cmd(cmd::PANEL_TOGGLE_HIDDEN),
            Sep,
            Cmd(cmd::PANEL_REFRESH),
            Cmd(cmd::PANEL_SWITCH),
            Cmd(cmd::PANEL_TOGGLE_OTHER),
            Cmd(cmd::PANEL_SWAP),
            Cmd(cmd::PANEL_SAME_DIR),
            Cmd(cmd::PANEL_TOGGLE_QUICKVIEW),
            Sep,
            Cmd(cmd::TAB_NEW),
            Cmd(cmd::TAB_CLOSE),
            Cmd(cmd::TAB_NEXT),
            Cmd(cmd::TAB_PREV),
            Cmd(cmd::TAB_LOCK),
        ],
    },
    MenuDef {
        title: "Go",
        items: &[
            Cmd(cmd::PANEL_GO_UP),
            Cmd(cmd::PANEL_GO_ROOT),
            Cmd(cmd::PANEL_GO_HOME),
            Cmd(cmd::PANEL_GOTO_PATH),
            Sep,
            Cmd(cmd::PANEL_HISTORY_BACK),
            Cmd(cmd::PANEL_HISTORY_FORWARD),
            Cmd(cmd::PANEL_HISTORY_LIST),
            Cmd(cmd::PANEL_HOTLIST),
            Cmd(cmd::PANEL_HOTLIST_ADD),
            Sep,
            Cmd(cmd::PANEL_DRIVE_MENU_LEFT),
            Cmd(cmd::PANEL_DRIVE_MENU_RIGHT),
        ],
    },
    MenuDef {
        title: "Commands",
        items: &[
            Cmd(cmd::SEARCH_FIND),
            Cmd(cmd::SEARCH_PANELIZE_COMMAND),
            Cmd(cmd::PANEL_COMPARE_DIRS),
            Cmd(cmd::PANEL_QUICKSEARCH),
            Cmd(cmd::PANEL_FILTER),
            Sep,
            Cmd(cmd::MENU_USER),
            Cmd(cmd::CMDLINE_HISTORY_LIST),
            Cmd(cmd::APP_TERMINAL_TOGGLE),
            Cmd(cmd::APP_JOBS),
        ],
    },
    MenuDef {
        title: "Options",
        items: &[
            Cmd(cmd::APP_PREFERENCES),
            Cmd(cmd::APP_THEME_NEXT),
            Cmd(cmd::APP_RELOAD_CONFIG),
            Cmd(cmd::WINDOW_FULLSCREEN),
            Sep,
            Cmd(cmd::APP_HELP),
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_menu_entry_is_a_registered_command_listed_once() {
        let mut seen = std::collections::HashSet::new();
        for m in MENUS {
            assert!(!m.items.is_empty(), "{}", m.title);
            for it in m.items {
                if let Item::Cmd(id) = it {
                    assert!(id.info().is_some(), "{id} is not in the command registry");
                    assert!(seen.insert(*id), "{id} listed twice");
                }
            }
            // No leading/trailing/double separators.
            assert!(matches!(m.items.first(), Some(Item::Cmd(_))), "{}", m.title);
            assert!(matches!(m.items.last(), Some(Item::Cmd(_))), "{}", m.title);
            for pair in m.items.windows(2) {
                assert!(!matches!(pair, [Item::Sep, Item::Sep]), "{}", m.title);
            }
        }
    }
}
