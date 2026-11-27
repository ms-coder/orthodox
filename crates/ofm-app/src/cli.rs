//! Command-line flags: `--help`, `--version` and `--dump-keys <preset>`.
//!
//! These run before any GTK initialisation so they work without a display (packaging
//! post-install scripts, documentation generation, CI). Anything that is not a flag is a
//! positional panel path and starts the GUI (handled by [`crate::app`]).

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

use ofm_config::keymap::{Keymap, Preset, TypingMode};
use ofm_core::KeyContext;
use ofm_core::commands;

/// What the binary should do after parsing the arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// No flags: positional arguments are panel paths; start the GTK application.
    Gui,
    /// `--help` / `-h`.
    Help,
    /// `--version` / `-V`.
    Version,
    /// `--dump-keys <preset>`: print the key reference of the preset as Markdown.
    DumpKeys(Preset),
}

/// Parse the process arguments (without argv[0]). Unknown flags are an error; the
/// returned string is meant for stderr and is followed by exit code 2.
pub fn parse(args: &[OsString]) -> Result<Action, String> {
    let mut action: Option<Action> = None;
    let mut it = args.iter().peekable();
    while let Some(arg) = it.next() {
        // A non-UTF-8 argument can only be a path; flags are always valid UTF-8.
        let Some(text) = arg.to_str() else {
            continue;
        };
        match text {
            "--help" | "-h" => action = Some(Action::Help),
            "--version" | "-V" => action = Some(Action::Version),
            "--dump-keys" => {
                let Some(preset_arg) = it.next() else {
                    return Err("--dump-keys needs a preset name (far, tc or mc)".to_owned());
                };
                let Some(name) = preset_arg.to_str() else {
                    return Err("--dump-keys: preset name is not valid UTF-8".to_owned());
                };
                match Preset::from_name(name) {
                    Some(p) => action = Some(Action::DumpKeys(p)),
                    None => {
                        return Err(format!(
                            "--dump-keys: unknown preset {name:?} (expected far, tc or mc)"
                        ));
                    }
                }
            }
            _ if text.starts_with('-') && text.len() > 1 => {
                return Err(format!("unknown option {text:?} (try --help)"));
            }
            _ => {} // positional panel path; handled by `app`
        }
    }
    Ok(action.unwrap_or(Action::Gui))
}

/// Handle the flags: print and return an exit code, or tell the caller to start the GUI.
/// Output goes to stdout (help, version, key reference) or stderr (errors).
pub fn run(args: &[OsString]) -> Option<ExitCode> {
    match parse(args) {
        Ok(Action::Gui) => None,
        Ok(Action::Help) => Some(emit(&render_help())),
        Ok(Action::Version) => Some(emit(&format!("ofm {}\n", env!("CARGO_PKG_VERSION")))),
        Ok(Action::DumpKeys(preset)) => Some(emit(&render_keys(preset))),
        Err(message) => {
            eprintln!("ofm: {message}");
            Some(ExitCode::from(2))
        }
    }
}

/// Write to stdout. A closed pipe (`ofm --dump-keys far | head`) is not an error and must not
/// panic, as `print!` would.
fn emit(text: &str) -> ExitCode {
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ofm: cannot write to stdout: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `--help` text.
pub fn render_help() -> String {
    format!(
        "OFM — Orthodox File Manager (GTK 4)\n\
         \n\
         Usage: ofm [OPTIONS] [LEFT_DIR [RIGHT_DIR]]\n\
         \n\
         Arguments:\n\
         \x20 LEFT_DIR    Directory shown in the left panel (default: current directory)\n\
         \x20 RIGHT_DIR   Directory shown in the right panel (default: home directory)\n\
         \n\
         Options:\n\
         \x20 -h, --help              Print this help and exit\n\
         \x20 -V, --version           Print the version and exit\n\
         \x20     --dump-keys PRESET  Print the key reference of a keymap preset\n\
         \x20                         (far, tc or mc) as Markdown and exit\n\
         \n\
         Version: {}\n\
         Project: https://github.com/mscoder/ofm\n",
        env!("CARGO_PKG_VERSION")
    )
}

/// Display name and blurb of a preset for the generated document.
fn preset_name(preset: Preset) -> &'static str {
    match preset {
        Preset::Far => "Far Manager (default)",
        Preset::Tc => "Total / Double Commander",
        Preset::Mc => "Midnight Commander",
    }
}

/// What plain letter keys do under this keymap (the `typing` mode).
fn typing_blurb(mode: TypingMode) -> &'static str {
    match mode {
        TypingMode::CommandLine => {
            "Plain letter keys are appended to the command line; Alt+letter starts a quick search."
        }
        TypingMode::QuickSearch => "Plain letter keys start a quick search in the active panel.",
    }
}

/// When a keymap section is consulted (ADR-0005).
fn section_blurb(ctx: KeyContext) -> &'static str {
    match ctx {
        KeyContext::Global => "Always available in the main window.",
        KeyContext::Panels => "Main window with the file panels (the normal state).",
        KeyContext::CmdLine => "While the command line under the panels contains text.",
        KeyContext::Viewer => "Inside the F3 viewer window.",
        KeyContext::Dialog => "Inside modal dialogs and the jobs view.",
    }
}

