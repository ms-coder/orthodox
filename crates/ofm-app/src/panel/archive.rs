//! Archives as folders, UI side (T6.3, F26, ADR-0008).
//!
//! * [`ArchiveService`] (in `Services::archives`) owns the one `ArchiveVfs` (registered for
//!   `Location::Archive`), the app's password provider and the private temp directories used by
//!   F3/quick view. Cheap to clone.
//! * [`open_file`] is what Enter / Ctrl+PgDn on a regular file does: magic bytes decide (on the I/O
//!   pool, never the extension), the index is built on a worker with a "Reading archive..." state
//!   that Esc cancels, and only then does the panel navigate into `Location::Archive`. A failure
//!   shows a message and the panel stays where it was; a file that is not an archive falls back to
//!   the normal Enter behaviour.
//! * [`intercept`] is called by the dispatcher before every command: it refuses write operations
//!   in an archive panel ("Archives are read-only"), turns F5/F6 into extraction and F3/F4 into a
//!   read-only view of the entry, and lets Esc cancel a running open. Pure decisions live in
//!   [`refusal`] (unit-tested).
//!
//! Sub-modules of this feature: `archive_extract` (extract dialog, view, sizes).

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ofm_core::{CancelToken, CommandId, Entry, Location, LocationKind, cmd};
use ofm_vfs::archive::{
    ArchiveError, ArchiveVfs, IndexWarnings, LimitKind, PasswordSource, detect_format,
};
use ofm_vfs::{Caps, ListEvent, ReadSeek, SpaceInfo, Vfs, VfsError, VfsRegistry};

use crate::compare::message;
use crate::context::AppContext;
use crate::dialogs::password::{self, AppPasswords};
use crate::i18n::tr;
use crate::jobs::ui;
use crate::navigation::goto;
use crate::panel::PanelController;
use crate::wiring;

/// Flag in `Entry::mode` (above the 16 bits of `st_mode`) of an archive entry whose data is
/// encrypted: the panel draws a lock marker. Set by the archive provider wrapper only.
pub const ARCHIVE_ENCRYPTED: u32 = 1 << 20;

/// "Reading archive..." appears only when the open takes longer than this (small archives open
/// without flicker).
const SHOW_BUSY_AFTER: Duration = Duration::from_millis(250);

// ---- service ----------------------------------------------------------------------------------

/// The archive provider and what goes with it. Created by `Services::new`.
#[derive(Clone)]
pub struct ArchiveService {
    vfs: Arc<ArchiveVfs>,
    passwords: PasswordSource,
    prompts: Arc<AppPasswords>,
    temps: Arc<Mutex<Vec<PathBuf>>>,
}

impl ArchiveService {
    /// Service with the default limits and a password provider that asks through the GTK thread
    /// once the UI attached to it (`dialogs::password::install`).
    pub fn new() -> Self {
        let prompts = Arc::new(AppPasswords::new());
        let passwords = PasswordSource::new(prompts.clone());
        ArchiveService {
            vfs: Arc::new(ArchiveVfs::new(passwords.clone())),
            passwords,
            prompts,
            temps: Arc::default(),
        }
    }

    /// Register the provider for `Location::Archive` in `registry`.
    pub fn register(&self, registry: &mut VfsRegistry) {
        registry.register(
            LocationKind::Archive,
            Arc::new(ScopedVfs(Arc::clone(&self.vfs))),
        );
    }

    /// The provider (index access for workers).
    pub fn vfs(&self) -> &Arc<ArchiveVfs> {
        &self.vfs
    }

    /// The password source shared by browsing and extract jobs (one question per archive).
    pub fn passwords(&self) -> &PasswordSource {
        &self.passwords
    }

    /// The app's password provider (tests script it).
    pub fn prompts(&self) -> &Arc<AppPasswords> {
        &self.prompts
    }

