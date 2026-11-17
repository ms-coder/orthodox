//! F23: choosing and launching the external editor.
//!
//! Selection (pure, unit-tested): the first `[[editor.rule]]` whose mask matches the file name,
//! else `[editor] command`, else `$VISUAL`, else `$EDITOR`, else GNOME Text Editor. A command is
//! a template split into words (`'` and `"` quotes, `\` escapes); `%f` is replaced by the file
//! path, and the path is appended as the last word when no word contains `%f`.
//!
//! Launching: GUI editors are spawned detached through GIO (`gio::Subprocess`, the child is
//! reaped by the main loop; the argument vector is exact bytes, so non-UTF-8 names work).
//! Terminal editors (decided by `terminal = true/false` in config, else by the configured and
//! built-in lists of well-known terminal editors) run inside the embedded terminal as their own
//! child process with the exact argument vector (`TerminalHandle::run_program`, no line is typed
//! into the shell); the panels come back when the editor exits.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::gio;
use gtk::prelude::*;
use ofm_config::config::EditorSection;
use ofm_core::mask::{Mask, MaskOpts};
use ofm_core::{EntryKind, Location, TargetKind, cmd};

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::i18n::tr;
use crate::jobs::ui;
use crate::terminal;

/// Editors that need a terminal even when the user's list does not name them.
pub const BUILTIN_TERMINAL_EDITORS: &[&str] = &[
    "vim", "vi", "nvim", "view", "vimx", "nano", "pico", "mcedit", "micro", "hx", "helix", "joe",
    "jed", "ne", "kak", "kakoune", "mg", "ed", "tilde", "zile",
];

/// Flags that make `emacs`/`emacsclient` run in the terminal.
const EMACS_TERMINAL_FLAGS: &[&str] = &["-nw", "-t", "--tty", "--no-window-system"];

/// Where the chosen command came from (diagnostics and tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A `[[editor.rule]]` mask matched.
    Rule,
    /// `[editor] command`.
    Config,
    /// `$VISUAL`.
    Visual,
    /// `$EDITOR`.
    Editor,
}

/// The editor selected for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Run `words` (a template, see the module docs); inside the terminal when `terminal`.
    Command {
        /// Template words.
        words: Vec<String>,
        /// Run inside the embedded terminal.
        terminal: bool,
        /// Which setting produced it.
        source: Source,
    },
    /// Nothing configured: GNOME Text Editor.
    GnomeTextEditor,
}

/// The environment variables that take part in the selection (a value instead of
/// `std::env` so tests do not touch the process environment).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorEnv {
    /// `$VISUAL`.
    pub visual: Option<String>,
    /// `$EDITOR`.
    pub editor: Option<String>,
}

impl EditorEnv {
    /// Read `$VISUAL` and `$EDITOR` of this process (empty values count as unset).
    pub fn from_process() -> Self {
        let get = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        EditorEnv {
            visual: get("VISUAL"),
            editor: get("EDITOR"),
        }
    }
}

/// Split a command template into words: whitespace separates, `'…'` is literal, `"…"` allows
/// `\"` and `\\`, a `\` outside quotes escapes the next character. An unterminated quote runs
/// to the end of the text.
pub fn split_words(template: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    cur.push(q);
                }
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' if matches!(chars.peek(), Some('"' | '\\')) => {
                            if let Some(n) = chars.next() {
                                cur.push(n);
                            }
                        }
                        _ => cur.push(q),
                    }
                }
            }
            '\\' => {
                in_word = true;
                cur.push(chars.next().unwrap_or('\\'));
            }
            _ => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    words
}

/// Whether the command starting with `words` is a terminal editor: named in `listed` (the
/// `[editor] terminal_editors` setting) or in [`BUILTIN_TERMINAL_EDITORS`], or `emacs`/
/// `emacsclient` with a flag that keeps them in the terminal. Matches the program's base name.
pub fn is_terminal_editor(words: &[String], listed: &[String]) -> bool {
    let Some(first) = words.first() else {
        return false;
    };
    let base = Path::new(first)
        .file_name()
        .map_or_else(|| first.clone(), |n| n.to_string_lossy().into_owned());
    if listed.contains(&base) || BUILTIN_TERMINAL_EDITORS.contains(&base.as_str()) {
        return true;
    }
    matches!(base.as_str(), "emacs" | "emacsclient")
        && words
            .iter()
            .skip(1)
            .any(|w| EMACS_TERMINAL_FLAGS.contains(&w.as_str()))
}

