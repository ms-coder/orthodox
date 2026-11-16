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
/// The embedded zsh `.zshenv` passthrough (sources the user's `.zshenv`, keeps the wrapper dir).
pub const ZSH_ENV: &str = include_str!("../../resources/shell/zshenv.zsh");
/// The embedded zsh `.zprofile` passthrough (sources the user's `.zprofile`).
pub const ZSH_PROFILE: &str = include_str!("../../resources/shell/zprofile.zsh");
/// The embedded fish init snippet (fed via `fish --init-command`, see ADR-0003).
pub const FISH_INIT: &str = include_str!("../../resources/shell/fish_init.fish");

/// File name the zsh wrapper is written under inside the session directory.
pub const ZSH_RC_NAME: &str = ".zshrc";
/// File name of the zsh `.zshenv` passthrough inside the session directory.
pub const ZSH_ENV_NAME: &str = ".zshenv";
/// File name of the zsh `.zprofile` passthrough inside the session directory.
pub const ZSH_PROFILE_NAME: &str = ".zprofile";
/// File name the bash wrapper is written under inside the session directory.
pub const BASH_RC_NAME: &str = "ofm.bashrc";

/// Which shell a path refers to, by basename (ADR-0003). Only these three get an injected rc
/// wrapper and are therefore sync-capable; everything else spawns plainly with no sync in
/// either direction (OSC 7 arriving from a user rc is still parsed and moves the panel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShellKind {
    /// `bash` — spawned with `--rcfile <wrapper>`.
    Bash,
    /// `zsh` — spawned with `ZDOTDIR=<session dir>` holding the wrapper `.zshenv`,
    /// `.zprofile` and `.zshrc` (each sources the user's file from `$OFM_ORIG_ZDOTDIR`/`$HOME`).
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

    /// How names must be quoted for this shell (fish is not POSIX, see
    /// [`ofm_core::cmdline::QuoteStyle`]).
    pub fn quote_style(self) -> ofm_core::cmdline::QuoteStyle {
        match self {
            ShellKind::Fish => ofm_core::cmdline::QuoteStyle::Fish,
            _ => ofm_core::cmdline::QuoteStyle::Posix,
        }
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
/// environment): `TERM=xterm-256color` always, plus for zsh `ZDOTDIR=<wrapper dir>` and
/// `OFM_ORIG_ZDOTDIR=<the user's own ZDOTDIR, or empty>` (`orig_zdotdir`) — zsh reads
/// `.zshenv`/`.zprofile`/`.zshrc` from `ZDOTDIR`, so the wrapper files source the user's
/// from `${OFM_ORIG_ZDOTDIR:-$HOME}` and restore `ZDOTDIR` before the user's `.zshrc` runs.
pub fn build_env(
    kind: ShellKind,
    wrapper_dir: Option<&Path>,
    orig_zdotdir: Option<&str>,
) -> Vec<String> {
    let mut envv = vec!["TERM=xterm-256color".to_owned()];
    if kind == ShellKind::Zsh
        && let Some(dir) = wrapper_dir
    {
        envv.push(format!("ZDOTDIR={}", dir.to_string_lossy()));
        envv.push(format!("OFM_ORIG_ZDOTDIR={}", orig_zdotdir.unwrap_or("")));
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
    /// half-typed terminal line, non-local panel). Never inject then; a blocked *location change* marks the shell stale.
    Blocked,
}

/// Decide whether OFM may feed a sync `cd` for a panel at `panel_dir` into a shell whose
/// last OSC 7 cwd is `shell_cwd` (ADR-0003 preconditions, pure so it is unit-testable).
/// `terminal_line_dirty`: the user typed something into the terminal itself that has not been
/// submitted (see [`line_dirty_after`]); a fed `cd` would be appended to that half-typed line.
pub fn should_sync(
    state: super::ShellState,
    sync_capable: bool,
    cmdline_has_text: bool,
    terminal_line_dirty: bool,
    panel_dir: Option<&Path>,
    shell_cwd: Option<&Path>,
) -> SyncDecision {
    use super::ShellState;
    if state != ShellState::Idle || !sync_capable || cmdline_has_text || terminal_line_dirty {
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

/// Whether the user's half-typed terminal line may still hold text after the VTE widget
/// committed `committed` to the child (`prev` = state before). Set by anything other than a
/// bare Enter; cleared by Enter (`\r`, the line is submitted) and Ctrl-C (`\x03`, the line is
/// abandoned). The next OSC 7 (a fresh prompt) clears it too — done by the caller.
pub fn line_dirty_after(prev: bool, committed: &str) -> bool {
    if committed.contains('\r') || committed.contains('\x03') {
        false
    } else if committed.is_empty() {
        prev
    } else {
        true
    }
}

/// How fast and how often the shell may exit before OFM stops respawning it (ADR-0003):
/// [`RespawnGuard::MAX_EXITS`] exits within [`RespawnGuard::WINDOW`] means the shell cannot
/// start (broken rc, bad `terminal.shell`), and a respawn loop would spin the CPU.
#[derive(Debug, Default)]
pub struct RespawnGuard {
    exits: Vec<std::time::Instant>,
}

impl RespawnGuard {
    /// Number of exits inside [`RespawnGuard::WINDOW`] that halts respawning.
    pub const MAX_EXITS: usize = 3;
    /// The sliding window.
    pub const WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

    /// Record a shell exit at `now`. `true` = too many exits in too short a time: stop
    /// respawning (the history is cleared, so the next manual restart starts fresh).
    pub fn record_exit(&mut self, now: std::time::Instant) -> bool {
        self.exits
            .retain(|t| now.saturating_duration_since(*t) <= Self::WINDOW);
        self.exits.push(now);
        if self.exits.len() >= Self::MAX_EXITS {
            self.exits.clear();
            true
        } else {
            false
        }
    }

    /// Forget the history (a manual restart).
    pub fn reset(&mut self) {
        self.exits.clear();
    }
}

/// Base directory for session directories: `$XDG_RUNTIME_DIR/ofm` (per-user, mode 0700,
/// tmpfs), falling back to the shared temp dir when it is unset (ADR-0003).
pub fn session_base() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d).join("ofm"),
        None => std::env::temp_dir(),
    }
}

/// A process-unique, hard-to-guess suffix for session directory names.
fn unique_suffix() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    format!("{:016x}", h.finish())
}

