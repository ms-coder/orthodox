//! Pure shell-integration logic of the terminal module (ADR-0003): shell detection, argv/env
//! building, the rc wrappers (embedded from `resources/shell/`), hostname acceptance for
//! OSC 7 and the panel→shell sync decision. No GTK types here, so everything is unit-testable;
//! the tests also run the wrappers through the *real* bash/zsh/fish when installed and compare
//! their OSC 7 output against [`ofm_core::cmdline::osc7_encode`] byte for byte.

use std::path::{Path, PathBuf};

/// The embedded bash rc wrapper (fed via `bash --rcfile`, see ADR-0003).
pub const BASH_RC: &str = include_str!("../../resources/shell/bashrc.sh");
/// The embedded zsh rc wrapper (the `.zshrc` of the per-session `ZDOTDIR`, see ADR-0003).
pub const ZSH_RC: &str = include_str!("../../resources/shell/zshrc.zsh");
/// The embedded fish init snippet (fed via `fish --init-command`, see ADR-0003).
pub const FISH_INIT: &str = include_str!("../../resources/shell/fish_init.fish");

/// File name the zsh wrapper is written under inside the session directory.
pub const ZSH_RC_NAME: &str = ".zshrc";
/// File name the bash wrapper is written under inside the session directory.
pub const BASH_RC_NAME: &str = "ofm.bashrc";

/// Which shell a path refers to, by basename (ADR-0003). Only these three get an injected rc
/// wrapper and are therefore sync-capable; everything else spawns plainly with no sync in
/// either direction (OSC 7 arriving from a user rc is still parsed and moves the panel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShellKind {
    /// `bash` — spawned with `--rcfile <wrapper>`.
    Bash,
    /// `zsh` — spawned with `ZDOTDIR=<session dir>` holding the wrapper `.zshrc`.
    Zsh,
    /// `fish` — spawned with `--init-command <snippet>`.
    Fish,
    /// Anything else (sh, dash, csh, …): plain spawn, no injection, no cd sync.
    #[default]
    Plain,
}

impl ShellKind {
    /// Whether OFM injects a `cd` into this shell (needs the OSC 7 wrapper for idle detection).
    pub fn sync_capable(self) -> bool {
        !matches!(self, ShellKind::Plain)
    }

    /// Detect the kind from a shell path's basename. Version suffixes (e.g. `bash5`) and any
    /// other name are [`ShellKind::Plain`]: a wrong injection is worse than no sync.
    pub fn of_path(path: &Path) -> ShellKind {
        match path.file_name().and_then(|n| n.to_str()) {
            Some("bash") => ShellKind::Bash,
            Some("zsh") => ShellKind::Zsh,
            Some("fish") => ShellKind::Fish,
            _ => ShellKind::Plain,
        }
    }
}

/// Resolve the shell to spawn: the configured path when non-empty, else `$SHELL`, else
/// `/bin/sh` (ADR-0003). No file system access — a configured shell that does not exist
/// fails at spawn time and is reported there.
pub fn resolve_shell(configured: &str, env_shell: Option<&str>) -> PathBuf {
    if !configured.is_empty() {
        return PathBuf::from(configured);
    }
    match env_shell {
        Some(s) if !s.is_empty() => PathBuf::from(s),
        // Invariant: /bin/sh exists on every supported system (Ubuntu); documented fallback.
        _ => PathBuf::from("/bin/sh"),
    }
}

/// The argv for spawning the shell of `kind` (ADR-0003):
/// bash `--rcfile <wrapper>`, fish `--init-command <snippet>`, zsh/plain just the shell.
/// `wrapper_dir` is the per-session directory the wrappers were written to; `None` (write
/// failed or not yet done) downgrades every kind to a plain spawn.
pub fn build_argv(shell: &Path, kind: ShellKind, wrapper_dir: Option<&Path>) -> Vec<String> {
    let mut argv = vec![shell.to_string_lossy().into_owned()];
    match (kind, wrapper_dir) {
        (ShellKind::Bash, Some(dir)) => {
            argv.push("--rcfile".into());
            argv.push(dir.join(BASH_RC_NAME).to_string_lossy().into_owned());
        }
        (ShellKind::Fish, _) => {
            // The snippet is plain text; no file needed.
            argv.push("--init-command".into());
            argv.push(FISH_INIT.to_owned());
        }
        _ => {}
    }
    argv
}

/// Extra environment entries for the shell child (VTE adds them to the inherited
/// environment): `TERM=xterm-256color` always, plus `ZDOTDIR` for zsh.
pub fn build_env(kind: ShellKind, wrapper_dir: Option<&Path>) -> Vec<String> {
    let mut envv = vec!["TERM=xterm-256color".to_owned()];
    if kind == ShellKind::Zsh
        && let Some(dir) = wrapper_dir
    {
        envv.push(format!("ZDOTDIR={}", dir.to_string_lossy()));
    }
    envv
}

