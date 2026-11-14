//! View-mode and column settings of the list.
//!
//! The frozen `PanelController` has no view-mode field, so the mode of every tab lives here, in a
//! main-thread registry keyed by the controller (weakly: closed tabs disappear). The panel
//! settings from `config.toml` (`[panels]`) are installed with [`configure`]; before that the
//! built-in defaults apply.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use ofm_config::config::PanelsSection;

use crate::i18n::tr;
use crate::panel::{PanelController, PanelEvent};

/// How a panel lists its entries (F02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ViewMode {
    /// Several columns of names, filled column by column.
    #[default]
    Brief,
    /// One column set: name, size, date, permissions.
    Full,
    /// One column set chosen in `[panels] custom_columns`.
    Custom,
}

impl ViewMode {
    /// Parse the config / session spelling (`brief`, `full`, `custom`).
    pub fn parse(s: &str) -> Option<ViewMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "brief" => Some(ViewMode::Brief),
            "full" => Some(ViewMode::Full),
            "custom" => Some(ViewMode::Custom),
            _ => None,
        }
    }

    /// The config / session spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ViewMode::Brief => "brief",
            ViewMode::Full => "full",
            ViewMode::Custom => "custom",
        }
    }
}

/// A data column of Full/Custom mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Column {
    /// Name (takes the remaining width).
    Name,
    /// Extension.
    Ext,
    /// Size.
    Size,
    /// Modification time.
    Mtime,
    /// Permission string.
    Mode,
    /// Owner uid.
    Owner,
    /// Group gid.
    Group,
}

impl Column {
    /// Parse a `custom_columns` item.
    pub fn parse(s: &str) -> Option<Column> {
        match s.trim().to_ascii_lowercase().as_str() {
            "name" => Some(Column::Name),
            "ext" | "extension" => Some(Column::Ext),
            "size" => Some(Column::Size),
            "mtime" | "modified" | "date" => Some(Column::Mtime),
            "mode" | "perms" | "permissions" => Some(Column::Mode),
            "owner" | "uid" | "user" => Some(Column::Owner),
            "group" | "gid" => Some(Column::Group),
            _ => None,
        }
    }

    /// Column width in characters (the name column has none: it stretches).
    pub fn width_chars(self) -> u32 {
        match self {
            Column::Name => 0,
            Column::Ext => 6,
            Column::Size => 10,
            Column::Mtime => 17,
            Column::Mode => 11,
            Column::Owner | Column::Group => 7,
        }
    }

    /// Header label.
    pub fn title(self) -> &'static str {
        match self {
            Column::Name => tr("Name"),
            Column::Ext => tr("Ext"),
            Column::Size => tr("Size"),
            Column::Mtime => tr("Modified"),
            Column::Mode => tr("Mode"),
            Column::Owner => tr("Owner"),
            Column::Group => tr("Group"),
        }
    }

    /// Right-aligned columns hold numbers.
    pub fn right_aligned(self) -> bool {
        matches!(self, Column::Size | Column::Owner | Column::Group)
    }

    /// The sort key a click on this column's header selects.
    pub fn sort_key(self) -> Option<ofm_core::panel::SortKey> {
        use ofm_core::panel::SortKey;
        match self {
            Column::Name => Some(SortKey::Name),
            Column::Ext => Some(SortKey::Ext),
            Column::Size => Some(SortKey::Size),
            Column::Mtime => Some(SortKey::Mtime),
            _ => None,
        }
    }
}

/// How sizes are written (`[panels] size_format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SizeFormat {
    /// 1.5 KiB (powers of 1024).
    #[default]
    Binary,
    /// 1.5 kB (powers of 1000).
    Si,
    /// Exact byte count.
    Bytes,
}

impl SizeFormat {
    fn parse(s: &str) -> SizeFormat {
        match s.trim().to_ascii_lowercase().as_str() {
            "si" => SizeFormat::Si,
            "bytes" => SizeFormat::Bytes,
            _ => SizeFormat::Binary,
        }
    }
}

/// Everything the list reads from `[panels]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// View mode of tabs that were never given one explicitly.
    pub default_view: ViewMode,
    /// Columns of [`ViewMode::Custom`].
    pub custom_columns: Vec<Column>,
    /// Size notation.
    pub size_format: SizeFormat,
    /// `strftime`-style time format.
    pub time_format: String,
}