/// Select the editor for the file called `name`.
pub fn choose(cfg: &EditorSection, name: &OsStr, env: &EditorEnv) -> Choice {
    // First matching rule wins; an empty command means "the default editor for these files".
    for rule in &cfg.rule {
        let Ok(mask) = Mask::parse(
            &rule.mask,
            MaskOpts {
                case_insensitive: true,
            },
        ) else {
            tracing::warn!(mask = %rule.mask, "editor rule: invalid mask, skipped");
            continue;
        };
        if !mask.is_match(name) {
            continue;
        }
        let words = split_words(&rule.command);
        if words.is_empty() {
            break;
        }
        return command(cfg, words, rule.terminal, Source::Rule);
    }
    let words = split_words(&cfg.command);
    if !words.is_empty() {
        return command(cfg, words, cfg.terminal, Source::Config);
    }
    for (value, source) in [(&env.visual, Source::Visual), (&env.editor, Source::Editor)] {
        if let Some(v) = value {
            let words = split_words(v);
            if !words.is_empty() {
                return command(cfg, words, None, source);
            }
        }
    }
    Choice::GnomeTextEditor
}

fn command(
    cfg: &EditorSection,
    words: Vec<String>,
    forced: Option<bool>,
    source: Source,
) -> Choice {
    let terminal = forced.unwrap_or_else(|| is_terminal_editor(&words, &cfg.terminal_editors));
    Choice::Command {
        words,
        terminal,
        source,
    }
}

/// The argument vector for `words` and the file `path`: `%f` becomes the path (bytes, so
/// non-UTF-8 names survive), `%%` a literal `%`; the path is appended when no word used `%f`.
pub fn argv(words: &[String], path: &Path) -> Vec<OsString> {
    let file = path.as_os_str().as_bytes();
    let mut used = false;
    let mut out: Vec<OsString> = words
        .iter()
        .map(|w| {
            let b = w.as_bytes();
            let mut v = Vec::with_capacity(b.len() + file.len());
            let mut i = 0;
            while i < b.len() {
                match (b[i], b.get(i + 1)) {
                    (b'%', Some(b'f')) => {
                        v.extend_from_slice(file);
                        used = true;
                        i += 2;
                    }
                    (b'%', Some(b'%')) => {
                        v.push(b'%');
                        i += 2;
                    }
                    (c, _) => {
                        v.push(c);
                        i += 1;
                    }
                }
            }
            OsString::from_vec(v)
        })
        .collect();
    if !used {
        out.push(path.as_os_str().to_owned());
    }
    out
}

// ---- commands ---------------------------------------------------------------------------------

/// Register `file.edit` (F4). `file.new_file` (Shift+F4) is registered by [`super::newfile`].
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::FILE_EDIT, edit_cursor_file);
}

/// What F4 acts on: the real local path of the item under the cursor of the active panel.
fn cursor_file(ctx: &AppContext) -> Result<PathBuf, String> {
    let ctrl = ctx.active_controller();
    let (entry, dir) = {
        let m = ctrl.model();
        (m.cursor_entry().cloned(), ctrl.location())
    };
    let Some(entry) = entry else {
        return Err(tr("There is no file under the cursor.").to_owned());
    };
    if entry.name == ".." || entry.kind.is_dir_like() {
        return Err(tr("A folder cannot be edited. Select a file.").to_owned());
    }
    if matches!(
        entry.kind,
        EntryKind::Other
            | EntryKind::Symlink {
                target: Some(TargetKind::Other) | None
            }
    ) {
        return Err(tr("Only regular files can be edited.").to_owned());
    }
    if let Some(p) = &entry.real_path {
        return Ok(p.clone());
    }
    match dir.join(&entry.name) {
        Location::Local(p) => Ok(p),
        _ => Err(tr("Files inside archives cannot be edited.").to_owned()),
    }
}

fn edit_cursor_file(ctx: &Rc<AppContext>) {
    match cursor_file(ctx) {
        Ok(path) => open_in_editor(ctx, &path),
        Err(msg) => ui::message(ctx, tr("Edit"), &msg),
    }
}

