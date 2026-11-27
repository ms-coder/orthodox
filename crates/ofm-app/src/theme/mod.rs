//! Theme runtime (owner: T2.2). Converts an `ofm-config::Theme` into
//! - a [`Palette`]: resolved colours for the custom-drawn widgets (`FileList`, F-key bar, ...), and
//! - generated CSS ([`css::generate`]) applied through a `gtk::CssProvider` for standard widgets.
//!
//! [`ThemeHost`] is the runtime object in `AppContext::theme`: `set_theme` switches instantly and
//! emits `changed` with the new palette; widgets subscribe once and redraw.
//!
//! T2.0 provides the working skeleton (palette for panel/cursor/marked roles, minimal CSS);
//! T2.2 extends all three files freely as long as the public items below stay source-compatible.

pub mod css;
mod host;
pub mod icons;
mod palette;
pub mod registry;

pub use host::ThemeHost;
pub use palette::{CursorPalette, Palette, PanelPalette, SystemColors, luminance, mix, over};
pub use registry::ThemeState;