impl Settings {
    /// Settings for a `[panels]` section; unknown column names are ignored.
    pub fn from_config(p: &PanelsSection) -> Settings {
        let mut custom_columns: Vec<Column> = p
            .custom_columns
            .iter()
            .filter_map(|c| Column::parse(c))
            .collect();
        if custom_columns.is_empty() {
            custom_columns.push(Column::Name);
        }
        Settings {
            default_view: ViewMode::parse(&p.default_view).unwrap_or_default(),
            custom_columns,
            size_format: SizeFormat::parse(&p.size_format),
            time_format: if p.time_format.trim().is_empty() {
                "%Y-%m-%d %H:%M".to_owned()
            } else {
                p.time_format.clone()
            },
        }
    }

    /// Columns shown in `mode` (`Brief` has none: it shows names only).
    pub fn columns(&self, mode: ViewMode) -> Vec<Column> {
        match mode {
            ViewMode::Brief => vec![Column::Name],
            ViewMode::Full => vec![Column::Name, Column::Size, Column::Mtime, Column::Mode],
            ViewMode::Custom => self.custom_columns.clone(),
        }
    }
}

thread_local! {
    static SETTINGS: RefCell<Rc<Settings>> =
        RefCell::new(Rc::new(Settings::from_config(&PanelsSection::default())));
    /// Bumped whenever [`configure`] runs, so widgets drop their text caches.
    static GENERATION: Cell<u64> = const { Cell::new(0) };
    static MODES: RefCell<Vec<(Weak<PanelController>, ViewMode)>> = const { RefCell::new(Vec::new()) };
}

/// Install the `[panels]` settings (startup and `app.reload_config`). Lists pick them up at their
/// next redraw; call `queue_draw` on lists if an immediate refresh matters.
pub fn configure(panels: &PanelsSection) {
    SETTINGS.with(|s| *s.borrow_mut() = Rc::new(Settings::from_config(panels)));
    GENERATION.with(|g| g.set(g.get() + 1));
}

/// Current settings.
pub fn settings() -> Rc<Settings> {
    SETTINGS.with(|s| Rc::clone(&s.borrow()))
}

/// Counter that changes whenever the settings changed.
pub fn generation() -> u64 {
    GENERATION.with(Cell::get)
}

/// View mode of a tab (the configured default until one was set).
pub fn view_mode(ctrl: &PanelController) -> ViewMode {
    let ptr: *const PanelController = ctrl;
    let found = MODES.with(|m| {
        m.borrow()
            .iter()
            .find(|(w, _)| std::ptr::eq(w.as_ptr(), ptr))
            .map(|(_, mode)| *mode)
    });
    found.unwrap_or_else(|| settings().default_view)
}

/// Set the view mode of a tab and tell its views (they re-layout and redraw). Session restore
/// calls this with the saved mode.
pub fn set_view_mode(ctrl: &Rc<PanelController>, mode: ViewMode) {
    MODES.with(|m| {
        let mut m = m.borrow_mut();
        m.retain(|(w, _)| w.strong_count() > 0);
        let ptr = Rc::as_ptr(ctrl);
        match m.iter_mut().find(|(w, _)| std::ptr::eq(w.as_ptr(), ptr)) {
            Some(slot) => slot.1 = mode,
            None => m.push((Rc::downgrade(ctrl), mode)),
        }
    });
    ctrl.changed.emit(&PanelEvent::Redraw);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_modes_and_columns() {
        assert_eq!(ViewMode::parse("Full"), Some(ViewMode::Full));
        assert_eq!(ViewMode::parse("nope"), None);
        for m in [ViewMode::Brief, ViewMode::Full, ViewMode::Custom] {
            assert_eq!(ViewMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(Column::parse("mtime"), Some(Column::Mtime));
        assert_eq!(Column::parse("bogus"), None);
    }

    #[test]
    fn settings_from_default_config() {
        let s = Settings::from_config(&PanelsSection::default());
        assert_eq!(s.default_view, ViewMode::Brief);
        assert_eq!(
            s.custom_columns,
            [Column::Name, Column::Size, Column::Mtime, Column::Mode]
        );
        assert_eq!(s.columns(ViewMode::Brief), [Column::Name]);
        let bad = PanelsSection {
            custom_columns: vec!["zzz".into()],
            ..PanelsSection::default()
        };
        assert_eq!(Settings::from_config(&bad).custom_columns, [Column::Name]);
    }
}
