//! Enter on a file (F31): `panel.open` and `file.open_with` (Shift+Enter).
//!
//! * A regular file with an execute bit that starts with `#!` (script) or `\x7fELF` (program)
//!   runs in the embedded terminal as its own child process (`TerminalHandle::run_program`:
//!   exact argv, no shell line typed, working directory = the file's folder). The wrapper keeps
//!   the terminal on screen after the program ends until Enter is pressed, so its output can be
//!   read; the panels come back then.
//! * Everything else (documents, scripts without the execute bit, executable-bit text files on
//!   FAT/NTFS mounts, ...) opens with the default application through `gtk::FileLauncher`
//!   (portal-aware, asynchronous).
//!
//! Deciding needs the mode bits and the first bytes, so the probe runs on the I/O pool; the GTK
//! thread never touches the file. Failures (cannot start, no application) are shown in a message
//! dialog and never crash.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::gio;
use ofm_core::{Location, cmd};

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::i18n::tr;
use crate::jobs::ui;
use crate::terminal;
use crate::wiring;

/// What Enter does with a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAction {
    /// Run it in the terminal.
    Run,
    /// Open it with the default application.
    Default,
}

/// Decide from the execute bit and the first bytes of the file (`None` = could not be read).
///
/// An executable file is run when it is a script (`#!`) or an ELF program; executable-bit files
/// that are neither (text on FAT/NTFS, data files with sloppy modes) open with the default
/// application, because the kernel could not run them anyway. An unreadable file with the
/// execute bit is run (execute-only programs exist; the kernel decides).
pub fn classify(exec_bit: bool, head: Option<&[u8]>) -> OpenAction {
    if !exec_bit {
        return OpenAction::Default;
    }
    match head {
        None => OpenAction::Run,
        Some(h) if h.starts_with(b"#!") || h.starts_with(b"\x7fELF") => OpenAction::Run,
        Some(_) => OpenAction::Default,
    }
}

/// Result of the worker's look at a file.
#[derive(Debug)]
struct Probe {
    is_file: bool,
    exec_bit: bool,
    head: Option<Vec<u8>>,
}

/// Mode bits and first four bytes (worker thread only).
#[allow(clippy::disallowed_methods)] // runs on the I/O pool, never on the GTK thread
fn probe(path: &Path) -> Probe {
    use std::io::Read;
    let Ok(meta) = std::fs::metadata(path) else {
        return Probe {
            is_file: false,
            exec_bit: false,
            head: None,
        };
    };
    let head = std::fs::File::open(path).ok().map(|f| {
        let mut buf = Vec::with_capacity(4);
        // A short read (small file) is fine; errors give what was read so far.
        let _ = f.take(4).read_to_end(&mut buf);
        buf
    });
    Probe {
        is_file: meta.is_file(),
        exec_bit: meta.permissions().mode() & 0o111 != 0,
        head,
    }
}

/// The argument vector that runs `program` and then waits for Enter (see the module docs). The
/// program path is passed as its own word, never spliced into the script text.
pub fn run_argv(program: &Path) -> Vec<OsString> {
    const SCRIPT: &str = "e=$1; k=$2; p=$3; \"$p\"; s=$?; \
                          printf '\\n[%s %s] %s ' \"$e\" \"$s\" \"$k\"; IFS= read -r _";
    vec![
        OsString::from("/bin/sh"),
        OsString::from("-c"),
        OsString::from(SCRIPT),
        OsString::from("sh"),
        OsString::from(tr("Process exited with code")),
        OsString::from(tr("Press Enter to return to OFM.")),
        program.as_os_str().to_owned(),
    ]
}

/// Register `file.open_with`; `panel.open` lives in `navigation::goto` and calls [`open_path`].
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::FILE_OPEN_WITH, open_with);
}

/// Enter on the file `path`: run it or open it (decided on a worker, then acted on here).
pub fn open_path(ctx: &Rc<AppContext>, path: PathBuf) {
    let probe_path = path.clone();
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let _ = tx.send_blocking(probe(&probe_path));
    });
    let weak = Rc::downgrade(ctx);
    wiring::attach(
        rx,
        move |p| {
            if let Some(ctx) = weak.upgrade() {
                if p.is_file && classify(p.exec_bit, p.head.as_deref()) == OpenAction::Run {
                    run_in_terminal(&ctx, &path);
                } else {
                    launch_default(&ctx, path.clone(), false);
                }
            }
            std::ops::ControlFlow::Break(())
        },
        || {},
    );
}

/// Run `path` in the embedded terminal; the panels return when it ends.
fn run_in_terminal(ctx: &Rc<AppContext>, path: &Path) {
    let Some(handle) = terminal::handle(ctx) else {
        ui::message(ctx, tr("Run"), tr("The terminal is not available."));
        return;
    };
    let cwd = path.parent().map(Path::to_owned);
    let weak = Rc::downgrade(ctx);
    let result = handle.run_program(&run_argv(path), cwd.as_deref(), move |msg| {
        if let Some(ctx) = weak.upgrade() {
            ui::message(&ctx, tr("Run"), &msg);
        }
    });
    if let Err(msg) = result {
        ui::message(ctx, tr("Run"), &msg);
    }
}

