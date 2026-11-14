//! Status line texts (F04): file under the cursor, selection summary, free space. Pure formatting
//! lives here (unit-tested); `window/panel_view.rs` puts the results into labels and asks a worker
//! for the free space (never the main thread).

use gtk::glib;
use ofm_core::Entry;
use ofm_core::EntryKind;

use crate::i18n::tr;

/// How sizes are written (`[panels] size_format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SizeFormat {
    /// 1024-based with IEC names (KiB, MiB).
    #[default]
    Binary,
    /// 1000-based with SI names (kB, MB).
    Si,
    /// Exact byte count with thousands separators.
    Bytes,
}

impl SizeFormat {
    /// Parse the config value; unknown text gives the default.
    pub fn from_config(s: &str) -> Self {
        match s {
            "si" => SizeFormat::Si,
            "bytes" => SizeFormat::Bytes,
            _ => SizeFormat::Binary,
        }
    }
}

/// Formatting options taken from the config.
#[derive(Debug, Clone, Default)]
pub struct StatusFormat {
    /// Size style.
    pub size: SizeFormat,
    /// strftime-style date format (`%Y-%m-%d %H:%M`).
    pub time: String,
}

/// Human-readable size.
pub fn format_size(bytes: u64, fmt: SizeFormat) -> String {
    let (base, units): (f64, &[&str]) = match fmt {
        SizeFormat::Bytes => return group_digits(bytes),
        SizeFormat::Binary => (1024.0, &["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"]),
        SizeFormat::Si => (1000.0, &["B", "kB", "MB", "GB", "TB", "PB", "EB"]),
    };
    if (bytes as f64) < base {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= base && i + 1 < units.len() {
        v /= base;
        i += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", units[i])
    } else {
        format!("{v:.0} {}", units[i])
    }
}

/// `1234567` -> `1 234 567`.
fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// Local date/time of a nanosecond Unix timestamp; empty when out of range.
pub fn format_time(mtime_ns: i128, fmt: &str) -> String {
    let secs = i64::try_from(mtime_ns.div_euclid(1_000_000_000)).unwrap_or(i64::MAX);
    glib::DateTime::from_unix_local(secs)
        .ok()
        .and_then(|d| {
            d.format(if fmt.is_empty() {
                "%Y-%m-%d %H:%M"
            } else {
                fmt
            })
            .ok()
        })
        .map(|g| g.to_string())
        .unwrap_or_default()
}

/// Text for the entry under the cursor: name, size (or kind marker) and modification time.
/// The synthetic `..` row gives a hint instead.
pub fn cursor_info(entry: &Entry, fmt: &StatusFormat) -> String {
    if entry.name == ".." {
        return tr("Up one level").to_owned();
    }
    let what = match &entry.kind {
        EntryKind::Dir => "<DIR>".to_owned(),
        EntryKind::Symlink { .. } => "<LNK>".to_owned(),
        EntryKind::Other => "<SPECIAL>".to_owned(),
        EntryKind::File => format_size(entry.size, fmt.size),
    };
    let when = format_time(entry.mtime_ns, &fmt.time);
    format!("{}   {what}   {when}", entry.name.to_string_lossy())
}

/// "3 selected (1.2 MiB) of 120" / "120 items".
pub fn summary(total: usize, selected: usize, selected_size: u64, fmt: SizeFormat) -> String {
    if selected > 0 {
        format!(
            "{selected} {} ({}) / {total}",
            tr("selected"),
            format_size(selected_size, fmt)
        )
    } else {
        format!("{total} {}", tr("items"))
    }
}

/// "12.3 GiB free".
pub fn free_text(free: u64, fmt: SizeFormat) -> String {
    format!("{} {}", format_size(free, fmt), tr("free"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(format_size(0, SizeFormat::Binary), "0 B");
        assert_eq!(format_size(1023, SizeFormat::Binary), "1023 B");
        assert_eq!(format_size(1024, SizeFormat::Binary), "1.0 KiB");
        assert_eq!(format_size(1536, SizeFormat::Binary), "1.5 KiB");
        assert_eq!(format_size(10 * 1024 * 1024, SizeFormat::Binary), "10 MiB");
        assert_eq!(format_size(1000, SizeFormat::Si), "1.0 kB");
        assert_eq!(format_size(999, SizeFormat::Si), "999 B");
        assert_eq!(format_size(1_234_567, SizeFormat::Bytes), "1 234 567");
        assert_eq!(format_size(12, SizeFormat::Bytes), "12");
        assert_eq!(format_size(u64::MAX, SizeFormat::Binary), "16 EiB");
    }

    #[test]
    fn format_names() {
        assert_eq!(SizeFormat::from_config("si"), SizeFormat::Si);
        assert_eq!(SizeFormat::from_config("bytes"), SizeFormat::Bytes);
        assert_eq!(SizeFormat::from_config("binary"), SizeFormat::Binary);
        assert_eq!(SizeFormat::from_config("?"), SizeFormat::Binary);
    }

    #[test]
    fn summaries() {
        assert_eq!(summary(120, 0, 0, SizeFormat::Binary), "120 items");
        assert_eq!(
            summary(120, 3, 2048, SizeFormat::Binary),
            "3 selected (2.0 KiB) / 120"
        );
        assert_eq!(
            free_text(1024 * 1024 * 1024, SizeFormat::Binary),
            "1.0 GiB free"
        );
    }

    #[test]
    fn cursor_text() {
        let fmt = StatusFormat {
            size: SizeFormat::Binary,
            time: "%Y".into(),
        };
        let up = Entry::new("..", EntryKind::Dir);
        assert_eq!(cursor_info(&up, &fmt), "Up one level");
        let mut f = Entry::new("a.txt", EntryKind::File);
        f.size = 2048;
        f.mtime_ns = 86_400 * 1_000_000_000 * 365 * 30;
        let text = cursor_info(&f, &fmt);
        assert!(text.starts_with("a.txt   2.0 KiB   "), "{text}");
        let d = Entry::new("sub", EntryKind::Dir);
        assert!(cursor_info(&d, &fmt).contains("<DIR>"));
    }

    #[test]
    fn non_utf8_name_is_shown_lossy() {
        use std::os::unix::ffi::OsStringExt;
        let e = Entry::new(
            std::ffi::OsString::from_vec(b"a\xffb".to_vec()),
            EntryKind::File,
        );
        let t = cursor_info(&e, &StatusFormat::default());
        assert!(t.starts_with("a\u{fffd}b"), "{t}");
    }
}
