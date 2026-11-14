//! Built-in files embedded from `crates/ofm-config/defaults/`.

/// Default `config.toml`.
pub const CONFIG: &str = include_str!("../defaults/config.toml");
/// Default `menu.toml`.
pub const MENU: &str = include_str!("../defaults/menu.toml");
/// Keymap preset: Far (default).
pub const KEYMAP_FAR: &str = include_str!("../defaults/keymap-far.toml");
/// Keymap preset: Total/Double Commander.
pub const KEYMAP_TC: &str = include_str!("../defaults/keymap-tc.toml");
/// Keymap preset: Midnight Commander.
pub const KEYMAP_MC: &str = include_str!("../defaults/keymap-mc.toml");
/// Built-in theme: Far Blue.
pub const THEME_FAR_BLUE: &str = include_str!("../defaults/themes/far-blue.toml");
/// Built-in theme: Commander Light.
pub const THEME_COMMANDER_LIGHT: &str = include_str!("../defaults/themes/commander-light.toml");
/// Built-in theme: Adwaita.
pub const THEME_ADWAITA: &str = include_str!("../defaults/themes/adwaita.toml");
