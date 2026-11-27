//! Icon names of theme highlight rules (F30) against the display's icon theme. `ofm-config` can only
//! check the syntax of a name; whether the icon exists is known here.

use gtk::gdk;
use ofm_config::{Theme, Warning};

/// The icon-theme entry to draw for `name`: the name itself, else its `-symbolic` variant (many
/// action icons exist only that way), else `None`.
pub fn resolve_name(icons: &gtk::IconTheme, name: &str) -> Option<String> {
    if icons.has_icon(name) {
        return Some(name.to_owned());
    }
    let symbolic = format!("{name}-symbolic");
    icons.has_icon(&symbolic).then_some(symbolic)
}

/// One warning per (theme, icon name) the icon theme does not have; the list draws a generic icon
/// instead. Unmodified built-in themes are not reported: a sparse system icon theme is not a
/// problem in the user's configuration. Empty without a display.
pub fn warnings(themes: &[Theme]) -> Vec<Warning> {
    let Some(display) = gdk::Display::default() else {
        return Vec::new();
    };
    let icons = gtk::IconTheme::for_display(&display);
    unknown(themes, |name| resolve_name(&icons, name).is_some())
}

fn unknown(themes: &[Theme], known: impl Fn(&str) -> bool) -> Vec<Warning> {
    themes
        .iter()
        .filter(|t| Theme::builtin(&t.id).as_ref() != Some(*t))
        .flat_map(|t| {
            t.icon_names()
                .into_iter()
                .filter(|name| !known(name))
                .map(|name| Warning {
                    file: None,
                    message: format!(
                        "theme {:?}: unknown icon {name:?}; a generic icon is shown",
                        t.id
                    ),
                })
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn only_unknown_names_of_non_builtin_themes_warn() {
        let builtin = Theme::builtin("commander-light").unwrap();
        assert!(unknown(std::slice::from_ref(&builtin), |_| false).is_empty());
        let mut mine = builtin;
        mine.id = "mine".into();
        let w = unknown(std::slice::from_ref(&mine), |n| n != "image-x-generic");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].message.contains("\"mine\""));
        assert!(w[0].message.contains("image-x-generic"));
        assert!(unknown(&[mine], |_| true).is_empty());
    }
}