    fn temps(&self) -> std::sync::MutexGuard<'_, Vec<PathBuf>> {
        self.temps.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Create a private temp directory (mode 0700, fresh unpredictable name, `mkdir` never follows
    /// a planted symlink) and remember it for cleanup. Blocking: worker threads only.
    #[allow(clippy::disallowed_methods)] // worker thread
    pub fn new_temp_dir(&self) -> io::Result<PathBuf> {
        use std::os::unix::fs::DirBuilderExt;
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir();
        let mut last = None;
        for _ in 0..16 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = base.join(format!(
                "ofm-archive-{}-{:x}{:x}",
                std::process::id(),
                nanos,
                n
            ));
            match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => {
                    self.temps().push(dir.clone());
                    return Ok(dir);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last.unwrap_or_else(|| io::Error::other("no free temp directory name")))
    }

    /// Delete a temp directory made by [`new_temp_dir`](Self::new_temp_dir). Blocking: workers.
    #[allow(clippy::disallowed_methods)] // worker thread
    pub fn remove_temp(&self, dir: &Path) {
        self.temps().retain(|d| d != dir);
        if let Err(e) = std::fs::remove_dir_all(dir) {
            tracing::debug!(dir = %dir.display(), error = %e, "temp dir cleanup failed");
        }
    }

    /// Remove every remaining temp directory (application exit).
    #[allow(clippy::disallowed_methods)] // application exit: the window is already gone
    pub fn cleanup_all(&self) {
        let dirs = std::mem::take(&mut *self.temps());
        for d in dirs {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    /// Number of temp directories still alive (tests).
    pub fn temp_count(&self) -> usize {
        self.temps().len()
    }
}

impl Default for ArchiveService {
    fn default() -> Self {
        Self::new()
    }
}

/// The registered provider: `ArchiveVfs` with the listing's `CancelToken` made known to the
/// password provider, so cancelling a listing releases a worker that waits for a password.
struct ScopedVfs(Arc<ArchiveVfs>);

impl ScopedVfs {
    /// Names of the encrypted files directly in `loc` (empty when unknown).
    fn encrypted_names(&self, loc: &Location, cancel: &CancelToken) -> HashSet<OsString> {
        let Some((archive, inner)) = split(loc) else {
            return HashSet::new();
        };
        let Ok(index) = self.0.index(archive, cancel) else {
            return HashSet::new();
        };
        index
            .list_dir(inner)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.encrypted)
            .filter_map(|e| e.path.file_name().map(ToOwned::to_owned))
            .collect()
    }
}

impl Vfs for ScopedVfs {
    fn list(&self, loc: &Location, cancel: &CancelToken, sink: &mut dyn FnMut(ListEvent)) {
        let _scope = password::scope(cancel);
        let mut locked: Option<HashSet<OsString>> = None;
        self.0.list(loc, cancel, &mut |ev| match ev {
            ListEvent::Batch(mut entries) => {
                // Entries with encrypted data get the lock flag (the index is cached by now).
                let names = locked.get_or_insert_with(|| self.encrypted_names(loc, cancel));
                if !names.is_empty() {
                    for e in &mut entries {
                        if names.contains(&e.name) {
                            e.mode |= ARCHIVE_ENCRYPTED;
                        }
                    }
                }
                sink(ListEvent::Batch(entries));
            }
            other => sink(other),
        });
    }

    fn stat(&self, loc: &Location) -> Result<Entry, VfsError> {
        self.0.stat(loc)
    }

    fn open_read(&self, loc: &Location) -> Result<Box<dyn ReadSeek + Send>, VfsError> {
        self.0.open_read(loc)
    }

    fn capabilities(&self, loc: &Location) -> Caps {
        self.0.capabilities(loc)
    }

    fn parent(&self, loc: &Location) -> Option<Location> {
        self.0.parent(loc)
    }

    fn space(&self, loc: &Location) -> Result<SpaceInfo, VfsError> {
        self.0.space(loc)
    }
}

// ---- pure helpers -----------------------------------------------------------------------------

/// True for `Location::Archive`.
pub fn is_archive(loc: &Location) -> bool {
    matches!(loc, Location::Archive { .. })
}

/// `(archive file, inner path)` of an archive location.
pub fn split(loc: &Location) -> Option<(&Path, &Path)> {
    match loc {
        Location::Archive { archive, inner } => Some((archive, inner)),
        _ => None,
    }
}

/// Where a copy out of `archive` goes by default when the other panel is not a plain folder:
/// the folder that contains the archive.
pub fn default_extract_dir(other: &Location, archive: &Path) -> Option<PathBuf> {
    match other {
        Location::Local(p) => Some(p.clone()),
        _ => archive.parent().map(Path::to_owned),
    }
}

/// The message shown when something modifies an archive.
pub fn read_only_message() -> &'static str {
    tr("Archives are read-only")
}

/// Why a command is not available in an archive panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The command would modify the archive.
    ReadOnly,
    /// The command needs real files: extract first.
    ExtractFirst,
}

