//! Passwords for encrypted archives (ADR-0008).
//!
//! The provider is a blocking callback invoked on a worker thread. The app implements it by sending
//! a question to the UI and waiting on a `Responder`, polling the job's/panel's `CancelToken`
//! (capture the token in the provider) so a cancelled operation does not hang on an open dialog.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// How many wrong passwords a backend tries before giving up with `ArchiveError::WrongPassword`.
pub const MAX_PASSWORD_ATTEMPTS: u32 = 3;

/// Supplies passwords. One password per archive (per-entry passwords are not supported).
pub trait PasswordProvider: Send + Sync {
    /// Password for `archive`; `retry` is true if the previous one did not work. `None` = the user
    /// cancelled (the backend then fails with `NeedsPassword`).
    fn password(&self, archive: &Path, retry: bool) -> Option<String>;

    /// Called by the backend once a password was verified (CRC/MAC ok). Caching providers remember
    /// it; the default does nothing.
    fn accepted(&self, _archive: &Path, _password: &str) {}
}

/// Provider that never has a password (searches, background indexing): encrypted archives fail
/// with `NeedsPassword` instead of prompting.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPassword;

impl PasswordProvider for NoPassword {
    fn password(&self, _archive: &Path, _retry: bool) -> Option<String> {
        None
    }
}

/// Provider with a fixed password (tests, CLI).
#[derive(Debug, Clone)]
pub struct FixedPassword(pub String);

impl PasswordProvider for FixedPassword {
    fn password(&self, _archive: &Path, retry: bool) -> Option<String> {
        (!retry).then(|| self.0.clone())
    }
}

/// Decorator remembering the accepted password per archive path for the lifetime of the app, so
/// opening, F3 and F5 extraction of one archive ask once. Shared (`Arc`) between `ArchiveVfs` and
/// extract jobs. Passwords live in memory only, never in config/session files.
#[derive(Clone)]
pub struct PasswordSource {
    inner: Arc<dyn PasswordProvider>,
    cache: Arc<Mutex<HashMap<PathBuf, String>>>,
}

impl PasswordSource {
    /// Wrap `inner` with a per-archive cache.
    pub fn new(inner: Arc<dyn PasswordProvider>) -> Self {
        PasswordSource {
            inner,
            cache: Arc::default(),
        }
    }

    /// Source that never has a password.
    pub fn none() -> Self {
        Self::new(Arc::new(NoPassword))
    }

    /// Forget the remembered password of `archive` (e.g. after the archive file changed).
    pub fn forget(&self, archive: &Path) {
        if let Ok(mut c) = self.cache.lock() {
            c.remove(archive);
        }
    }
}

impl std::fmt::Debug for PasswordSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PasswordSource(..)") // never print passwords
    }
}

impl PasswordProvider for PasswordSource {
    fn password(&self, archive: &Path, retry: bool) -> Option<String> {
        if retry {
            self.forget(archive);
        } else if let Ok(c) = self.cache.lock()
            && let Some(p) = c.get(archive)
        {
            return Some(p.clone());
        }
        self.inner.password(archive, retry)
    }

    fn accepted(&self, archive: &Path, password: &str) {
        if let Ok(mut c) = self.cache.lock() {
            c.insert(archive.to_owned(), password.to_owned());
        }
        self.inner.accepted(archive, password);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_remembers_accepted_password_and_forgets_on_retry() {
        let src = PasswordSource::new(Arc::new(FixedPassword("pw".into())));
        let a = Path::new("/x.zip");
        assert_eq!(src.password(a, false).as_deref(), Some("pw"));
        src.accepted(a, "other");
        assert_eq!(src.password(a, false).as_deref(), Some("other"));
        // A retry drops the cache and asks the inner provider, which refuses a second attempt.
        assert_eq!(src.password(a, true), None);
        assert_eq!(src.password(a, false).as_deref(), Some("pw"));
        assert_eq!(PasswordSource::none().password(a, false), None);
    }
}
