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
        Self::from_vars(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("XDG_STATE_HOME"),
            std::env::var_os("HOME"),
        )
    }
    /// Pure form of [`from_env`](Self::from_env). Per the XDG spec, empty or relative values are ignored.
    pub fn from_vars(
        config_home: Option<std::ffi::OsString>,
        state_home: Option<std::ffi::OsString>,
        home: Option<std::ffi::OsString>,
    ) -> Self {
        let abs = |v: Option<std::ffi::OsString>| v.map(PathBuf::from).filter(|p| p.is_absolute());
        let home = abs(home).unwrap_or_else(|| PathBuf::from("/"));
        let config = abs(config_home).unwrap_or_else(|| home.join(".config"));
        let state = abs(state_home).unwrap_or_else(|| home.join(".local/state"));
        ConfigPaths {
            config_dir: config.join("ofm"),
            state_dir: state.join("ofm"),
        }
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn xdg_vars_and_fallbacks() {
        let p = ConfigPaths::from_vars(os("/c"), os("/s"), os("/h"));
        assert_eq!(p.config_dir, PathBuf::from("/c/ofm"));
        assert_eq!(p.state_dir, PathBuf::from("/s/ofm"));
        assert_eq!(p.themes_dir(), PathBuf::from("/c/ofm/themes"));
        let p = ConfigPaths::from_vars(None, os(""), os("/h"));
        assert_eq!(p.config_dir, PathBuf::from("/h/.config/ofm"));
        assert_eq!(p.state_dir, PathBuf::from("/h/.local/state/ofm"));
        let p = ConfigPaths::from_vars(os("rel"), None, os("/h"));
        assert_eq!(p.config_dir, PathBuf::from("/h/.config/ofm"));
    }
}
