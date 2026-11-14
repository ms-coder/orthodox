//! Command identity types. The list of commands lives in [`crate::commands`].

use std::fmt;

/// Stable string id of a user action, e.g. `"file.copy"`. Every action in the app is a command.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandId(&'static str);

impl CommandId {
    /// Const constructor (used by the registry; call sites use `cmd::*` constants).
    pub const fn new(id: &'static str) -> Self {
        CommandId(id)
    }
    /// The id string.
    pub const fn as_str(&self) -> &'static str {
        self.0
    }
    /// Look up a registered command by its string id (keymap parsing).
    pub fn lookup(id: &str) -> Option<CommandId> {
        crate::commands::ALL
            .iter()
            .find(|c| c.id.0 == id)
            .map(|c| c.id)
    }
    /// Registry metadata of this command.
    pub fn info(&self) -> Option<&'static CommandInfo> {
        crate::commands::ALL.iter().find(|c| c.id == *self)
    }
}

impl fmt::Debug for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CommandId({})", self.0)
    }
}
impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Keymap section a command's default binding lives in; also how the dispatcher resolves keys
/// (ADR-0005): modal dialog -> `Dialog`; viewer window -> `Viewer`; else (main window)
/// `CmdLine` (only while the command line is non-empty), then `Panels`, then `Global`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeyContext {
    /// Always available in the main window; lowest priority.
    Global,
    /// Main window with the file panels (the normal state).
    Panels,
    /// Main window while the command line has text; beats `Panels`.
    CmdLine,
    /// Viewer window.
    Viewer,
    /// Modal dialogs and the jobs view.
    Dialog,
}

impl KeyContext {
    /// Section name in keymap files (`global`, `panels`, `cmdline`, `viewer`, `dialog`).
    pub const fn section(&self) -> &'static str {
        match self {
            KeyContext::Global => "global",
            KeyContext::Panels => "panels",
            KeyContext::CmdLine => "cmdline",
            KeyContext::Viewer => "viewer",
            KeyContext::Dialog => "dialog",
        }
    }
    /// Inverse of [`section`](Self::section).
    pub fn from_section(s: &str) -> Option<Self> {
        [
            Self::Global,
            Self::Panels,
            Self::CmdLine,
            Self::Viewer,
            Self::Dialog,
        ]
        .into_iter()
        .find(|c| c.section() == s)
    }
}

/// Registry metadata of a command (menus, F-key bar, docs, keymap validation).
#[derive(Debug, Clone, Copy)]
pub struct CommandInfo {
    /// Id.
    pub id: CommandId,
    /// English title (wrap with `tr()` in the UI).
    pub title: &'static str,
    /// Context of the default binding.
    pub context: KeyContext,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_consistent() {
        let mut seen = std::collections::HashSet::new();
        for c in crate::commands::ALL {
            assert!(seen.insert(c.id), "duplicate {}", c.id);
            assert_eq!(CommandId::lookup(c.id.as_str()), Some(c.id));
        }
        assert!(CommandId::lookup("nope.nothing").is_none());
        assert_eq!(KeyContext::from_section("viewer"), Some(KeyContext::Viewer));
    }
}
