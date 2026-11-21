//! Settings of the "compare directories" dialog (F27) and their translation to the engine's
//! [`CompareOptions`]. Pure: no GTK, unit-tested.

use std::time::Duration;

use ofm_core::compare::{CompareMode, CompareOptions};

use crate::i18n::tr;

/// Default mtime tolerance in seconds (FAT stores 2-second timestamps).
pub const DEFAULT_TOLERANCE_SECS: u32 = 2;
/// Largest accepted mtime tolerance (one day).
pub const MAX_TOLERANCE_SECS: u32 = 86_400;

/// Which of the differing entries are marked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MarkFilter {
    /// Everything that differs (missing, size, time, content, type).
    #[default]
    AllDiffering,
    /// Only entries that are newer than their counterpart on the other side.
    NewerHere,
    /// Only entries that do not exist on the other side.
    MissingOnOther,
}

/// Everything the dialog asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareSettings {
    /// What is compared for entries present on both sides.
    pub mode: CompareMode,
    /// Match names case-insensitively.
    pub case_insensitive: bool,
    /// Subdirectories take part (compared by name only: there is no recursive comparison).
    pub include_dirs: bool,
    /// Modification-time tolerance for [`CompareMode::NameSizeMtime`].
    pub tolerance: Duration,
    /// Which differences end up selected.
    pub mark: MarkFilter,
}

impl Default for CompareSettings {
    fn default() -> Self {
        CompareSettings {
            mode: CompareMode::NameSizeMtime,
            case_insensitive: false,
            include_dirs: true,
            tolerance: Duration::from_secs(u64::from(DEFAULT_TOLERANCE_SECS)),
            mark: MarkFilter::AllDiffering,
        }
    }
}

impl CompareSettings {
    /// Engine options for these settings.
    pub fn to_options(&self) -> CompareOptions {
        CompareOptions {
            mode: self.mode,
            mtime_tolerance: self.tolerance,
            case_insensitive: self.case_insensitive,
            include_dirs: self.include_dirs,
            ignore_dirs: false,
        }
    }
}

/// Parse the tolerance field: a non-negative number of seconds (`2`, `0`, `1.5`), at most
/// [`MAX_TOLERANCE_SECS`]. Surrounding blanks are ignored; an empty field means the default.
pub fn parse_tolerance(text: &str) -> Result<Duration, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(Duration::from_secs(u64::from(DEFAULT_TOLERANCE_SECS)));
    }
    let bad = || tr("The time tolerance must be a number of seconds.").to_owned();
    let secs: f64 = t.replace(',', ".").parse().map_err(|_| bad())?;
    if !secs.is_finite() || secs < 0.0 {
        return Err(bad());
    }
    if secs > f64::from(MAX_TOLERANCE_SECS) {
        return Err(tr("The time tolerance is at most 86400 seconds.").to_owned());
    }
    Ok(Duration::from_secs_f64(secs))
}

/// Tolerance as the text shown in the field (whole seconds without a fraction).
pub fn format_tolerance(d: Duration) -> String {
    if d.subsec_nanos() == 0 {
        d.as_secs().to_string()
    } else {
        format!("{}", d.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_spec() {
        let s = CompareSettings::default();
        assert_eq!(s.mode, CompareMode::NameSizeMtime);
        assert_eq!(s.tolerance, Duration::from_secs(2));
        assert!(!s.case_insensitive);
        assert!(s.include_dirs);
        assert_eq!(s.mark, MarkFilter::AllDiffering);
        assert_eq!(s.to_options(), CompareOptions::default());
    }

    #[test]
    fn tolerance_parsing() {
        assert_eq!(parse_tolerance("2"), Ok(Duration::from_secs(2)));
        assert_eq!(parse_tolerance(" 0 "), Ok(Duration::ZERO));
        assert_eq!(parse_tolerance("1.5"), Ok(Duration::from_millis(1500)));
        assert_eq!(parse_tolerance("1,5"), Ok(Duration::from_millis(1500)));
        assert_eq!(parse_tolerance(""), Ok(Duration::from_secs(2)));
        assert_eq!(parse_tolerance("86400"), Ok(Duration::from_secs(86_400)));
        for bad in ["abc", "-1", "NaN", "inf", "1e400", "86401", "2s"] {
            assert!(parse_tolerance(bad).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn tolerance_roundtrips_through_the_field_text() {
        for d in [
            Duration::ZERO,
            Duration::from_secs(2),
            Duration::from_millis(250),
        ] {
            assert_eq!(parse_tolerance(&format_tolerance(d)), Ok(d));
        }
    }

    #[test]
    fn settings_map_to_engine_options() {
        let s = CompareSettings {
            mode: CompareMode::Content,
            case_insensitive: true,
            include_dirs: false,
            tolerance: Duration::from_secs(7),
            mark: MarkFilter::NewerHere,
        };
        let o = s.to_options();
        assert_eq!(o.mode, CompareMode::Content);
        assert!(o.case_insensitive);
        assert!(!o.include_dirs);
        assert_eq!(o.mtime_tolerance, Duration::from_secs(7));
    }
}
