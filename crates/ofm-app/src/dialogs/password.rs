//! Password prompt for encrypted archives (T6.3, F26, ADR-0008 "Password model").
//!
//! The archive backends call [`PasswordProvider::password`] on a *worker* thread and block. The
//! app's provider, [`AppPasswords`], sends a [`PasswordRequest`] to the GTK thread, where
//! [`install`]'s pump shows a modal "This archive is encrypted" dialog, and then waits on a
//! [`Responder`] **while polling for cancellation** every 100 ms, so a cancelled open or a
//! cancelled job never leaves a blocked thread behind:
//!
//! * browsing (panel opens, listings, F3 extraction on the I/O pool) runs inside a [`scope`] that
//!   carries the operation's `CancelToken`; the provider watches it;
//! * extract jobs run on the engine's threads (no scope): [`abort_pending`]`(Origin::Job)` is
//!   called when a job is cancelled and closes their dialogs;
//! * closing the dialog (Esc, Cancel, window gone) drops the responder: the worker gets `None`.
//!
//! Passwords are kept in memory only (the `PasswordSource` cache of the [`ArchiveService`]) and
//! are overwritten (best effort) when the wrapper types holding them are dropped; they never reach
//! config, history, logs or `Debug` output.
//!
//! [`ArchiveService`]: crate::panel::archive::ArchiveService

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use adw::prelude::*;
use ofm_core::{CancelToken, Responder, Wait};
use ofm_vfs::archive::PasswordProvider;

use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::ui;
use crate::navigation::modal;
use crate::wiring;

/// How often a blocked worker looks for cancellation.
const POLL: Duration = Duration::from_millis(100);

/// Who asks: decides which cancellations close the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Browsing, listing, viewing: bound to a panel/open `CancelToken` through [`scope`].
    Browse,
    /// An extract job on an engine thread.
    Job,
}

/// A password that overwrites its bytes when dropped (best effort: copies made by the allocator
/// or by the callee cannot be reached).
pub struct Secret(String);

impl Secret {
    /// Wrap `s`.
    pub fn new(s: String) -> Self {
        Secret(s)
    }

    /// Copy of the text for the (String-based) provider API.
    pub fn to_plain(&self) -> String {
        self.0.clone()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)") // never print passwords
    }
}

/// Overwrite `s` with zeros (volatile writes, so the compiler cannot drop them) and clear it.
pub fn wipe(s: &mut String) {
    let mut bytes = std::mem::take(s).into_bytes();
    for b in bytes.iter_mut() {
        // SAFETY: `b` is a valid, aligned, exclusive reference to a `u8`.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(Ordering::SeqCst);
}

/// One question from a worker to the UI.
pub struct PasswordRequest {
    /// The archive file.
    pub archive: PathBuf,
    /// The previous password was wrong.
    pub retry: bool,
    /// Which kind of operation asks.
    pub origin: Origin,
    /// Answer exactly once; dropping it means "cancelled".
    pub responder: Responder<Option<Secret>>,
    /// Set by whoever cancels the operation: the dialog closes itself.
    pub abandoned: Arc<AtomicBool>,
}

thread_local! {
    /// Cancel token of the browse operation running on this thread.
    static SCOPE: RefCell<Option<CancelToken>> = const { RefCell::new(None) };
}

/// Keeps a thread-local cancel token installed (see [`scope`]).
pub struct ScopeGuard(Option<CancelToken>);

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        SCOPE.with(|s| *s.borrow_mut() = self.0.take());
    }
}

/// While the guard lives, password prompts raised on this thread are bound to `cancel`.
pub fn scope(cancel: &CancelToken) -> ScopeGuard {
    ScopeGuard(SCOPE.with(|s| s.borrow_mut().replace(cancel.clone())))
}