/// Markdown key reference of a built-in preset: one table per keymap section, one row per
/// bound chord. Sections follow the dispatcher's resolution order (ADR-0005).
pub fn render_keys(preset: Preset) -> String {
    let keymap = Keymap::preset(preset);
    let mut out = String::new();
    out.push_str(&format!(
        "# OFM key reference — {} preset\n\
         \n\
         Generated by `ofm --dump-keys {}`. Do not edit by hand; rebind keys in\n\
         `$XDG_CONFIG_HOME/ofm/keymap.toml` instead (see the user guide).\n\
         \n\
         {}\n",
        preset_name(preset),
        preset_source_name(preset),
        typing_blurb(keymap.typing)
    ));
    for ctx in [
        KeyContext::Global,
        KeyContext::Panels,
        KeyContext::CmdLine,
        KeyContext::Viewer,
        KeyContext::Dialog,
    ] {
        // One row per bound chord, sorted by chord for lookup; command order is lost
        // deliberately: a key reference is searched by key, not by command.
        let mut rows: Vec<(ofm_core::KeyChord, ofm_core::CommandId)> = commands::ALL
            .iter()
            .flat_map(|info| {
                keymap
                    .chords_for(info.id)
                    .into_iter()
                    .filter(|(c, _)| *c == ctx)
                    .map(|(_, chord)| (chord, info.id))
            })
            .collect();
        rows.sort();
        out.push_str(&format!(
            "\n## {}\n\n{}\n\n| Key | Action | Command ID |\n|---|---|---|\n",
            ctx.section(),
            section_blurb(ctx)
        ));
        for (chord, id) in rows {
            let title = id.info().map_or("", |i| i.title);
            out.push_str(&format!("| {chord} | {title} | `{id}` |\n"));
        }
    }
    out
}

/// The `config.toml` spelling of a preset (`far`, `tc`, `mc`).
fn preset_source_name(preset: Preset) -> &'static str {
    match preset {
        Preset::Far => "far",
        Preset::Tc => "tc",
        Preset::Mc => "mc",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn flags_parse() {
        assert_eq!(parse(&[os("--help")]), Ok(Action::Help));
        assert_eq!(parse(&[os("-h")]), Ok(Action::Help));
        assert_eq!(parse(&[os("--version")]), Ok(Action::Version));
        assert_eq!(parse(&[os("-V")]), Ok(Action::Version));
        assert_eq!(
            parse(&[os("--dump-keys"), os("far")]),
            Ok(Action::DumpKeys(Preset::Far))
        );
        assert_eq!(
            parse(&[os("--dump-keys"), os("tc")]),
            Ok(Action::DumpKeys(Preset::Tc))
        );
        assert_eq!(
            parse(&[os("--dump-keys"), os("mc")]),
            Ok(Action::DumpKeys(Preset::Mc))
        );
        assert_eq!(parse(&[]), Ok(Action::Gui));
        assert_eq!(parse(&[os("a"), os("/b")]), Ok(Action::Gui));
        assert!(parse(&[os("--bogus")]).is_err());
        assert!(parse(&[os("--dump-keys")]).is_err());
        assert!(parse(&[os("--dump-keys"), os("norton")]).is_err());
        // A flag wins even with paths around it.
        assert_eq!(parse(&[os("x"), os("--version")]), Ok(Action::Version));
    }

    #[test]
    fn help_mentions_everything() {
        let h = render_help();
        for needle in [
            "Usage:",
            "--version",
            "--dump-keys",
            "LEFT_DIR",
            "RIGHT_DIR",
        ] {
            assert!(h.contains(needle), "help is missing {needle:?}");
        }
    }

    #[test]
    fn dump_keys_covers_all_sections_and_commands() {
        for (preset, name) in [(Preset::Far, "far"), (Preset::Tc, "tc"), (Preset::Mc, "mc")] {
            let md = render_keys(preset);
            assert!(md.starts_with("# OFM key reference"), "{name}");
            assert!(md.contains(&format!("--dump-keys {name}`")));
            for section in [
                "## global",
                "## panels",
                "## cmdline",
                "## viewer",
                "## dialog",
            ] {
                assert!(md.contains(section), "{name} is missing {section}");
            }
            // Every command of the registry appears at least once (the presets bind
            // everything, enforced by a keymap test in ofm-config); several chords may
            // be bound to the same command (e.g. Backspace and Ctrl+PgUp).
            for info in commands::ALL {
                let id = info.id.as_str();
                assert!(md.contains(&format!("`{id}`")), "{name}: {id} missing");
            }
            // No chord is dropped silently: the table row count equals the binding count.
            let keymap = Keymap::preset(preset);
            let bindings: usize = commands::ALL
                .iter()
                .map(|i| keymap.chords_for(i.id).len())
                .sum();
            assert_eq!(
                md.lines().filter(|l| l.starts_with("| ")).count() - 5,
                bindings
            );
        }
        // Spot-check the canonical Far chords users know from the original.
        let far = render_keys(Preset::Far);
        assert!(far.contains("| F5 | Copy | `file.copy` |"), "{far}");
        assert!(far.contains("| F3 | View (directory: calculate size) | `file.view` |"));
    }
}
