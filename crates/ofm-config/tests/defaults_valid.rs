#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Guards the shipped default files against the schema and the command registry (T1.0).

use std::collections::HashSet;

use ofm_config::defaults;
use ofm_config::theme::Theme;
use ofm_core::{CommandId, KeyContext, commands};

#[test]
fn themes_deserialize() {
    for (name, text) in [
        ("far-blue", defaults::THEME_FAR_BLUE),
        ("commander-light", defaults::THEME_COMMANDER_LIGHT),
        ("adwaita", defaults::THEME_ADWAITA),
    ] {
        let t: Theme = toml::from_str(text).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(t.id, name);
        assert_eq!(t.version, 1);
    }
}

#[test]
fn menu_deserializes() {
    let m: ofm_config::menu::UserMenu = toml::from_str(defaults::MENU).unwrap();
    assert!(!m.item.is_empty());
}

#[test]
fn keymaps_reference_only_known_commands_and_cover_all_in_every_preset() {
    for (name, text) in [
        ("far", defaults::KEYMAP_FAR),
        ("tc", defaults::KEYMAP_TC),
        ("mc", defaults::KEYMAP_MC),
    ] {
        let table: toml::Table = toml::from_str(text).unwrap();
        let mut bound = HashSet::new();
        for (section, value) in &table {
            if let Some(ctx) = KeyContext::from_section(section) {
                let keys = value.as_table().unwrap();
                for (chord, cmd) in keys {
                    let id = cmd.as_str().unwrap();
                    let c = CommandId::lookup(id)
                        .unwrap_or_else(|| panic!("{name}: unknown command {id} for {chord}"));
                    assert_eq!(
                        c.info().unwrap().context,
                        ctx,
                        "{name}: {id} in wrong section"
                    );
                    bound.insert(c);
                }
            }
        }
        for c in commands::ALL {
            assert!(bound.contains(&c.id), "{name}: {} unbound", c.id);
        }
    }
}