/// Open `path` in the editor chosen for it (see the module docs). Errors are shown in a message
/// dialog; the call itself never blocks on the editor.
pub fn open_in_editor(ctx: &Rc<AppContext>, path: &Path) {
    let name = path.file_name().unwrap_or(path.as_os_str());
    let choice = {
        let cfg = ctx.config.borrow();
        choose(&cfg.editor, name, &EditorEnv::from_process())
    };
    tracing::debug!(?choice, path = %path.display(), "open in editor");
    let result = match choice {
        Choice::GnomeTextEditor => launch_gnome_text_editor(path),
        Choice::Command {
            words,
            terminal: false,
            ..
        } => spawn_gui(&argv(&words, path)),
        Choice::Command {
            words,
            terminal: true,
            ..
        } => run_in_terminal(ctx, &argv(&words, path)),
    };
    if let Err(msg) = result {
        ui::message(ctx, tr("Edit"), &msg);
    }
}

/// Launch a GUI editor detached: exact argument vector, no shell, inherited environment.
fn spawn_gui(argv: &[OsString]) -> Result<(), String> {
    let args: Vec<&OsStr> = argv.iter().map(OsString::as_os_str).collect();
    gio::Subprocess::newv(&args, gio::SubprocessFlags::NONE)
        .map(|_| ())
        .map_err(|e| {
            format!(
                "{} {}: {e}",
                tr("Cannot start the editor"),
                argv.first()
                    .map(|a| a.to_string_lossy().into_owned())
                    .unwrap_or_default()
            )
        })
}

/// GNOME Text Editor through a GIO `AppInfo` (the launch goes through GIO, so the desktop
/// startup environment is honoured).
fn launch_gnome_text_editor(path: &Path) -> Result<(), String> {
    let app = gio::AppInfo::create_from_commandline(
        "gnome-text-editor",
        Some("GNOME Text Editor"),
        gio::AppInfoCreateFlags::NONE,
    )
    .map_err(|e| e.to_string())?;
    let file = gio::File::for_path(path);
    app.launch(&[file], None::<&gio::AppLaunchContext>)
        .map_err(|e| {
            format!(
                "{} ({e}). {}",
                tr("GNOME Text Editor is not available"),
                tr("Set $EDITOR or [editor] command in the configuration.")
            )
        })
}

