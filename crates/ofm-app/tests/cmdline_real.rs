//! Real-binary tests for the command line (F11, T4.2): the `command_line` typing-mode key
//! routing, the Enter decision (a plain `cd` navigates the active panel, everything else is
//! stored in the history), Esc clearing, Ctrl+Enter name insertion, Tab completion (unique
//! completes with a trailing `/` for directories, ambiguous extends by the common prefix) and
//! Ctrl+E / Ctrl+Shift+E history stepping with a surviving draft — all through the real `ofm`
//! binary under a private Xvfb.
//!
//! The shell side (`TerminalHandle::send_line`) is a T4.1 stub, so shell-bound lines are
//! asserted on the persisted `history.toml` (written by a pool worker — the tests settle
//! before quitting, process exit does not drain the pool). The `cd` navigation is asserted on
//! the saved session. Skips (with a log line) when Xvfb/xdotool/import/python3 are missing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods // test fixture setup runs on the test thread, not the GTK thread
)]

mod real_support;

use std::time::Duration;

use ofm_config::history::History;
use ofm_testkit::tree::{FileSpec, TreeSpec};
use real_support::{Running, tools_available};

/// Let the last history write (async on the pool) land, quit, and return the persisted
/// command history (newest first) plus the saved session.
fn quit_with_history(s: Running) -> (Vec<String>, ofm_config::session::Session, Running) {
    std::thread::sleep(Duration::from_millis(1000));
    let (session, s) = s.quit();
    let (h, warnings) = History::load(&s.paths);
    assert!(warnings.is_empty(), "history warnings: {warnings:?}");
    (h.commands, session, s)
}

/// Typing mode `command_line`: plain printable keys reach the command line, the text is
/// visible, and Enter on a plain resolvable `cd` navigates the active panel (no shell).
#[test]
fn typed_text_appears_and_plain_cd_navigates() {
    if !tools_available() {
        return;
    }
    let s = Running::launch("");
    let blank = s.shot();
    s.keys(&["c", "d", "space", "s", "u", "b"]);
    let typed = s.shot();
    assert!(
        blank.diff_pixels(&typed) > 50,
        "typed text is not visible in the command line ({} px differ)",
        blank.diff_pixels(&typed)
    );
    s.keys(&["Return"]);
    let (session, s) = s.quit();
    assert_eq!(
        session.panels[0].tabs[0].path_buf().expect("tab path"),
        s.left.join("sub"),
        "Enter on `cd sub` did not navigate the panel"
    );
}

/// Esc with text clears the line; without the clearing, `junkcd sub` would have gone to the
/// shell and the panel would not have moved.
#[test]
fn escape_clears_the_line() {
    if !tools_available() {
        return;
    }
    let s = Running::launch("");
    s.keys(&["j", "u", "n", "k", "Escape"]);
    s.keys(&["c", "d", "space", "s", "u", "b", "Return"]);
    let (session, s) = s.quit();
    assert_eq!(
        session.panels[0].tabs[0].path_buf().expect("tab path"),
        s.left.join("sub"),
        "Esc did not clear the line before typing"
    );
}

/// A line that is not a plain `cd` is sent to the shell (T4.1 stub) and pushed into the
/// persisted history.
#[test]
fn shell_bound_line_lands_in_history() {
    if !tools_available() {
        return;
    }
    let s = Running::launch("");
    s.keys(&["e", "c", "h", "o", "space", "h", "i", "Return"]);
    let (commands, _session, _s) = quit_with_history(s);
    assert_eq!(
        commands.first().map(String::as_str),
        Some("echo hi"),
        "shell-bound line missing from history: {commands:?}"
    );
}

/// Ctrl+Enter inserts the name under the panel cursor into the line.
#[test]
fn ctrl_enter_inserts_name_under_cursor() {
    if !tools_available() {
        return;
    }
    let s = Running::launch("");
    s.keys(&["Down", "Down"]); // `..` -> sub -> a.txt
    s.keys(&["ctrl+Return"]);
    let with_name = s.shot();
    s.keys(&["Return"]);
    let (commands, _session, _s) = quit_with_history(s);
    assert!(
        with_name.distinct_colors() >= 6,
        "screen looks blank after Ctrl+Enter"
    );
    assert!(
        commands.iter().any(|c| c == "a.txt"),
        "inserted name not executed/stored: {commands:?}"
    );
}

/// Tab completion: an ambiguous prefix extends by the common prefix (`cat a` -> `cat amb`,
/// the candidate list pops over the entry); a unique directory completes with a trailing `/`
/// and Enter then navigates into it.
#[test]
fn tab_completes_unique_and_extends_ambiguous() {
    if !tools_available() {
        return;
    }
    let left = TreeSpec::new()
        .dir(
            "unique_dir",
            TreeSpec::new().file("u.txt", FileSpec::content(b"u")),
        )
        .file("amb1", FileSpec::content(b"1"))
        .file("amb2", FileSpec::content(b"2"));
    let right = TreeSpec::new().file("x1", FileSpec::content(b"x"));
    let s = Running::launch_with("", &left, &right);

    s.keys(&["c", "a", "t", "space", "a"]);
    let before_tab = s.shot();
    s.keys(&["Tab"]);
    let after_tab = s.shot();
    assert!(
        before_tab.diff_pixels(&after_tab) > 20,
        "Tab completion did not change the line/popover ({} px differ)",
        before_tab.diff_pixels(&after_tab)
    );
    s.keys(&["Return"]); // `cat amb` -> shell

    s.keys(&["c", "d", "space", "u", "Tab", "Return"]); // -> `cd unique_dir/`, navigates
    let (commands, session, s) = quit_with_history(s);
    assert!(
        commands.iter().any(|c| c == "cat amb"),
        "ambiguous prefix was not extended by the common prefix: {commands:?}"
    );
    assert_eq!(
        commands.first().map(String::as_str),
        Some("cd unique_dir/"),
        "unique completion did not complete the directory: {commands:?}"
    );
    assert_eq!(
        session.panels[0].tabs[0].path_buf().expect("tab path"),
        s.left.join("unique_dir"),
        "Enter on the completed `cd` did not navigate"
    );
}

/// Ctrl+E / Ctrl+Shift+E step through the history and the half-typed draft survives the
/// excursion: stepping back to the oldest entry and forward past the newest restores the
/// draft, which Enter then executes into the history.
#[test]
fn history_stepping_restores_the_draft() {
    if !tools_available() {
        return;
    }
    let s = Running::launch("");
    s.keys(&["e", "c", "h", "o", "space", "o", "n", "e", "Return"]);
    s.keys(&["e", "c", "h", "o", "space", "t", "w", "o", "Return"]);
    // Half-typed draft, then an excursion: two steps back, two steps forward.
    s.keys(&["d", "r", "a", "f", "t"]);
    s.keys(&["ctrl+e"]); // "echo two" (draft saved)
    s.keys(&["ctrl+e"]); // "echo one"
    s.keys(&["ctrl+shift+e"]); // "echo two"
    s.keys(&["ctrl+shift+e"]); // draft "draft" again
    s.keys(&["Return"]); // executes the draft
    let (commands, _session, _s) = quit_with_history(s);
    assert_eq!(
        commands.first().map(String::as_str),
        Some("draft"),
        "draft did not survive the history excursion: {commands:?}"
    );
    assert!(
        commands.iter().any(|c| c == "echo one") && commands.iter().any(|c| c == "echo two"),
        "executed lines missing from history: {commands:?}"
    );
}
