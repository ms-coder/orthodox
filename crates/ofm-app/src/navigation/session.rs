//! Tabs <-> `ofm_config::session::PanelSession` (F05 "restored per session").
//!
//! Pure data conversion; the session file itself (and when it is read/written) belongs to the
//! window/session code, which calls [`capture`] when saving and [`restore`] right after the window
//! exists. Only local locations are stored; an archive tab is stored as the archive's folder.

use ofm_config::session::{PanelSession, TabSession};
use ofm_core::Location;
use ofm_core::panel::{SortKey, SortSpec};

use crate::panel::{LoadOpts, PanelHost};

fn local_path_of(loc: &Location) -> Option<std::path::PathBuf> {
    match loc {
        Location::Local(p) => Some(p.clone()),
        Location::Archive { archive, .. } => archive.parent().map(std::path::Path::to_owned),
        Location::List { .. } => None,
    }
}

fn sort_name(key: SortKey) -> &'static str {
    match key {
        SortKey::Name => "name",
        SortKey::Ext => "ext",
        SortKey::Size => "size",
        SortKey::Mtime => "mtime",
    }
}

fn sort_key(name: &str) -> SortKey {
    match name {
        "ext" => SortKey::Ext,
        "size" => SortKey::Size,
        "mtime" => SortKey::Mtime,
        _ => SortKey::Name,
    }
}

/// Snapshot the tabs of `host`. The `view` field is left empty (view mode is owned by the list
/// widget); tabs without a local location (panelized lists) are skipped.
pub fn capture(host: &PanelHost) -> PanelSession {
    let mut tabs = Vec::new();
    let mut active_tab = 0;
    for (i, tab) in host.tabs().iter().enumerate() {
        let Some(path) = local_path_of(&tab.location()) else {
            continue;
        };
        if i == host.active_index() {
            active_tab = tabs.len();
        }
        let model = tab.model();
        let spec = model.sort_spec();
        let mut t = TabSession {
            sort: sort_name(spec.key).to_owned(),
            sort_reverse: spec.reverse,
            dirs_first: spec.dirs_first,
            show_hidden: model.show_hidden(),
            locked: tab.locked(),
            ..TabSession::default()
        };
        t.set_path(&path);
        if let Some(e) = model.cursor_entry() {
            t.set_cursor_name(&e.name);
        }
        tabs.push(t);
    }
    PanelSession { active_tab, tabs }
}

/// Re-create the tabs of `ps` in `host`: the first stored tab takes over the host's existing tab,
/// the others are added to its right. Does nothing for an empty session; the active tab index is
/// clamped to the tab count (a corrupt session must not panic).
pub fn restore(host: &PanelHost, ps: &PanelSession) {
    let stored: Vec<(&TabSession, std::path::PathBuf)> = ps
        .tabs
        .iter()
        .filter_map(|t| t.path_buf().map(|p| (t, p)))
        .collect();
    if stored.is_empty() {
        return;
    }
    let base = host.active_index();
    for (i, (t, path)) in stored.iter().enumerate() {
        let loc = Location::Local(path.clone());
        let tab = if i == 0 {
            host.active()
        } else {
            // Inserted right of the previous one (which is active), so the order is kept.
            host.add_tab(loc.clone(), true)
        };
        tab.mutate_view(|m| {
            m.set_show_hidden(t.show_hidden);
            m.set_sort(SortSpec {
                key: sort_key(&t.sort),
                reverse: t.sort_reverse,
                dirs_first: t.dirs_first,
            })
        });
        tab.set_locked(t.locked);
        tab.load(
            loc,
            LoadOpts {
                keep_selection: false,
                cursor_name: t.cursor_name_os(),
                skip_history: i != 0,
            },
        );
    }
    let last = base + stored.len() - 1;
    let want = base + ps.active_tab.min(stored.len() - 1);
    host.set_active(want.min(last));
    host.tabs_changed.emit(&());
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::context::Services;

    #[test]
    fn capture_skips_lists_and_tracks_active() {
        let host = PanelHost::new(Services::new(), Location::Local("/tmp".into()));
        let ps = capture(&host);
        assert_eq!(ps.tabs.len(), 1);
        assert_eq!(ps.tabs[0].path_buf(), Some("/tmp".into()));
        assert_eq!(ps.active_tab, 0);
        assert_eq!(ps.tabs[0].sort, "name");
        assert!(ps.tabs[0].dirs_first);
    }

    #[test]
    fn sort_names_round_trip() {
        for k in [SortKey::Name, SortKey::Ext, SortKey::Size, SortKey::Mtime] {
            assert_eq!(sort_key(sort_name(k)), k);
        }
    }
}
