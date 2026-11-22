//! Text of the cells: names, sizes, dates, permission strings. Pure functions (the date needs
//! glib's `DateTime`, which works without a display).

use std::os::unix::ffi::OsStrExt;

use gtk::glib;
use ofm_core::{Entry, EntryKind, TargetKind};

use super::settings::{Column, SizeFormat};
use crate::i18n::tr;

/// True for the synthetic parent row.
pub fn is_parent(e: &Entry) -> bool {
    e.name.as_bytes() == b".."
}

/// One-character type marker shown before the name (Midnight Commander style).
pub fn marker(e: &Entry) -> char {
    match e.kind {
        EntryKind::Dir => {
            if is_parent(e) {
                ' '
            } else {
                '/'
            }
        }
        EntryKind::Symlink {
            target: Some(TargetKind::Dir),
        } => '~',
        EntryKind::Symlink { target: None } => '!',
        EntryKind::Symlink { .. } => '@',
        EntryKind::Other => '=',
        EntryKind::File if e.mode & 0o111 != 0 => '*',
        EntryKind::File => ' ',
    }
}

/// Display text of the name cell: marker + lossy name (non-UTF-8 bytes become U+FFFD).
pub fn name_cell(e: &Entry) -> String {
    let mut s = String::with_capacity(e.name.len() + 1);
    s.push(marker(e));
    s.push_str(&String::from_utf8_lossy(e.name.as_bytes()));
    s
}

/// Extension of a file name: text after the last dot, none for dot-files and directories.
pub fn ext_of(e: &Entry) -> String {
    if e.kind.is_dir_like() {
        return String::new();
    }
    let name = e.name.as_bytes();
    match name.iter().rposition(|b| *b == b'.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => {
            String::from_utf8_lossy(&name[dot + 1..]).into_owned()
        }
        _ => String::new(),
    }
}

/// Size cell: `<DIR>` for directories, `<UP>` for the parent row, else the formatted size.
pub fn size_cell(e: &Entry, fmt: SizeFormat) -> String {
    if is_parent(e) {
        return tr("<UP>").to_owned();
    }
    match e.kind {
        EntryKind::Dir
        | EntryKind::Symlink {
            target: Some(TargetKind::Dir),
        } => tr("<DIR>").to_owned(),
        EntryKind::Other => tr("<DEV>").to_owned(),
        _ => format_size(e.size, fmt),
    }
}

