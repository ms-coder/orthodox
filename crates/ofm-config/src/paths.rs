//! XDG locations: `$XDG_CONFIG_HOME/ofm/` (config.toml, keymap.toml, menu.toml, themes/*.toml) and
//! `$XDG_STATE_HOME/ofm/` (session.toml, history.toml).

use std::path::PathBuf;

/// Resolved directories; constructible with explicit roots for tests (no env access in tests).
#[derive(Debug, Clone)]
pub struct ConfigPaths {
    /// `<config home>/ofm`.
    pub config_dir: PathBuf,
    /// `<state home>/ofm`.
    pub state_dir: PathBuf,
}

impl ConfigPaths {
    /// From the environment (`XDG_CONFIG_HOME`/`XDG_STATE_HOME`, falling back to `~/.config`, `~/.local/state`).
    pub fn from_env() -> Self {
        todo!("T1.3")
    }
    /// With explicit roots (tests): dirs are used as given.
    pub fn with_dirs(config_dir: PathBuf, state_dir: PathBuf) -> Self {
        ConfigPaths {
            config_dir,
            state_dir,
        }
    }
    /// `config.toml`.
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
    /// `keymap.toml` (user overrides on top of the preset).
    pub fn keymap_file(&self) -> PathBuf {
        self.config_dir.join("keymap.toml")
    }
    /// `menu.toml`.
    pub fn menu_file(&self) -> PathBuf {
        self.config_dir.join("menu.toml")
    }
    /// `themes/` directory (user themes, also watched for live reload).
    pub fn themes_dir(&self) -> PathBuf {
        self.config_dir.join("themes")
    }
    /// `session.toml`.
    pub fn session_file(&self) -> PathBuf {
        self.state_dir.join("session.toml")
    }
    /// `history.toml`.
    pub fn history_file(&self) -> PathBuf {
        self.state_dir.join("history.toml")
    }
}