impl Refusal {
    /// Text for the message row.
    pub fn text(self) -> &'static str {
        match self {
            Refusal::ReadOnly => read_only_message(),
            Refusal::ExtractFirst => {
                tr("Not available in an archive: extract the files first (F5)")
            }
        }
    }
}

/// The disabled-operation table: whether `command` must be refused when the active panel shows an
/// archive (`active_archive`) or the *other* panel does (`other_archive`, the copy/move target).
pub fn refusal(command: CommandId, active_archive: bool, other_archive: bool) -> Option<Refusal> {
    if active_archive {
        let table: [(CommandId, Refusal); 12] = [
            (cmd::FILE_RENAME, Refusal::ReadOnly),
            (cmd::FILE_MKDIR, Refusal::ReadOnly),
            (cmd::FILE_DELETE, Refusal::ReadOnly),
            (cmd::FILE_DELETE_PERMANENT, Refusal::ReadOnly),
            (cmd::FILE_LINK, Refusal::ReadOnly),
            (cmd::FILE_PROPERTIES, Refusal::ReadOnly),
            (cmd::FILE_NEW_FILE, Refusal::ReadOnly),
            (cmd::FILE_CLIPBOARD_PASTE, Refusal::ReadOnly),
            (cmd::FILE_CLIPBOARD_CUT, Refusal::ReadOnly),
            (cmd::FILE_BATCH_RENAME, Refusal::ReadOnly),
            (cmd::FILE_BATCH_RENAME_UNDO, Refusal::ReadOnly),
            (cmd::FILE_CLIPBOARD_COPY, Refusal::ExtractFirst),
        ];
        if let Some((_, r)) = table.iter().find(|(c, _)| *c == command) {
            return Some(*r);
        }
        if command == cmd::FILE_OPEN_WITH {
            return Some(Refusal::ExtractFirst);
        }
    }
    if !active_archive && other_archive && (command == cmd::FILE_COPY || command == cmd::FILE_MOVE)
    {
        return Some(Refusal::ReadOnly);
    }
    None
}

/// User-presentable text of an archive error (the dialog shown when opening fails).
pub fn error_text(e: &ArchiveError) -> String {
    match e {
        ArchiveError::Corrupt { archive, detail } => format!(
            "{} {}\n\n{detail}",
            tr("The archive is damaged or incomplete:"),
            name_of(archive)
        ),
        ArchiveError::Unsupported { archive, what } => format!(
            "{} {}\n\n{what}",
            tr("This archive uses a feature that is not supported:"),
            name_of(archive)
        ),
        ArchiveError::NeedsPassword(a) => {
            format!("{} {}", tr("A password is required for"), name_of(a))
        }
        ArchiveError::WrongPassword(a) => {
            format!("{} {}", tr("The password is wrong for"), name_of(a))
        }
        ArchiveError::Unsafe { path, .. } => format!(
            "{} {}",
            tr("Refusing an unsafe archive entry:"),
            path.display()
        ),
        ArchiveError::LimitExceeded { archive, kind } => format!(
            "{} {}\n\n{}",
            tr("The archive is too large or too complex to open:"),
            name_of(archive),
            match kind {
                LimitKind::EntryCount => tr("It has too many entries."),
                LimitKind::Depth => tr("Its folders are nested too deeply."),
                LimitKind::CodecMemory => tr("It needs more memory than the safety limit allows."),
                LimitKind::ExceedsDeclaredSize =>
                    tr("An entry is larger than the archive says (possible decompression bomb)."),
            }
        ),
        ArchiveError::NotAnArchive(a) => {
            format!("{} {}", tr("Not a supported archive:"), name_of(a))
        }
        ArchiveError::NoSuchEntry(_) => tr("The entry is not in the archive.").to_owned(),
        ArchiveError::Cancelled => tr("Cancelled.").to_owned(),
        ArchiveError::Io { path, source } => format!("{}: {source}", path.display()),
        ArchiveError::NotImplemented(what) => format!("{} {what}", tr("Not implemented yet:")),
    }
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

// ---- running operations ----------------------------------------------------------------------

/// Bookkeeping of the archive operations that can be cancelled with Esc (state in `ctx.ext`).
#[derive(Default)]
pub struct ArchiveState {
    current: RefCell<Option<CancelToken>>,
    serial: Cell<u64>,
    last_error: RefCell<Option<String>>,
    last_note: RefCell<Option<String>>,
}

impl ArchiveState {
    /// Start tracking a new operation (cancelling the previous one); returns its serial.
    pub fn begin(&self, cancel: &CancelToken) -> u64 {
        if let Some(old) = self.current.borrow_mut().replace(cancel.clone()) {
            old.cancel();
        }
        let serial = self.serial.get() + 1;
        self.serial.set(serial);
        serial
    }

    /// The operation `serial` ended (if it is still the current one).
    pub(crate) fn end(&self, serial: u64) {
        if self.serial.get() == serial {
            self.current.borrow_mut().take();
        }
    }

    pub fn is_current(&self, serial: u64) -> bool {
        self.serial.get() == serial
    }

    /// Esc: cancel the running operation. Returns whether there was one.
    pub fn cancel_current(&self) -> bool {
        match self.current.borrow_mut().take() {
            Some(c) => {
                c.cancel();
                self.serial.set(self.serial.get() + 1);
                true
            }
            None => false,
        }
    }

    /// Whether an open/extract-for-view is running.
    pub fn busy(&self) -> bool {
        self.current.borrow().is_some()
    }

    /// The last error text that was shown (tests).
    pub fn last_error(&self) -> Option<String> {
        self.last_error.borrow().clone()
    }

    /// The last informational note ("N unsafe entries hidden", tests).
    pub fn last_note(&self) -> Option<String> {
        self.last_note.borrow().clone()
    }

    pub(crate) fn set_error(&self, text: &str) {
        *self.last_error.borrow_mut() = Some(text.to_owned());
    }
}

/// The context's [`ArchiveState`].
pub fn state(ctx: &AppContext) -> Option<Rc<ArchiveState>> {
    ctx.ext.get::<ArchiveState>()
}

/// Create the state and attach the password prompt UI (hook in `hooks.rs`).
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(ArchiveState::default()));
    password::install(ctx);
}

