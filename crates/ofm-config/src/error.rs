//! Errors and non-fatal warnings.

use std::path::PathBuf;

/// Hard failure (only for explicit saves; loads degrade to defaults + [`Warning`]).
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// I/O failure on `path`.
    #[error("{}: {source}", path.display())]
    Io {
        /// File.
        path: PathBuf,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// TOML syntax or schema error.
    #[error("{}: {message}", path.display())]
    Parse {
        /// File (or a pseudo path like `<builtin keymap-far.toml>`).
        path: PathBuf,
        /// Message with line information.
        message: String,
    },
    /// TOML serialization failure.
    #[error("serialize: {0}")]
    Serialize(String),
}

/// A problem the UI should show (e.g. an infobar) while continuing with defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// File concerned, if any.
    pub file: Option<PathBuf>,
    /// Human-readable message.
    pub message: String,
}