/// Whether an OSC 7 host is this machine (ADR-0003): empty (how OFM's own wrappers emit it),
/// `localhost`, or the machine's own hostname (case-insensitive; `own` is fetched once off
/// the main thread at start-up and may be unknown, in which case only the first two match).
pub fn accept_host(host: &str, own: Option<&str>) -> bool {
    host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || own.is_some_and(|o| o.eq_ignore_ascii_case(host))
}

/// Outcome of [`should_sync`] — the panel→shell `cd` decision on plain inputs (ADR-0003).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDecision {
    /// Feed `cd_command(dir)`: shell idle and capable, panel local and empty command line,
    /// and the shell's known cwd differs.
    FeedCd,
    /// Shell and panel already agree (or cwd unknown): nothing to do, stale flag cleared.
    AlreadyThere,
    /// A precondition blocks the sync (not idle, not sync-capable, command line in use,
    /// non-local panel). Never inject then; a blocked *location change* marks the shell stale.
    Blocked,
}

/// Decide whether OFM may feed a sync `cd` for a panel at `panel_dir` into a shell whose
/// last OSC 7 cwd is `shell_cwd` (ADR-0003 preconditions, pure so it is unit-testable).
pub fn should_sync(
    state: super::ShellState,
    sync_capable: bool,
    cmdline_has_text: bool,
    panel_dir: Option<&Path>,
    shell_cwd: Option<&Path>,
) -> SyncDecision {
    use super::ShellState;
    if state != ShellState::Idle || !sync_capable || cmdline_has_text {
        return SyncDecision::Blocked;
    }
    let (Some(dir), Some(cwd)) = (panel_dir, shell_cwd) else {
        return SyncDecision::Blocked; // non-local panel or no OSC 7 yet: never inject
    };
    if dir == cwd {
        SyncDecision::AlreadyThere
    } else {
        SyncDecision::FeedCd
    }
}

/// The per-session wrapper directory for this OFM process:
/// `$XDG_RUNTIME_DIR/ofm/terminal-<pid>`, falling back to the temp dir (ADR-0003).
/// Created and populated on a worker thread (invariant 2).
pub fn session_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("ofm")
        .join(format!("terminal-{}", std::process::id()))
}

/// Write the rc wrappers into `dir` and return it (best effort, worker thread only).
/// Failure is logged and returns `None` — the shell then spawns plainly without sync.
#[allow(clippy::disallowed_methods)] // worker thread, not the GTK main thread
pub fn write_wrappers(dir: &Path) -> Option<PathBuf> {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    let write_one = |p: &Path, content: &str| -> std::io::Result<()> {
        fs::write(p, content)?;
        fs::set_permissions(p, fs::Permissions::from_mode(0o600))
    };
    let res = fs::create_dir_all(dir)
        .and_then(|()| fs::set_permissions(dir, fs::Permissions::from_mode(0o700)))
        .and_then(|()| write_one(&dir.join(BASH_RC_NAME), BASH_RC))
        .and_then(|()| write_one(&dir.join(ZSH_RC_NAME), ZSH_RC));
    match res {
        Ok(()) => Some(dir.to_owned()),
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "terminal: writing shell wrappers failed");
            None
        }
    }
}

/// The machine's hostname (for OSC 7 acceptance), fetched on a worker thread (ADR-0003).
/// Prefers `g_get_host_name` (cached by glib, no file I/O); reading `/etc/hostname` is the
/// documented fallback.
pub fn hostname() -> Option<String> {
    // Invariant: glib returns a cached, permanently valid string (never NULL in practice).
    let ptr = unsafe { gtk::glib::ffi::g_get_host_name() };
    if !ptr.is_null() {
        let name = unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
        if !name.is_empty() {
            return Some(name);
        }
    }
    read_hostname_file()
}