/// Show "Reading archive..." (or similar) once `delay` passed and the operation is still the
/// current one; hide it again with [`busy_done`].
pub(crate) fn busy_after(ctx: &Rc<AppContext>, serial: u64, text: &'static str) {
    let weak = Rc::downgrade(ctx);
    gtk::glib::timeout_add_local_once(SHOW_BUSY_AFTER, move || {
        let Some(ctx) = weak.upgrade() else { return };
        if let Some(st) = state(&ctx)
            && st.is_current(serial)
        {
            message::show_sticky(&ctx, text);
        }
    });
}

/// Hide the busy message of an operation that ended.
pub(crate) fn busy_done(ctx: &Rc<AppContext>) {
    message::hide(ctx);
}

/// Show an error text in a message dialog and remember it.
pub(crate) fn report_error(ctx: &Rc<AppContext>, text: &str) {
    if let Some(st) = state(ctx) {
        st.set_error(text);
    }
    ui::message(ctx, tr("Archive"), text);
}

// ---- opening ----------------------------------------------------------------------------------

/// What the worker found out about a file.
#[derive(Debug)]
enum Probe {
    /// Not an archive (or unreadable): treat as an ordinary file.
    NotArchive,
    /// Opened; the index is cached in the provider.
    Opened(IndexWarnings),
    /// Cancelled by the user.
    Cancelled,
    /// The archive is broken, unsupported, unsafe, too big, or needs a password nobody gave.
    Failed(ArchiveError),
}

/// Detect by magic bytes and build the index (blocking; worker threads only).
fn probe_archive(service: &ArchiveService, path: &Path, cancel: &CancelToken) -> Probe {
    match detect_format(path) {
        Ok(Some(_)) => {}
        // A compressed tar whose probe hits a resource limit: say so, do not hand the file to
        // another application as if it were no archive.
        Err(e @ ArchiveError::LimitExceeded { .. }) => return Probe::Failed(e),
        Ok(None) | Err(_) => return Probe::NotArchive,
    }
    let _scope = password::scope(cancel);
    match service.vfs().index(path, cancel) {
        Ok(ix) => Probe::Opened(ix.warnings()),
        Err(ArchiveError::NotAnArchive(_)) => Probe::NotArchive, // e.g. a plain .gz
        Err(ArchiveError::Cancelled) => Probe::Cancelled,
        Err(e) => Probe::Failed(e),
    }
}

