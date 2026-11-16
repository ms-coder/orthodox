//! Prompt text of the command line (pure; unit-tested): the active panel's directory, home
//! abbreviated as `~`, non-UTF-8 components displayed lossily (invariant 4).

use std::path::Path;

use ofm_core::Location;

/// The prompt for `loc`: `~/src/ofm$ ` for a local directory under home, `/etc$ ` elsewhere,
/// and the lossy display form for archives / panelized lists.
pub fn prompt_text(loc: &Location, home: &Path) -> String {
    let dir = match loc {
        Location::Local(p) => abbreviate_home(p, home),
        other => other.display_lossy(),
    };
    format!("{dir}$ ")
}

/// Lossy display of `path` with a leading `home` replaced by `~`.
pub fn abbreviate_home(path: &Path, home: &Path) -> String {
    if path == home {
        return "~".to_owned();
    }
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.to_string_lossy()),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn home() -> PathBuf {
        PathBuf::from("/home/u")
    }

    #[test]
    fn local_paths() {
        assert_eq!(
            prompt_text(&Location::Local("/home/u".into()), &home()),
            "~$ "
        );
        assert_eq!(
            prompt_text(&Location::Local("/home/u/src/ofm".into()), &home()),
            "~/src/ofm$ "
        );
        assert_eq!(
            prompt_text(&Location::Local("/etc".into()), &home()),
            "/etc$ "
        );
        // `/home/ux` must not abbreviate to `~x`.
        assert_eq!(abbreviate_home(Path::new("/home/ux"), &home()), "/home/ux");
        assert_eq!(abbreviate_home(Path::new("/"), &home()), "/");
    }

    #[test]
    fn non_local_locations() {
        let a = Location::Archive {
            archive: "/tmp/x.zip".into(),
            inner: "d".into(),
        };
        assert_eq!(prompt_text(&a, &home()), "/tmp/x.zip#/d$ ");
        assert_eq!(
            prompt_text(
                &Location::List {
                    id: ofm_core::ListId(3)
                },
                &home()
            ),
            "list:3$ "
        );
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_components_display_lossily() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let raw = PathBuf::from(OsString::from_vec(b"/home/u/\xffx".to_vec()));
        assert_eq!(abbreviate_home(&raw, &home()), "~/\u{fffd}x");
    }
}