/// Create a fresh session directory under `base` (mkdtemp style): mode 0700, a name nobody can
/// predict, created with a plain `mkdir` so a pre-existing entry — including a symlink planted
/// in a shared temp dir — makes it fail and retry with another name instead of being followed.
#[allow(clippy::disallowed_methods)] // worker thread, not the GTK main thread
pub fn create_session_dir(base: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    // The base itself (`$XDG_RUNTIME_DIR/ofm`) may be new; the temp dir always exists.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(base)?;
    let mut last = None;
    for _ in 0..32 {
        let dir = base.join(format!(
            "ofm-terminal-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no free session directory name")))
}

/// Create a new file `path` (mode 0600, `O_EXCL`, `O_NOFOLLOW`) holding `content`; never
/// follows or overwrites anything that already exists there.
pub fn write_private_file(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    f.write_all(content.as_bytes())?;
    f.sync_all()
}

/// Create a session directory under `base` and write the rc wrappers into it (best effort,
/// worker thread only). Failure is logged, the half-written directory removed, and `None`
/// returned — the shell then spawns plainly without sync.
#[allow(clippy::disallowed_methods)] // worker thread, not the GTK main thread
pub fn write_wrappers(base: &Path) -> Option<PathBuf> {
    let dir = match create_session_dir(base) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(base = %base.display(), error = %e, "terminal: creating the session directory failed");
            return None;
        }
    };
    let res = write_private_file(&dir.join(BASH_RC_NAME), BASH_RC)
        .and_then(|()| write_private_file(&dir.join(ZSH_ENV_NAME), ZSH_ENV))
        .and_then(|()| write_private_file(&dir.join(ZSH_PROFILE_NAME), ZSH_PROFILE))
        .and_then(|()| write_private_file(&dir.join(ZSH_RC_NAME), ZSH_RC));
    match res {
        Ok(()) => Some(dir),
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "terminal: writing shell wrappers failed");
            let _ = std::fs::remove_dir_all(&dir);
            None
        }
    }
}