#[allow(clippy::disallowed_methods)] // worker thread, not the GTK main thread
fn read_hostname_file() -> Option<String> {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use super::*;
    use crate::terminal::ShellState;
    use ofm_core::cmdline::osc7_encode;
    use std::path::Path;

    fn kind(p: &str) -> ShellKind {
        ShellKind::of_path(Path::new(p))
    }

    #[test]
    fn shell_detection_by_basename() {
        assert_eq!(kind("/bin/bash"), ShellKind::Bash);
        assert_eq!(kind("/usr/bin/zsh"), ShellKind::Zsh);
        assert_eq!(kind("/usr/bin/fish"), ShellKind::Fish);
        assert_eq!(kind("fish"), ShellKind::Fish);
        // Version suffixes, other shells, empty, non-UTF-8: plain, no sync.
        assert_eq!(kind("/bin/bash5"), ShellKind::Plain);
        assert_eq!(kind("/bin/sh"), ShellKind::Plain);
        assert_eq!(kind("/bin/dash"), ShellKind::Plain);
        assert_eq!(kind("/bin/tcsh"), ShellKind::Plain);
        assert_eq!(kind("/bin/BASH"), ShellKind::Plain);
        assert_eq!(kind("/"), ShellKind::Plain);
        use std::os::unix::ffi::OsStrExt;
        let odd = Path::new("/bin").join(std::ffi::OsStr::from_bytes(b"ba\xffsh"));
        assert_eq!(ShellKind::of_path(&odd), ShellKind::Plain);
        assert!(!ShellKind::Plain.sync_capable());
        assert!(ShellKind::Bash.sync_capable());
    }

    #[test]
    fn shell_resolution() {
        assert_eq!(
            resolve_shell("/usr/bin/fish", None),
            PathBuf::from("/usr/bin/fish")
        );
        assert_eq!(
            resolve_shell("", Some("/bin/zsh")),
            PathBuf::from("/bin/zsh")
        );
        assert_eq!(resolve_shell("", Some("")), PathBuf::from("/bin/sh"));
        assert_eq!(resolve_shell("", None), PathBuf::from("/bin/sh"));
    }

    #[test]
    fn argv_and_env_building() {
        let dir = Path::new("/run/user/1000/ofm/terminal-42");
        let bash = build_argv(Path::new("/bin/bash"), ShellKind::Bash, Some(dir));
        assert_eq!(
            bash,
            vec![
                "/bin/bash".to_owned(),
                "--rcfile".into(),
                dir.join(BASH_RC_NAME).display().to_string()
            ]
        );
        assert_eq!(
            build_env(ShellKind::Bash, Some(dir)),
            vec!["TERM=xterm-256color"]
        );

        let zsh = build_argv(Path::new("/bin/zsh"), ShellKind::Zsh, Some(dir));
        assert_eq!(zsh, vec!["/bin/zsh".to_owned()]);
        assert_eq!(
            build_env(ShellKind::Zsh, Some(dir)),
            vec![
                "TERM=xterm-256color".into(),
                format!("ZDOTDIR={}", dir.display())
            ]
        );
        // Without a wrapper dir zsh (and bash) degrade to a plain spawn.
        assert_eq!(build_env(ShellKind::Zsh, None), vec!["TERM=xterm-256color"]);

        let fish = build_argv(Path::new("/usr/bin/fish"), ShellKind::Fish, None);
        assert_eq!(fish.len(), 3);
        assert_eq!(fish[0], "/usr/bin/fish");
        assert_eq!(fish[1], "--init-command");
        assert_eq!(fish[2], FISH_INIT);

        assert_eq!(
            build_argv(Path::new("/bin/sh"), ShellKind::Plain, Some(dir)),
            vec!["/bin/sh".to_owned()]
        );
        // A failed wrapper write downgrades bash to plain as well.
        assert_eq!(
            build_argv(Path::new("/bin/bash"), ShellKind::Bash, None),
            vec!["/bin/bash".to_owned()]
        );
    }

    #[test]
    fn hostname_acceptance() {
        assert!(accept_host("", Some("box")));
        assert!(accept_host("", None));
        assert!(accept_host("localhost", None));
        assert!(accept_host("LocalHost", Some("box")));
        assert!(accept_host("box", Some("box")));
        assert!(accept_host("BOX", Some("box")));
        assert!(!accept_host("other", Some("box")));
        assert!(!accept_host("box", None));
        assert!(!accept_host("box.domain", Some("box")));
    }

    #[test]
    fn sync_decisions() {
        use super::SyncDecision::*;
        let a = Path::new("/tmp/a");
        let b = Path::new("/tmp/b");
        // The happy path.
        assert_eq!(
            should_sync(ShellState::Idle, true, false, Some(a), Some(b)),
            FeedCd
        );
        assert_eq!(
            should_sync(ShellState::Idle, true, false, Some(a), Some(a)),
            AlreadyThere
        );
        // Busy (running command, fed line, or still starting up): never inject.
        assert_eq!(
            should_sync(ShellState::Busy, true, false, Some(a), Some(b)),
            Blocked
        );
        assert_eq!(
            should_sync(ShellState::Exited, true, false, Some(a), Some(b)),
            Blocked
        );
        // No wrapper (unknown shell): parse-always, inject-never.
        assert_eq!(
            should_sync(ShellState::Idle, false, false, Some(a), Some(b)),
            Blocked
        );
        // The user is composing a command: the cd would land in their line.
        assert_eq!(
            should_sync(ShellState::Idle, true, true, Some(a), Some(b)),
            Blocked
        );
        // Non-local panel (archive/list) or shell cwd not yet known (no OSC 7 seen).
        assert_eq!(
            should_sync(ShellState::Idle, true, false, None, Some(b)),
            Blocked
        );
        assert_eq!(
            should_sync(ShellState::Idle, true, false, Some(a), None),
            Blocked
        );
    }

    #[test]
    fn wrapper_texts_follow_the_adr() {
        // bash: interactive guard, user rc, ignorespace, PROMPT_COMMAND prepend, encoder.
        assert!(BASH_RC.contains("case $- in"));
        assert!(BASH_RC.contains("[ -r \"$HOME/.bashrc\" ] && . \"$HOME/.bashrc\""));
        assert!(BASH_RC.contains("HISTCONTROL=\"ignorespace${HISTCONTROL:+:$HISTCONTROL}\""));
        assert!(
            BASH_RC.contains("PROMPT_COMMAND=\"__ofm_osc7${PROMPT_COMMAND:+;$PROMPT_COMMAND}\"")
        );
        // zsh: user rc, HIST_IGNORE_SPACE, precmd/chpwd hooks.
        assert!(ZSH_RC.contains("[[ -o interactive ]] || return 0"));
        assert!(ZSH_RC.contains("[ -r \"$HOME/.zshrc\" ] && . \"$HOME/.zshrc\""));
        assert!(ZSH_RC.contains("setopt HIST_IGNORE_SPACE"));
        assert!(ZSH_RC.contains("precmd_functions+=(__ofm_osc7)"));
        assert!(ZSH_RC.contains("chpwd_functions+=(__ofm_osc7)"));
        // fish: prompt wrap and PWD hook.
        assert!(FISH_INIT.contains("functions -c fish_prompt __ofm_ofm_prompt"));
        assert!(FISH_INIT.contains("__ofm_osc7_pwd --on-variable PWD"));
    }

    // ---- real shells: the wrappers must emit exactly osc7_encode("", path) ------------------------

    fn have(prog: &str) -> bool {
        std::process::Command::new(prog)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Directory names that exercise every branch of the encoders.
    fn nasty_names() -> Vec<std::ffi::OsString> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        [
            "plain123",
            "sp ace",
            "it's",
            "dou\"ble",
            "back\\slash",
            "київ",
            "emoji-😀",
            "a+b:c;d?e#f",
            "per%cent",
            "tab\tand\nnewline",
            "ctrl\x1besc\x07bel",
            "tilde~under_score.dot-dash",
        ]
        .iter()
        .map(|s| OsString::from(s.to_owned()))
        .chain([OsString::from_vec(vec![0xff, b'a', 0xfe])])
        .collect()
    }

    fn run_shell_osc7(shell: &str, args: &[&str], rc: &Path, home: &Path, cwd: &Path) -> String {
        let out = std::process::Command::new(shell)
            .args(args)
            .env("HOME", home)
            .env("OFM_RC", rc)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|e| panic!("{shell}: {e}"));
        assert!(
            out.status.success(),
            "{shell} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn assert_wrapper_matches_osc7(shell: &str, args: &[&str], rc_content: &str, rc_ext: &str) {
        let dir = ofm_testkit::temp::TempDir::in_dir(std::env::temp_dir().as_path()).unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let rc = dir.path().join(format!("wrapper{rc_ext}"));
        std::fs::write(&rc, rc_content).unwrap();
        for name in nasty_names() {
            let target = dir.path().join(&name);
            std::fs::create_dir(&target).unwrap();
            let got = run_shell_osc7(shell, args, &rc, &home, &target);
            let want = format!("\x1b]7;{}\x1b\\", osc7_encode("", &target));
            assert_eq!(got, want, "{shell}: wrong OSC 7 for {:?}", name);
            std::fs::remove_dir(&target).unwrap();
        }
    }

    #[test]
    fn bash_wrapper_encodes_like_osc7() {
        if !have("bash") {
            eprintln!("skipping: bash not installed");
            return;
        }
        assert_wrapper_matches_osc7(
            "bash",
            &["-c", "source \"$OFM_RC\"; __ofm_osc7"],
            BASH_RC,
            ".sh",
        );
    }

    #[test]
    fn zsh_wrapper_encodes_like_osc7() {
        if !have("zsh") {
            eprintln!("skipping: zsh not installed");
            return;
        }
        assert_wrapper_matches_osc7(
            "zsh",
            &["-c", "source \"$OFM_RC\"; __ofm_osc7"],
            ZSH_RC,
            ".zsh",
        );
    }

    #[test]
    fn fish_wrapper_encodes_like_osc7() {
        if !have("fish") {
            eprintln!("skipping: fish not installed");
            return;
        }
        // -C evaluates before the first prompt; `source` keeps the same code path as
        // `--init-command <contents>` used by the spawn.
        assert_wrapper_matches_osc7(
            "fish",
            &["-C", "source $OFM_RC", "-c", "__ofm_osc7"],
            FISH_INIT,
            ".fish",
        );
    }
}