/// Enter / Ctrl+PgDn on the regular local file `path` in `ctrl`: open it as an archive if its
/// magic bytes say so. Otherwise, when `run` is set (Enter, not Ctrl+PgDn), the file is opened the
/// ordinary way (`desktop::open`).
pub fn open_file(ctx: &Rc<AppContext>, ctrl: &Rc<PanelController>, path: PathBuf, run: bool) {
    let Some(st) = state(ctx) else {
        if run {
            crate::desktop::open::open_path(ctx, path);
        }
        return;
    };
    let cancel = CancelToken::new();
    let serial = st.begin(&cancel);
    busy_after(ctx, serial, tr("Reading archive... (Esc cancels)"));
    let service = ctx.services.archives.clone();
    let worker_path = path.clone();
    let worker_cancel = cancel.clone();
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let _ = tx.send_blocking(probe_archive(&service, &worker_path, &worker_cancel));
    });
    let (weak_ctx, weak_ctrl): (Weak<AppContext>, Weak<PanelController>) =
        (Rc::downgrade(ctx), Rc::downgrade(ctrl));
    wiring::attach(
        rx,
        move |probe| {
            let Some(ctx) = weak_ctx.upgrade() else {
                return std::ops::ControlFlow::Break(());
            };
            let Some(st) = state(&ctx) else {
                return std::ops::ControlFlow::Break(());
            };
            if !st.is_current(serial) {
                return std::ops::ControlFlow::Break(()); // cancelled or replaced
            }
            st.end(serial);
            busy_done(&ctx);
            match probe {
                Probe::NotArchive => {
                    if run {
                        crate::desktop::open::open_path(&ctx, path.clone());
                    }
                }
                Probe::Cancelled => {}
                Probe::Failed(e) => report_error(&ctx, &error_text(&e)),
                Probe::Opened(warnings) => {
                    if let Some(ctrl) = weak_ctrl.upgrade() {
                        goto::navigate(
                            &ctx,
                            &ctrl,
                            Location::Archive {
                                archive: path.clone(),
                                inner: PathBuf::new(),
                            },
                            None,
                        );
                    }
                    if warnings.unsafe_entries > 0 {
                        let note = format!(
                            "{} {}",
                            warnings.unsafe_entries,
                            tr("unsafe entries are hidden and cannot be extracted")
                        );
                        *st.last_note.borrow_mut() = Some(note.clone());
                        message::show(&ctx, &note);
                    }
                }
            }
            std::ops::ControlFlow::Break(())
        },
        || {},
    );
}

// ---- command interception ---------------------------------------------------------------------