/// Open `path` with the default application (`ask`: let the user choose one first). Asynchronous;
/// a failure is reported in a message dialog, dismissing the chooser is not a failure.
pub fn launch_default(ctx: &Rc<AppContext>, path: PathBuf, ask: bool) {
    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&path)));
    launcher.set_always_ask(ask);
    let parent = ctx.window().map(|w| w.window.clone());
    let weak = Rc::downgrade(ctx);
    gtk::glib::spawn_future_local(async move {
        let res = launcher.launch_future(parent.as_ref()).await;
        if let Err(e) = res
            && !e.matches(gtk::DialogError::Dismissed)
        {
            tracing::warn!(path = %path.display(), error = %e, "could not open file");
            if let Some(ctx) = weak.upgrade() {
                ui::message(
                    &ctx,
                    tr("Open"),
                    &format!(
                        "{} {}: {e}",
                        tr("Cannot open"),
                        path.file_name()
                            .unwrap_or(path.as_os_str())
                            .to_string_lossy()
                    ),
                );
            }
        }
    });
}

/// The regular file under the cursor of the active panel (local or panelized), if any.
fn cursor_file(ctx: &AppContext) -> Option<PathBuf> {
    let ctrl = ctx.active_controller();
    let entry = ctrl.model().cursor_entry().cloned()?;
    if entry.kind.is_dir_like() || entry.name == ".." {
        return None;
    }
    match (ctrl.location(), &entry.real_path) {
        (Location::Local(dir), _) => Some(dir.join(&entry.name)),
        (Location::List { .. }, Some(real)) => Some(real.clone()),
        _ => None,
    }
}

/// `file.open_with` (Shift+Enter): choose the application, then open.
fn open_with(ctx: &Rc<AppContext>) {
    match cursor_file(ctx) {
        Some(path) => launch_default(ctx, path, true),
        None => tracing::debug!("open with: no local file under the cursor"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use std::os::unix::ffi::OsStrExt;

    use super::*;

    #[test]
    fn executable_scripts_and_programs_run() {
        assert_eq!(classify(true, Some(b"#!/b")), OpenAction::Run);
        assert_eq!(classify(true, Some(b"\x7fELF")), OpenAction::Run);
        assert_eq!(classify(true, None), OpenAction::Run);
    }

    #[test]
    fn everything_else_opens_with_the_default_app() {
        // No execute bit: even a script opens as a document.
        assert_eq!(classify(false, Some(b"#!/b")), OpenAction::Default);
        assert_eq!(classify(false, Some(b"\x7fELF")), OpenAction::Default);
        assert_eq!(classify(false, None), OpenAction::Default);
        // Execute bit on text (FAT/NTFS mounts): not runnable.
        assert_eq!(classify(true, Some(b"hell")), OpenAction::Default);
        assert_eq!(classify(true, Some(b"")), OpenAction::Default);
        assert_eq!(classify(true, Some(b"#")), OpenAction::Default);
    }

    #[test]
    fn probe_reads_mode_and_head() {
        let dir = ofm_testkit::temp::TempDir::new().unwrap();
        let script = dir.path().join("s.sh");
        std::fs::write(&script, b"#!/bin/sh\necho hi\n").unwrap();
        let plain = dir.path().join("p.txt");
        std::fs::write(&plain, b"x").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
        let p = probe(&script);
        assert!(p.is_file && p.exec_bit);
        assert_eq!(p.head.as_deref(), Some(&b"#!/b"[..]));
        assert_eq!(classify(p.exec_bit, p.head.as_deref()), OpenAction::Run);
        let p = probe(&plain);
        assert!(!p.exec_bit);
        assert_eq!(p.head.as_deref(), Some(&b"x"[..]));
        let p = probe(&dir.path().join("missing"));
        assert!(!p.is_file);
        let p = probe(dir.path());
        assert!(!p.is_file);
    }

    /// The wrapper runs the program with the exact path word (spaces, quotes, non-UTF-8 bytes)
    /// and then waits for Enter on stdin.
    #[test]
    fn run_argv_passes_the_path_verbatim() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let dir = ofm_testkit::temp::TempDir::new().unwrap();
        let name = std::ffi::OsStr::from_bytes(b"we ird'\"$x\xff.sh");
        let script = dir.path().join(name);
        let marker = dir.path().join("marker");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s' \"$0\" > '{}'\nexit 7\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let argv = run_argv(&script);
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"\n").unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(text.contains("] ") && text.contains(" 7]"), "{text}");
        assert_eq!(
            std::fs::read(&marker).unwrap(),
            script.as_os_str().as_bytes()
        );
    }
}