thread_local! {
    /// Prompts raised on this thread are refused (background previews must never open dialogs).
    static QUIET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Keeps prompts on this thread switched off (see [`quiet`]).
pub struct QuietGuard(bool);

impl Drop for QuietGuard {
    fn drop(&mut self) {
        QUIET.with(|q| q.set(self.0));
    }
}

/// While the guard lives, password questions on this thread are answered with "no password"
/// at once: the quick view must not pop up a dialog while the user moves the cursor.
pub fn quiet() -> QuietGuard {
    QuietGuard(QUIET.with(|q| q.replace(true)))
}

fn current_scope() -> Option<CancelToken> {
    SCOPE.with(|s| s.borrow().clone())
}

/// Prompts that are open or waiting for the UI (any thread).
static PENDING: Mutex<Vec<(Origin, Arc<AtomicBool>)>> = Mutex::new(Vec::new());

fn pending() -> std::sync::MutexGuard<'static, Vec<(Origin, Arc<AtomicBool>)>> {
    PENDING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Abort every pending prompt of `origin` (their workers get `None`, their dialogs close).
pub fn abort_pending(origin: Origin) {
    for (o, flag) in pending().iter() {
        if *o == origin {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

/// Removes a prompt from [`PENDING`] when the question is over.
struct PendingGuard(Arc<AtomicBool>);

impl Drop for PendingGuard {
    fn drop(&mut self) {
        pending().retain(|(_, f)| !Arc::ptr_eq(f, &self.0));
    }
}

/// The app's `PasswordProvider`: asks the user through the GTK thread.
pub struct AppPasswords {
    tx: async_channel::Sender<PasswordRequest>,
    rx: async_channel::Receiver<PasswordRequest>,
    attached: AtomicBool,
    scripted: Mutex<VecDeque<Option<String>>>,
}

impl AppPasswords {
    /// Provider with no UI attached yet (it then answers `None` at once).
    pub fn new() -> Self {
        let (tx, rx) = async_channel::unbounded();
        AppPasswords {
            tx,
            rx,
            attached: AtomicBool::new(false),
            scripted: Mutex::new(VecDeque::new()),
        }
    }

    /// Tests: answer the next prompts with these values instead of asking (`None` = cancel).
    pub fn script(&self, answers: impl IntoIterator<Item = Option<String>>) {
        self.scripted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(answers);
    }

    fn take_scripted(&self) -> Option<Option<String>> {
        self.scripted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
    }
}

impl Default for AppPasswords {
    fn default() -> Self {
        Self::new()
    }
}

impl PasswordProvider for AppPasswords {
    fn password(&self, archive: &Path, retry: bool) -> Option<String> {
        if let Some(answer) = self.take_scripted() {
            return answer;
        }
        if QUIET.with(std::cell::Cell::get) || !self.attached.load(Ordering::SeqCst) {
            return None; // not allowed to ask, or nobody can answer (headless)
        }
        let scope = current_scope();
        let origin = if scope.is_some() {
            Origin::Browse
        } else {
            Origin::Job
        };
        let (responder, waiting) = Responder::channel();
        let abandoned = Arc::new(AtomicBool::new(false));
        pending().push((origin, Arc::clone(&abandoned)));
        let _registered = PendingGuard(Arc::clone(&abandoned));
        let request = PasswordRequest {
            archive: archive.to_owned(),
            retry,
            origin,
            responder,
            abandoned: Arc::clone(&abandoned),
        };
        if self.tx.send_blocking(request).is_err() {
            return None;
        }
        loop {
            if abandoned.load(Ordering::SeqCst) || scope.as_ref().is_some_and(|c| c.is_cancelled())
            {
                abandoned.store(true, Ordering::SeqCst); // closes the dialog if it is shown
                return None;
            }
            match waiting.wait_timeout(POLL) {
                Wait::Answered(answer) => return answer.map(|s| s.to_plain()),
                Wait::Dropped => return None,
                Wait::TimedOut => {}
            }
        }
    }
}

/// State of the prompt UI in `ctx.ext`.
#[derive(Default)]
pub struct PasswordUi {
    shown: RefCell<Vec<Shown>>,
}

/// An open prompt (tests drive it through these handles).
struct Shown {
    dialog: adw::Dialog,
    entry: gtk::PasswordEntry,
    error: gtk::Label,
}

impl PasswordUi {
    /// Number of password dialogs currently open.
    pub fn open_count(&self) -> usize {
        self.shown.borrow().len()
    }

    /// Type `text` into the topmost open prompt (tests). Returns whether one was open.
    pub fn type_into_top(&self, text: &str) -> bool {
        match self.shown.borrow().last() {
            Some(s) => {
                s.entry.set_text(text);
                true
            }
            None => false,
        }
    }

    /// The error line of the topmost prompt, if it is visible (tests).
    pub fn top_error(&self) -> Option<String> {
        let shown = self.shown.borrow();
        let s = shown.last()?;
        s.error
            .property::<bool>("visible")
            .then(|| s.error.text().to_string())
    }
}

/// Start the pump that turns worker requests into dialogs.
pub fn install(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(PasswordUi::default()));
    let prompts = ctx.services.archives.prompts();
    prompts.attached.store(true, Ordering::SeqCst);
    let weak = Rc::downgrade(ctx);
    wiring::attach(
        prompts.rx.clone(),
        move |req| match weak.upgrade() {
            Some(ctx) => {
                present(&ctx, req);
                ControlFlow::Continue(())
            }
            None => ControlFlow::Break(()),
        },
        || {},
    );
}

/// Show the prompt for `req`. Cancelling (Esc, Cancel, closing) drops the responder.
pub fn present(ctx: &Rc<AppContext>, req: PasswordRequest) {
    let name = req
        .archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Encrypted archive"));
    dialog.set_content_width(420);
    let column = modal::padded_column(8);
    let heading = gtk::Label::builder()
        .label(tr("This archive is encrypted"))
        .xalign(0.0)
        .css_classes(["heading"])
        .build();
    column.append(&heading);
    column.append(&ui::caption(&name));
    column.append(&ui::label(tr("Enter the password:")));
    let entry = gtk::PasswordEntry::new();
    entry.set_widget_name("ofm-password-entry");
    entry.set_show_peek_icon(true); // the show-password toggle
    entry.set_hexpand(true);
    entry.set_activates_default(false);
    entry.update_property(&[gtk::accessible::Property::Label(tr("Archive password"))]);
    column.append(&entry);
    let error = ui::error_label();
    error.set_widget_name("ofm-password-error");
    if req.retry {
        ui::set_error(&error, Some(tr("Wrong password. Try again.")));
    }
    column.append(&error);
    let cancel = ui::button(tr("Cancel"));
    let ok = ui::button(tr("_OK"));
    ok.add_css_class("suggested-action");
    column.append(&ui::button_row(&[&cancel, &ok]));
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&entry));

    // The responder is answered at most once; whoever takes it first wins.
    let responder = Rc::new(RefCell::new(Some(req.responder)));
    let answer = {
        let responder = Rc::clone(&responder);
        let entry = entry.clone();
        move || {
            let mut text = entry.text().to_string();
            if let Some(r) = responder.borrow_mut().take() {
                let _ = r.answer(Some(Secret::new(text.clone())));
            }
            wipe(&mut text);
            entry.set_text("");
        }
    };
    let answer = Rc::new(answer);
    {
        let (answer, dialog) = (Rc::clone(&answer), dialog.clone());
        ok.connect_clicked(move |_| {
            answer();
            dialog.close();
        });
    }
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| {
            dialog.close();
        });
    }
    // Closing in any way without an answer cancels: dropping the responder wakes the worker.
    {
        let (responder, entry) = (Rc::clone(&responder), entry.clone());
        dialog.connect_closed(move |_| {
            responder.borrow_mut().take();
            entry.set_text("");
        });
    }
    // A cancelled operation closes its dialog.
    {
        let (abandoned, weak) = (Arc::clone(&req.abandoned), dialog.downgrade());
        gtk::glib::timeout_add_local(POLL, move || {
            let Some(d) = weak.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            if abandoned.load(Ordering::SeqCst) {
                d.close();
                return gtk::glib::ControlFlow::Break;
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    let on_ok = {
        let (ctx, answer) = (Rc::clone(ctx), Rc::clone(&answer));
        move || {
            if ui::activate_focused_button(&ctx) {
                return false; // Enter on the focused Cancel/OK button presses it
            }
            answer();
            true
        }
    };
    if !modal::present(ctx, &dialog, on_ok) {
        responder.borrow_mut().take(); // no window: cancel
        return;
    }
    if let Some(state) = ctx.ext.get::<PasswordUi>() {
        state.shown.borrow_mut().push(Shown {
            dialog: dialog.clone(),
            entry: entry.clone(),
            error: error.clone(),
        });
        let weak = Rc::downgrade(&state);
        let closing = dialog.clone();
        dialog.connect_closed(move |_| {
            if let Some(state) = weak.upgrade() {
                state.shown.borrow_mut().retain(|s| s.dialog != closing);
            }
        });
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn wipe_clears_the_text() {
        let mut s = String::from("hunter2");
        wipe(&mut s);
        assert!(s.is_empty());
        assert_eq!(format!("{:?}", Secret::new("x".into())), "Secret(..)");
    }

    #[test]
    fn provider_without_ui_or_script() {
        let p = AppPasswords::new();
        assert_eq!(p.password(Path::new("/a.zip"), false), None);
        p.script([Some("pw".to_owned()), None]);
        assert_eq!(
            p.password(Path::new("/a.zip"), false).as_deref(),
            Some("pw")
        );
        assert_eq!(p.password(Path::new("/a.zip"), true), None);
    }

    #[test]
    fn blocked_worker_wakes_on_scope_cancel_and_abort() {
        let p = Arc::new(AppPasswords::new());
        p.attached.store(true, Ordering::SeqCst);
        // Browse scope: cancelling the token releases the worker although nobody answers.
        let cancel = CancelToken::new();
        let (p2, c2) = (Arc::clone(&p), cancel.clone());
        let h = std::thread::spawn(move || {
            let _g = scope(&c2);
            p2.password(Path::new("/a.zip"), false)
        });
        std::thread::sleep(Duration::from_millis(250));
        cancel.cancel();
        assert_eq!(h.join().unwrap(), None);
        // Job origin: abort_pending releases it.
        let p3 = Arc::clone(&p);
        let h = std::thread::spawn(move || p3.password(Path::new("/b.zip"), false));
        std::thread::sleep(Duration::from_millis(250));
        abort_pending(Origin::Browse); // wrong origin: still blocked
        std::thread::sleep(Duration::from_millis(250));
        assert!(!h.is_finished());
        abort_pending(Origin::Job);
        assert_eq!(h.join().unwrap(), None);
        // The two requests above were queued for a UI that never answered: discard them.
        while p.rx.try_recv().is_ok() {}
        // A dropped responder (dialog closed) is "cancelled".
        let p4 = Arc::clone(&p);
        let h = std::thread::spawn(move || p4.password(Path::new("/c.zip"), false));
        let req = p.rx.recv_blocking().unwrap();
        drop(req.responder);
        assert_eq!(h.join().unwrap(), None);
        // And an answer is delivered.
        let p5 = Arc::clone(&p);
        let h = std::thread::spawn(move || p5.password(Path::new("/d.zip"), true));
        let req = p.rx.recv_blocking().unwrap();
        assert!(req.retry);
        assert!(req.responder.answer(Some(Secret::new("pw".into()))));
        assert_eq!(h.join().unwrap().as_deref(), Some("pw"));
    }
}
