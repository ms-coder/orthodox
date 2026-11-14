//! ofm-config: config, keymaps, themes, session/history storage, XDG paths. No GTK.
//! The whole crate is owned by T1.3 (file layout may change; keep the public API below).
//!
//! Load policy (ADR-0005): built-in defaults are embedded (`defaults/`); user files override;
//! an invalid user file yields the defaults plus a [`Warning`], never a crash; all writes are atomic.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod config;
pub mod defaults;
pub mod error;
pub mod highlight;
pub mod history;
pub mod io;
pub mod keymap;
pub mod menu;
pub mod paths;
pub mod reload;
pub mod session;
pub mod theme;

pub use config::Config;
pub use error::{ConfigError, Warning};
pub use keymap::{Keymap, Preset, TypingMode};
pub use paths::ConfigPaths;
pub use theme::Theme;