/// Called by the dispatcher before a command runs. Returns `true` when the command was handled
/// here (refused, redirected to extraction/view, or used to cancel a running open).
pub fn intercept(ctx: &Rc<AppContext>, command: CommandId) -> bool {
    if command == cmd::PANEL_CANCEL {
        return state(ctx).is_some_and(|s| {
            let was = s.cancel_current();
            if was {
                busy_done(ctx);
            }
            was
        });
    }
    let active = ctx.active_controller().location();
    let other = ctx.other_controller().location();
    let (in_archive, target_archive) = (is_archive(&active), is_archive(&other));
    if !in_archive && !target_archive {
        return false;
    }
    if let Some(r) = refusal(command, in_archive, target_archive) {
        message::show(ctx, r.text());
        return true;
    }
    if !in_archive {
        return false;
    }
    match command {
        c if c == cmd::FILE_COPY => super::archive_extract::present(ctx, false),
        c if c == cmd::FILE_MOVE => super::archive_extract::present(ctx, true),
        c if c == cmd::FILE_VIEW || c == cmd::FILE_EDIT => super::archive_extract::view_cursor(ctx),
        c if c == cmd::FILE_CALC_SIZE => super::archive_extract::calc_sizes(ctx),
        _ => return false,
    }
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn disabled_operation_table() {
        let all_writes = [
            cmd::FILE_RENAME,
            cmd::FILE_MKDIR,
            cmd::FILE_DELETE,
            cmd::FILE_DELETE_PERMANENT,
            cmd::FILE_LINK,
            cmd::FILE_PROPERTIES,
            cmd::FILE_NEW_FILE,
            cmd::FILE_CLIPBOARD_PASTE,
            cmd::FILE_CLIPBOARD_CUT,
            cmd::FILE_BATCH_RENAME,
            cmd::FILE_BATCH_RENAME_UNDO,
        ];
        for c in all_writes {
            assert_eq!(refusal(c, true, false), Some(Refusal::ReadOnly), "{c}");
            assert_eq!(refusal(c, false, false), None, "{c} outside archives");
            assert_eq!(refusal(c, false, true), None, "{c}: only copy/move target");
        }
        // Copy/move are extraction from an archive but refused INTO one.
        assert_eq!(refusal(cmd::FILE_COPY, true, false), None);
        assert_eq!(refusal(cmd::FILE_MOVE, true, false), None);
        assert_eq!(
            refusal(cmd::FILE_COPY, false, true),
            Some(Refusal::ReadOnly)
        );
        assert_eq!(
            refusal(cmd::FILE_MOVE, false, true),
            Some(Refusal::ReadOnly)
        );
        assert_eq!(refusal(cmd::FILE_COPY, false, false), None);
        // Viewing, searching and navigation are never refused.
        for c in [
            cmd::FILE_VIEW,
            cmd::FILE_EDIT,
            cmd::PANEL_GO_UP,
            cmd::FILE_CALC_SIZE,
        ] {
            assert_eq!(refusal(c, true, true), None, "{c}");
        }
        assert_eq!(
            refusal(cmd::FILE_CLIPBOARD_COPY, true, false),
            Some(Refusal::ExtractFirst)
        );
        assert_eq!(Refusal::ReadOnly.text(), "Archives are read-only");
    }

    #[test]
    fn default_extract_target() {
        let archive = Path::new("/d/x.zip");
        assert_eq!(
            default_extract_dir(&Location::Local("/o".into()), archive),
            Some(PathBuf::from("/o"))
        );
        assert_eq!(
            default_extract_dir(
                &Location::Archive {
                    archive: "/q.zip".into(),
                    inner: "".into()
                },
                archive
            ),
            Some(PathBuf::from("/d"))
        );
    }

    #[test]
    fn error_texts_name_the_archive() {
        let t = error_text(&ArchiveError::Corrupt {
            archive: "/d/x.zip".into(),
            detail: "bad crc".into(),
        });
        assert!(t.contains("x.zip") && t.contains("bad crc"));
        assert!(error_text(&ArchiveError::NeedsPassword("/d/y.7z".into())).contains("y.7z"));
        assert!(
            error_text(&ArchiveError::LimitExceeded {
                archive: "/d/z.zip".into(),
                kind: LimitKind::ExceedsDeclaredSize
            })
            .contains("decompression bomb")
        );
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // fixture file
    fn probe_reports_a_codec_limit_instead_of_not_an_archive() {
        // A zstd frame that asks for a 2 GiB window (magic, descriptor, window byte), then one
        // empty last block.
        let dir = ofm_testkit::temp::TempDir::new().unwrap();
        let path = dir.path().join("big-window.tar.zst");
        std::fs::write(
            &path,
            [0x28, 0xB5, 0x2F, 0xFD, 0x00, 0xA8, 0x01, 0x00, 0x00],
        )
        .unwrap();
        let probe = probe_archive(&ArchiveService::new(), &path, &CancelToken::new());
        assert!(
            matches!(
                probe,
                Probe::Failed(ArchiveError::LimitExceeded {
                    kind: LimitKind::CodecMemory,
                    ..
                })
            ),
            "{probe:?}"
        );
        // A file that merely cannot be read is still treated as an ordinary file.
        let missing = dir.path().join("missing");
        let probe = probe_archive(&ArchiveService::new(), &missing, &CancelToken::new());
        assert!(matches!(probe, Probe::NotArchive), "{probe:?}");
    }

    #[test]
    fn state_tracks_the_current_operation() {
        let st = ArchiveState::default();
        let (a, b) = (CancelToken::new(), CancelToken::new());
        let s1 = st.begin(&a);
        assert!(st.busy() && st.is_current(s1));
        let s2 = st.begin(&b);
        assert!(a.is_cancelled(), "a new operation cancels the old one");
        assert!(!st.is_current(s1) && st.is_current(s2));
        st.end(s1);
        assert!(st.busy(), "an old serial must not end the new operation");
        assert!(st.cancel_current());
        assert!(b.is_cancelled() && !st.busy());
        assert!(!st.cancel_current());
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // test asserting the 0700 mode we set
    fn temp_dirs_are_private_and_cleaned() {
        use std::os::unix::fs::PermissionsExt;
        let svc = ArchiveService::new();
        let d = svc.new_temp_dir().unwrap();
        assert_eq!(
            std::fs::metadata(&d).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let d2 = svc.new_temp_dir().unwrap();
        assert_ne!(d, d2);
        assert_eq!(svc.temp_count(), 2);
        svc.remove_temp(&d);
        assert!(!d.exists() && svc.temp_count() == 1);
        svc.cleanup_all();
        assert!(!d2.exists() && svc.temp_count() == 0);
    }
}