/// Run a terminal editor in the embedded terminal as its own child process (exact argv, no
/// line typed into the shell: no quoting, no half-typed-line or shell-idle problems, every
/// shell kind works). The panels come back when the editor exits.
fn run_in_terminal(ctx: &Rc<AppContext>, argv: &[OsString]) -> Result<(), String> {
    let handle =
        terminal::handle(ctx).ok_or_else(|| tr("The terminal is not available.").to_owned())?;
    let cwd = match ctx.active_controller().location() {
        Location::Local(p) => Some(p),
        _ => None,
    };
    let weak = Rc::downgrade(ctx);
    handle.run_program(argv, cwd.as_deref(), move |msg| {
        if let Some(ctx) = weak.upgrade() {
            ui::message(&ctx, tr("Edit"), &msg);
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use ofm_config::config::EditorRule;

    use super::*;

    fn cfg(command: &str, rules: &[(&str, &str, Option<bool>)]) -> EditorSection {
        EditorSection {
            command: command.into(),
            terminal_editors: vec!["vim".into(), "nano".into()],
            terminal: None,
            rule: rules
                .iter()
                .map(|(m, c, t)| EditorRule {
                    mask: (*m).into(),
                    command: (*c).into(),
                    terminal: *t,
                })
                .collect(),
        }
    }

    fn env(visual: Option<&str>, editor: Option<&str>) -> EditorEnv {
        EditorEnv {
            visual: visual.map(Into::into),
            editor: editor.map(Into::into),
        }
    }

    fn cmd_of(c: &Choice) -> (Vec<&str>, bool, Source) {
        match c {
            Choice::Command {
                words,
                terminal,
                source,
            } => (
                words.iter().map(String::as_str).collect(),
                *terminal,
                *source,
            ),
            Choice::GnomeTextEditor => panic!("expected a command"),
        }
    }

    #[test]
    fn first_matching_rule_wins() {
        let c = cfg(
            "global",
            &[("*.rs", "code %f", None), ("*.rs;*.toml", "other", None)],
        );
        let ch = choose(&c, OsStr::new("main.rs"), &env(None, None));
        assert_eq!(cmd_of(&ch), (vec!["code", "%f"], false, Source::Rule));
        let ch = choose(&c, OsStr::new("Cargo.toml"), &env(None, None));
        assert_eq!(cmd_of(&ch), (vec!["other"], false, Source::Rule));
    }

    #[test]
    fn rule_mask_is_case_insensitive() {
        let c = cfg("", &[("*.RS", "code", None)]);
        let ch = choose(&c, OsStr::new("lib.rs"), &env(None, None));
        assert_eq!(cmd_of(&ch).2, Source::Rule);
    }

    #[test]
    fn empty_rule_command_means_default_editor_and_stops_the_search() {
        let c = cfg("", &[("*.md", "", None), ("*.md", "later", None)]);
        let ch = choose(&c, OsStr::new("a.md"), &env(Some("vis"), Some("ed")));
        assert_eq!(cmd_of(&ch), (vec!["vis"], false, Source::Visual));
    }

    #[test]
    fn config_command_beats_environment() {
        let c = cfg("geany %f", &[]);
        let ch = choose(&c, OsStr::new("x"), &env(Some("vis"), Some("ed")));
        assert_eq!(cmd_of(&ch), (vec!["geany", "%f"], false, Source::Config));
    }

    #[test]
    fn visual_before_editor_before_gnome() {
        let c = cfg("", &[]);
        let ch = choose(&c, OsStr::new("x"), &env(Some("vis"), Some("ed")));
        assert_eq!(cmd_of(&ch).2, Source::Visual);
        let ch = choose(&c, OsStr::new("x"), &env(None, Some("ed")));
        assert_eq!(cmd_of(&ch), (vec!["ed"], true, Source::Editor));
        assert_eq!(
            choose(&c, OsStr::new("x"), &env(None, None)),
            Choice::GnomeTextEditor
        );
        // blank values are ignored
        let ch = choose(&c, OsStr::new("x"), &env(Some("  "), None));
        assert_eq!(ch, Choice::GnomeTextEditor);
    }

    #[test]
    fn invalid_mask_is_skipped() {
        let c = cfg("g", &[("[", "bad", None), ("*", "good", None)]);
        let ch = choose(&c, OsStr::new("x"), &env(None, None));
        assert_eq!(cmd_of(&ch).0, vec!["good"]);
    }

    #[test]
    fn terminal_detection_and_override() {
        let list = vec!["vim".to_owned(), "hx".to_owned()];
        let w = |s: &str| split_words(s);
        assert!(is_terminal_editor(&w("vim"), &list));
        assert!(is_terminal_editor(&w("/usr/bin/vim -p"), &list));
        assert!(is_terminal_editor(&w("nano"), &list)); // built-in
        assert!(is_terminal_editor(&w("mcedit"), &[]));
        assert!(!is_terminal_editor(&w("code -w"), &list));
        assert!(!is_terminal_editor(&w("emacs"), &[]));
        assert!(is_terminal_editor(&w("emacs -nw"), &[]));
        assert!(is_terminal_editor(&w("emacsclient -t %f"), &[]));
        assert!(!is_terminal_editor(&[], &list));
        // `terminal = ...` overrides the lists both ways
        let c = cfg(
            "",
            &[
                ("*.sh", "vim", Some(false)),
                ("*.log", "myviewer", Some(true)),
            ],
        );
        let ch = choose(&c, OsStr::new("a.sh"), &env(None, None));
        assert!(!cmd_of(&ch).1);
        let ch = choose(&c, OsStr::new("a.log"), &env(None, None));
        assert!(cmd_of(&ch).1);
    }

    #[test]
    fn splits_words_like_a_shell() {
        assert_eq!(split_words("code  -w   %f"), ["code", "-w", "%f"]);
        assert_eq!(
            split_words("sh -c 'echo \"a b\"' x"),
            ["sh", "-c", "echo \"a b\"", "x"]
        );
        assert_eq!(split_words("a \"b \\\" c\" d"), ["a", "b \" c", "d"]);
        assert_eq!(split_words("a\\ b c"), ["a b", "c"]);
        assert_eq!(split_words("''"), [""]);
        assert_eq!(split_words("  "), Vec::<String>::new());
        assert_eq!(split_words("a 'unterminated b"), ["a", "unterminated b"]);
    }

    #[test]
    fn argv_substitutes_or_appends_the_path() {
        let w = |s: &str| split_words(s);
        let p = Path::new("/d/my file.txt");
        assert_eq!(argv(&w("ed --file=%f"), p), ["ed", "--file=/d/my file.txt"]);
        assert_eq!(argv(&w("ed -w"), p), ["ed", "-w", "/d/my file.txt"]);
        assert_eq!(argv(&w("ed 100%% %f"), p), ["ed", "100%", "/d/my file.txt"]);
        let raw = Path::new(OsStr::from_bytes(b"/d/\xff\xfe.txt"));
        let a = argv(&w("ed"), raw);
        assert_eq!(a[1].as_bytes(), b"/d/\xff\xfe.txt");
    }
}
