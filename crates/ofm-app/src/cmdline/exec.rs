//! The Enter decision (pure; unit-tested): a plain resolvable `cd` navigates the active panel
//! itself, everything else goes to the shell (ADR-0003). Relative operands resolve against the
//! *panel's* directory — on a non-local panel (archive, panelized list) no `cd` can be resolved,
//! so it is the shell's turn and OSC 7 moves the panel afterwards.

use std::path::{Path, PathBuf};

use ofm_core::Location;
use ofm_core::cmdline::parse_cd_target;

/// What Enter does with a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecuteAction {
    /// Navigate the active panel here (a plain `cd` with a resolvable target); the shell is not
    /// touched — the next panel→shell sync aligns it.
    Navigate(PathBuf),
    /// Send the line to the shell (`TerminalHandle::send_line`) and show the terminal.
    SendShell,
}

/// Decide [`ExecuteAction`] for `text` typed in the command line while the active panel is at
/// `loc` and the user's home is `home`.
pub fn decide_execute(text: &str, loc: &Location, home: &Path) -> ExecuteAction {
    let cwd = match loc {
        Location::Local(p) => p,
        Location::Archive { .. } | Location::List { .. } => {
            // No local directory to resolve against: `cd x` would be relative to a path that
            // does not exist on the file system. The shell (whose cwd is a real directory)
            // handles it and reports back via OSC 7.
            return ExecuteAction::SendShell;
        }
    };
    match parse_cd_target(text, cwd, home) {
        Some(target) => ExecuteAction::Navigate(target),
        // Includes `cd -`, options, variables and every other command.
        None => ExecuteAction::SendShell,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn local() -> Location {
        Location::Local("/work/proj".into())
    }

    #[test]
    fn plain_cd_navigates() {
        let home = Path::new("/home/u");
        for (text, want) in [
            ("cd", "/home/u"),
            ("cd ~", "/home/u"),
            ("cd sub", "/work/proj/sub"),
            ("cd ./x/../y", "/work/proj/y"),
            ("cd ../lib", "/work/lib"),
            ("cd '/etc'", "/etc"),
            ("cd  --  sub", "/work/proj/sub"),
        ] {
            assert_eq!(
                decide_execute(text, &local(), home),
                ExecuteAction::Navigate(want.into()),
                "{text}"
            );
        }
    }

    #[test]
    fn everything_else_goes_to_the_shell() {
        let home = Path::new("/home/u");
        for text in [
            "",
            "   ",
            "ls -l",
            "cd -",
            "cd -P x",
            "cd $HOME",
            "cd a b",
            "cd a; make",
            "make && cd /tmp",
            "cdx",
        ] {
            assert_eq!(
                decide_execute(text, &local(), home),
                ExecuteAction::SendShell,
                "{text}"
            );
        }
    }

    #[test]
    fn non_local_panels_send_even_a_plain_cd() {
        let home = Path::new("/home/u");
        let archive = Location::Archive {
            archive: "/tmp/x.zip".into(),
            inner: "".into(),
        };
        let list = Location::List {
            id: ofm_core::ListId(1),
        };
        assert_eq!(
            decide_execute("cd /tmp", &archive, home),
            ExecuteAction::SendShell
        );
        assert_eq!(
            decide_execute("cd sub", &list, home),
            ExecuteAction::SendShell
        );
    }
}
