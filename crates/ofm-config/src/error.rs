//! Errors and non-fatal warnings.

use std::path::{Path, PathBuf};

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
    /// Reading the file was refused: it is not a small regular file (a symlink, FIFO,
    /// socket or device, where a plain read could block or follow an attacker-swapped link)
    /// or it exceeds the applicable size limit.
    #[error("{}: {message}", path.display())]
    ReadRefused {
        /// File.
        path: PathBuf,
        /// What the file is and why it was not read.
        message: String,
    },
}

/// A problem the UI should show (e.g. an infobar) while continuing with defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// File concerned, if any.
    pub file: Option<PathBuf>,
    /// Human-readable message.
    pub message: String,
}

/// A non-fatal problem found while loading `config.toml` or `keymap.toml`, typed so the UI
/// can present it (F29). Loading always continues (forward compatibility): unknown keys are
/// ignored, unreadable/invalid files fall back to defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigWarning {
    /// A top-level section or key that is not in the schema (probably a typo like `[pannel]`);
    /// the entry is ignored and everything else still loads.
    UnknownKey {
        /// File the key was found in.
        file: PathBuf,
        /// Dotted path of the unknown key (e.g. `pannel` or `panels.show_hiddenn`).
        path: String,
    },
    /// The file exists but could not be read; defaults/preset were used.
    Unreadable {
        /// File concerned.
        file: PathBuf,
        /// Human-readable message (already includes the fallback note).
        message: String,
    },
    /// TOML syntax or schema error; defaults/preset were used.
    Invalid {
        /// File concerned.
        file: PathBuf,
        /// Human-readable message (already includes the fallback note).
        message: String,
    },
    /// `[keymap] preset` names no built-in preset; `far` is used.
    UnknownPreset {
        /// File concerned.
        file: PathBuf,
        /// The value as written.
        value: String,
    },
    /// `[keymap] typing` is not a known mode; the preset default is used.
    UnknownTypingMode {
        /// File concerned.
        file: PathBuf,
        /// The value as written.
        value: String,
    },
    /// `[jobs] default_run_mode` is neither `queued` nor `parallel`; `queued` is used.
    UnknownRunMode {
        /// File concerned.
        file: PathBuf,
        /// The value as written.
        value: String,
    },
}

impl ConfigWarning {
    /// The file this warning refers to.
    pub fn file(&self) -> &Path {
        match self {
            ConfigWarning::UnknownKey { file, .. }
            | ConfigWarning::Unreadable { file, .. }
            | ConfigWarning::Invalid { file, .. }
            | ConfigWarning::UnknownPreset { file, .. }
            | ConfigWarning::UnknownTypingMode { file, .. }
            | ConfigWarning::UnknownRunMode { file, .. } => file,
        }
    }
    /// Human-readable message.
    pub fn message(&self) -> String {
        match self {
            ConfigWarning::UnknownKey { path, .. } => {
                format!("unknown key {path:?}; ignored")
            }
            ConfigWarning::Unreadable { message, .. } | ConfigWarning::Invalid { message, .. } => {
                message.clone()
            }
            ConfigWarning::UnknownPreset { value, .. } => {
                format!("unknown keymap preset {value:?}; using far")
            }
            ConfigWarning::UnknownTypingMode { value, .. } => {
                format!("unknown typing mode {value:?}; using preset default")
            }
            ConfigWarning::UnknownRunMode { value, .. } => {
                format!("unknown jobs.default_run_mode {value:?}; using queued")
            }
        }
    }
    /// Convert to the untyped legacy [`Warning`].
    pub fn to_warning(&self) -> Warning {
        Warning {
            file: Some(self.file().to_path_buf()),
            message: self.message(),
        }
    }
}