/// `bytes` written in `fmt`; the result is at most 9 characters.
pub fn format_size(bytes: u64, fmt: SizeFormat) -> String {
    let (base, units): (f64, [&str; 6]) = match fmt {
        SizeFormat::Bytes => return bytes.to_string(),
        SizeFormat::Binary => (1024.0, ["B", "KiB", "MiB", "GiB", "TiB", "PiB"]),
        SizeFormat::Si => (1000.0, ["B", "kB", "MB", "GB", "TB", "PB"]),
    };
    if (bytes as f64) < base {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= base && unit + 1 < units.len() {
        value /= base;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", units[unit])
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

/// Date cell for `mtime_ns` in the user's `strftime` format; empty for the parent row / unknown.
pub fn time_cell(e: &Entry, fmt: &str) -> String {
    if is_parent(e) {
        return String::new();
    }
    let secs = e.mtime_ns.div_euclid(1_000_000_000);
    let Ok(secs) = i64::try_from(secs) else {
        return String::new();
    };
    glib::DateTime::from_unix_local(secs)
        .ok()
        .and_then(|dt| dt.format(fmt).ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// `drwxr-xr-x`-style permission string.
pub fn mode_string(e: &Entry) -> String {
    if is_parent(e) {
        return String::new();
    }
    let kind = match e.kind {
        EntryKind::Dir => 'd',
        EntryKind::Symlink { .. } => 'l',
        EntryKind::Other => '?',
        EntryKind::File => '-',
    };
    let m = e.mode;
    let bit = |mask: u32, c: char| if m & mask != 0 { c } else { '-' };
    // Special bits replace the execute column (setuid/setgid: s/S, sticky: t/T).
    let exec = |x: u32, special: u32, on: char| match (m & x != 0, m & special != 0) {
        (true, true) => on,
        (false, true) => on.to_ascii_uppercase(),
        (true, false) => 'x',
        (false, false) => '-',
    };
    [
        kind,
        bit(0o400, 'r'),
        bit(0o200, 'w'),
        exec(0o100, 0o4000, 's'),
        bit(0o040, 'r'),
        bit(0o020, 'w'),
        exec(0o010, 0o2000, 's'),
        bit(0o004, 'r'),
        bit(0o002, 'w'),
        exec(0o001, 0o1000, 't'),
    ]
    .iter()
    .collect()
}

/// Text of `column` for `e`.
pub fn cell_text(e: &Entry, column: Column, size_fmt: SizeFormat, time_fmt: &str) -> String {
    match column {
        Column::Name => name_cell(e),
        Column::Ext => ext_of(e),
        Column::Size => size_cell(e, size_fmt),
        Column::Mtime => time_cell(e, time_fmt),
        Column::Mode => mode_string(e),
        Column::Owner if is_parent(e) => String::new(),
        Column::Owner => {
            crate::jobs::idnames::global().map_or_else(|| e.uid.to_string(), |n| n.user_name(e.uid))
        }
        Column::Group if is_parent(e) => String::new(),
        Column::Group => crate::jobs::idnames::global()
            .map_or_else(|| e.gid.to_string(), |n| n.group_name(e.gid)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    fn file(name: &str, mode: u32) -> Entry {
        let mut e = Entry::new(name, EntryKind::File);
        e.mode = mode;
        e
    }

    #[test]
    fn sizes() {
        assert_eq!(format_size(0, SizeFormat::Binary), "0 B");
        assert_eq!(format_size(1023, SizeFormat::Binary), "1023 B");
        assert_eq!(format_size(1536, SizeFormat::Binary), "1.5 KiB");
        assert_eq!(format_size(1500, SizeFormat::Si), "1.5 kB");
        assert_eq!(
            format_size(300 * 1024 * 1024, SizeFormat::Binary),
            "300 MiB"
        );
        assert_eq!(format_size(1234567, SizeFormat::Bytes), "1234567");
        assert_eq!(format_size(u64::MAX, SizeFormat::Binary), "16384 PiB");
    }

    #[test]
    fn dir_and_parent_cells() {
        let dir = Entry::new("sub", EntryKind::Dir);
        assert_eq!(size_cell(&dir, SizeFormat::Binary), "<DIR>");
        let up = Entry::new("..", EntryKind::Dir);
        assert_eq!(size_cell(&up, SizeFormat::Binary), "<UP>");
        assert_eq!(name_cell(&up), " ..");
        assert_eq!(name_cell(&dir), "/sub");
        assert_eq!(time_cell(&up, "%Y"), "");
    }

    #[test]
    fn markers() {
        assert_eq!(marker(&file("a", 0o644)), ' ');
        assert_eq!(marker(&file("a", 0o755)), '*');
        let broken = Entry::new("l", EntryKind::Symlink { target: None });
        assert_eq!(marker(&broken), '!');
        let dl = Entry::new(
            "l",
            EntryKind::Symlink {
                target: Some(TargetKind::Dir),
            },
        );
        assert_eq!(marker(&dl), '~');
    }

    #[test]
    fn extension_rules() {
        assert_eq!(ext_of(&file("a.tar.gz", 0)), "gz");
        assert_eq!(ext_of(&file(".bashrc", 0)), "");
        assert_eq!(ext_of(&file("noext", 0)), "");
        assert_eq!(ext_of(&file("trailing.", 0)), "");
        assert_eq!(ext_of(&Entry::new("d.d", EntryKind::Dir)), "");
    }

    #[test]
    fn permission_strings() {
        assert_eq!(mode_string(&file("a", 0o100_644)), "-rw-r--r--");
        assert_eq!(mode_string(&file("a", 0o104_755)), "-rwsr-xr-x");
        assert_eq!(mode_string(&file("a", 0o104_644)), "-rwSr--r--");
        let mut d = Entry::new("d", EntryKind::Dir);
        d.mode = 0o041_777;
        assert_eq!(mode_string(&d), "drwxrwxrwt");
    }

    #[test]
    fn non_utf8_names_display_lossy() {
        let e = Entry::new(OsString::from_vec(b"a\xffb".to_vec()), EntryKind::File);
        assert_eq!(name_cell(&e), " a\u{fffd}b");
    }
}