/// Remove a session directory on a detached thread (invariant 2: never on the main thread).
/// Best effort: the wrappers are only read at shell start, and a leftover lives in
/// `$XDG_RUNTIME_DIR`, which the system clears at logout.
pub fn remove_session_dir_async(dir: PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("ofm-term-cleanup".into())
        .spawn(move || {
            #[allow(clippy::disallowed_methods)] // dedicated worker thread
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                tracing::debug!(dir = %dir.display(), error = %e, "terminal: removing the session directory failed");
            }
        });
    if let Err(e) = spawned {
        tracing::debug!(error = %e, "terminal: could not start the cleanup thread");
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
            build_env(ShellKind::Bash, Some(dir), None),
            vec!["TERM=xterm-256color"]
        );

        let zsh = build_argv(Path::new("/bin/zsh"), ShellKind::Zsh, Some(dir));
        assert_eq!(zsh, vec!["/bin/zsh".to_owned()]);
        assert_eq!(
            build_env(ShellKind::Zsh, Some(dir), None),
            vec![
                "TERM=xterm-256color".into(),
                format!("ZDOTDIR={}", dir.display()),
                "OFM_ORIG_ZDOTDIR=".into()
            ]
        );
        // The user's own ZDOTDIR is handed to the wrappers.
        assert_eq!(
            build_env(ShellKind::Zsh, Some(dir), Some("/home/u/.config/zsh")),
            vec![
                "TERM=xterm-256color".into(),
                format!("ZDOTDIR={}", dir.display()),
                "OFM_ORIG_ZDOTDIR=/home/u/.config/zsh".into()
            ]
        );
        // Without a wrapper dir zsh (and bash) degrade to a plain spawn.
        assert_eq!(
            build_env(ShellKind::Zsh, None, None),
            vec!["TERM=xterm-256color"]
        );

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
            should_sync(ShellState::Idle, true, false, false, Some(a), Some(b)),
            FeedCd
        );
        assert_eq!(
            should_sync(ShellState::Idle, true, false, false, Some(a), Some(a)),
            AlreadyThere
        );
        // Busy (running command, fed line, or still starting up): never inject.
        assert_eq!(
            should_sync(ShellState::Busy, true, false, false, Some(a), Some(b)),
            Blocked
        );
        assert_eq!(
            should_sync(ShellState::Exited, true, false, false, Some(a), Some(b)),
            Blocked
        );
        // No wrapper (unknown shell): parse-always, inject-never.
        assert_eq!(
            should_sync(ShellState::Idle, false, false, false, Some(a), Some(b)),
            Blocked
        );
        // The user is composing a command: the cd would land in their line.
        assert_eq!(
            should_sync(ShellState::Idle, true, true, false, Some(a), Some(b)),
            Blocked
        );
        // Half-typed line in the terminal itself: the cd would be appended to it.
        assert_eq!(
            should_sync(ShellState::Idle, true, false, true, Some(a), Some(b)),
            Blocked
        );
        // Non-local panel (archive/list) or shell cwd not yet known (no OSC 7 seen).
        assert_eq!(
            should_sync(ShellState::Idle, true, false, false, None, Some(b)),
            Blocked
        );
        assert_eq!(
            should_sync(ShellState::Idle, true, false, false, Some(a), None),
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
        // zsh: user rc from the original ZDOTDIR/HOME, ZDOTDIR restored, HIST_IGNORE_SPACE,
        // precmd/chpwd hooks; .zshenv/.zprofile passthrough.
        assert!(ZSH_RC.contains("[[ -o interactive ]] || return 0"));
        assert!(ZSH_RC.contains("__ofm_home=${OFM_ORIG_ZDOTDIR:-$HOME}"));
        assert!(ZSH_RC.contains("[ -r \"$__ofm_home/.zshrc\" ] && . \"$__ofm_home/.zshrc\""));
        assert!(ZSH_ENV.contains("${OFM_ORIG_ZDOTDIR:-$HOME}/.zshenv"));
        assert!(ZSH_PROFILE.contains("${OFM_ORIG_ZDOTDIR:-$HOME}/.zprofile"));
        assert!(ZSH_RC.contains("setopt HIST_IGNORE_SPACE"));
        assert!(ZSH_RC.contains("precmd_functions+=(__ofm_osc7)"));
        assert!(ZSH_RC.contains("chpwd_functions+=(__ofm_osc7)"));
        // fish: prompt wrap and PWD hook.
        assert!(FISH_INIT.contains("functions -c fish_prompt __ofm_ofm_prompt"));
        assert!(FISH_INIT.contains("__ofm_osc7_pwd --on-variable PWD"));
    }

    #[test]
    fn terminal_line_dirty_tracking() {
        // Typed text leaves a half-typed line; Enter and Ctrl-C clear it.
        assert!(line_dirty_after(false, "l"));
        assert!(line_dirty_after(true, "s"));
        assert!(line_dirty_after(false, "\x1b[A")); // history recall
        assert!(!line_dirty_after(true, "\r"));
        assert!(!line_dirty_after(true, "ls\r"));
        assert!(!line_dirty_after(true, "\x03"));
        assert!(!line_dirty_after(false, "\r"));
        // An empty commit changes nothing.
        assert!(line_dirty_after(true, ""));
        assert!(!line_dirty_after(false, ""));
    }

    #[test]
    fn respawn_guard_stops_a_tight_exit_loop() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut g = RespawnGuard::default();
        assert!(!g.record_exit(ms(0)));
        assert!(!g.record_exit(ms(300)));
        assert!(
            g.record_exit(ms(600)),
            "3 exits within 2 s halt the respawns"
        );
        // History cleared: a manual restart that exits again starts counting afresh.
        assert!(!g.record_exit(ms(700)));
        // Slow exits (a shell the user quits now and then) never trigger it.
        let mut g = RespawnGuard::default();
        for i in 0..10 {
            assert!(!g.record_exit(ms(i * 5_000)), "exit {i}");
        }
        // Two quick exits, then a long pause, then one: still fine.
        let mut g = RespawnGuard::default();
        assert!(!g.record_exit(ms(0)));
        assert!(!g.record_exit(ms(100)));
        assert!(!g.record_exit(ms(10_000)));
        g.reset();
        assert!(!g.record_exit(ms(10_100)));
    }

    #[test]
    fn quote_style_follows_the_shell() {
        use ofm_core::cmdline::QuoteStyle;
        assert_eq!(ShellKind::Fish.quote_style(), QuoteStyle::Fish);
        for k in [ShellKind::Bash, ShellKind::Zsh, ShellKind::Plain] {
            assert_eq!(k.quote_style(), QuoteStyle::Posix);
        }
    }

    // ---- session directory and wrapper files -------------------------------------------------

    fn mode_of(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn session_dirs_are_unique_private_and_hold_all_wrappers() {
        let base = ofm_testkit::temp::TempDir::in_dir(std::env::temp_dir().as_path()).unwrap();
        let a = write_wrappers(base.path()).unwrap();
        let b = write_wrappers(base.path()).unwrap();
        assert_ne!(a, b, "every session gets its own directory");
        assert_eq!(mode_of(&a), 0o700);
        for name in [BASH_RC_NAME, ZSH_ENV_NAME, ZSH_PROFILE_NAME, ZSH_RC_NAME] {
            assert_eq!(mode_of(&a.join(name)), 0o600, "{name}");
        }
        assert_eq!(
            std::fs::read_to_string(a.join(ZSH_ENV_NAME)).unwrap(),
            ZSH_ENV
        );
        // The name is not the predictable `terminal-<pid>` of the old fallback.
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("ofm-terminal-"), "{name}");
        assert_ne!(name, format!("terminal-{}", std::process::id()));
    }

    #[test]
    fn a_planted_symlink_is_never_followed() {
        let base = ofm_testkit::temp::TempDir::in_dir(std::env::temp_dir().as_path()).unwrap();
        let victim = base.path().join("victim");
        std::fs::write(&victim, "precious").unwrap();
        // A file the wrapper writer wants to create, pre-planted as a symlink to the victim.
        let dir = create_session_dir(base.path()).unwrap();
        let target = dir.join(BASH_RC_NAME);
        std::os::unix::fs::symlink(&victim, &target).unwrap();
        assert!(write_private_file(&target, "overwritten").is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
        // Same for an existing regular file: create_new, never truncate.
        assert!(write_private_file(&victim, "x").is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
        // A directory name collision is an error for mkdir, not a reuse: simulate by creating
        // the dir again with the same path.
        use std::os::unix::fs::DirBuilderExt;
        assert!(std::fs::DirBuilder::new().mode(0o700).create(&dir).is_err());
    }

    #[test]
    fn session_dir_creation_in_a_missing_base_makes_it_private() {
        let root = ofm_testkit::temp::TempDir::in_dir(std::env::temp_dir().as_path()).unwrap();
        let base = root.path().join("run/ofm");
        let dir = create_session_dir(&base).unwrap();
        assert!(dir.starts_with(&base));
        assert_eq!(mode_of(&dir), 0o700);
    }

    #[test]
    fn session_dir_removal_runs_off_thread_and_deletes() {
        let base = ofm_testkit::temp::TempDir::in_dir(std::env::temp_dir().as_path()).unwrap();
        let dir = write_wrappers(base.path()).unwrap();
        remove_session_dir_async(dir.clone());
        for _ in 0..200 {
            if !dir.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("session dir {} still exists", dir.display());
    }

    // ---- real zsh: passthrough files, ZDOTDIR restore ---------------------------------------------

    /// Run an interactive (optionally login) zsh through the wrapper dir exactly like the spawn
    /// does (`ZDOTDIR=<wrapper>`, `OFM_ORIG_ZDOTDIR=<orig>`) with a fake `$HOME`, and return its
    /// stdout.
    fn run_wrapped_zsh(
        wrapper: &Path,
        home: &Path,
        orig: Option<&Path>,
        login: bool,
        cmd: &str,
    ) -> String {
        let mut c = std::process::Command::new("zsh");
        if login {
            c.arg("-l");
        }
        let out = c
            .args(["-i", "-c", cmd])
            .env_remove("ZDOTDIR")
            .env("HOME", home)
            .env("ZDOTDIR", wrapper)
            .env(
                "OFM_ORIG_ZDOTDIR",
                orig.map(Path::as_os_str).unwrap_or_default(),
            )
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn zsh_wrapper_sources_the_users_files_and_restores_zdotdir() {
        if !have("zsh") {
            eprintln!("skipping: zsh not installed");
            return;
        }
        let t = ofm_testkit::temp::TempDir::in_dir(std::env::temp_dir().as_path()).unwrap();
        let wrapper = write_wrappers(t.path()).unwrap();
        let w = |dir: &Path, name: &str, content: &str| {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(name), content).unwrap();
        };

        // 1. No ZDOTDIR of their own: all three user files under $HOME run, ZDOTDIR is gone
        //    again and the OFM hooks are installed.
        let home = t.path().join("home");
        w(&home, ".zshenv", "export U_ENV=1\n");
        w(&home, ".zprofile", "export U_PROFILE=1\n");
        w(&home, ".zshrc", "export U_RC=1 U_RC_ZD=${ZDOTDIR-unset}\n");
        let out = run_wrapped_zsh(
            &wrapper,
            &home,
            None,
            true,
            "print -r -- \"env=$U_ENV profile=$U_PROFILE rc=$U_RC zd=${ZDOTDIR-unset} orig=${OFM_ORIG_ZDOTDIR-unset} hook=${precmd_functions[(r)__ofm_osc7]}\"",
        );
        assert!(
            out.contains("env=1 profile=1 rc=1 zd=unset orig=unset hook=__ofm_osc7"),
            "{out:?}"
        );
        // The user's .zshrc already saw their own (unset) ZDOTDIR, not the wrapper dir.
        let out = run_wrapped_zsh(&wrapper, &home, None, false, "print -r -- $U_RC_ZD");
        assert!(out.trim().ends_with("unset"), "{out:?}");

        // 2. The user's own ZDOTDIR from the environment (OFM_ORIG_ZDOTDIR).
        let zd = t.path().join("zd");
        w(&zd, ".zshenv", "export U_ENV=2\n");
        w(&zd, ".zprofile", "export U_PROFILE=2\n");
        w(&zd, ".zshrc", "export U_RC=2\n");
        let out = run_wrapped_zsh(
            &wrapper,
            &home,
            Some(&zd),
            true,
            "print -r -- \"env=$U_ENV profile=$U_PROFILE rc=$U_RC zd=$ZDOTDIR hook=${precmd_functions[(r)__ofm_osc7]}\"",
        );
        assert!(
            out.contains(&format!(
                "env=2 profile=2 rc=2 zd={} hook=__ofm_osc7",
                zd.display()
            )),
            "{out:?}"
        );

        // 3. The user's ~/.zshenv sets ZDOTDIR itself (very common): still our .zshrc runs
        //    and theirs is read from the new location.
        let cfg = t.path().join("cfg");
        let home3 = t.path().join("home3");
        w(
            &home3,
            ".zshenv",
            &format!("export ZDOTDIR={}\n", cfg.display()),
        );
        w(&cfg, ".zshrc", "export U_RC=3\n");
        let out = run_wrapped_zsh(
            &wrapper,
            &home3,
            None,
            false,
            "print -r -- \"rc=$U_RC zd=$ZDOTDIR hook=${precmd_functions[(r)__ofm_osc7]}\"",
        );
        assert!(
            out.contains(&format!("rc=3 zd={} hook=__ofm_osc7", cfg.display())),
            "{out:?}"
        );
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
