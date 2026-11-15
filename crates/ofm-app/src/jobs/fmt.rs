//! Text formatting for job progress (jobs view, status strip). Pure functions.

use ofm_jobs::{JobState, Outcome, Progress};

use crate::chrome::statusline::{SizeFormat, format_size};
use crate::i18n::tr;

/// Fraction done in `0.0..=1.0`, `None` while the total is unknown (planning).
pub fn fraction(p: &Progress) -> Option<f64> {
    if p.planning || p.bytes_total == 0 {
        if !p.planning && p.files_total > 0 {
            return Some((p.files_done as f64 / p.files_total as f64).clamp(0.0, 1.0));
        }
        return None;
    }
    Some((p.bytes_done as f64 / p.bytes_total as f64).clamp(0.0, 1.0))
}

/// `1:05`, `1:02:03`.
pub fn duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// `12 MiB/s`.
pub fn speed(bytes_per_sec: u64) -> String {
    format!("{}/s", format_size(bytes_per_sec, SizeFormat::Binary))
}

/// One-line detail: `4.5 MiB of 120 MiB · 12 MiB/s · ETA 0:32 · file 3 of 10`.
pub fn details(p: &Progress) -> String {
    if p.planning {
        return format!(
            "{} ({} {}, {})",
            tr("Scanning..."),
            p.files_total,
            tr("files"),
            format_size(p.bytes_total, SizeFormat::Binary)
        );
    }
    let mut parts = Vec::new();
    if p.bytes_total > 0 {
        parts.push(format!(
            "{} {} {}",
            format_size(p.bytes_done, SizeFormat::Binary),
            tr("of"),
            format_size(p.bytes_total, SizeFormat::Binary)
        ));
    }
    if p.bytes_per_sec > 0 {
        parts.push(speed(p.bytes_per_sec));
    }
    if let Some(eta) = p.eta_secs {
        parts.push(format!("{} {}", tr("ETA"), duration(eta)));
    }
    if p.files_total > 1 {
        parts.push(format!(
            "{} {} {} {}",
            tr("file"),
            (p.files_done + 1).min(p.files_total),
            tr("of"),
            p.files_total
        ));
    }
    parts.join(" \u{b7} ")
}

/// Short state word for a job row.
pub fn state_text(state: JobState, asking: bool) -> &'static str {
    match (state, asking) {
        (JobState::Queued, _) => tr("Queued"),
        (JobState::Running, true) => tr("Waiting for your answer"),
        (JobState::Running, false) => tr("Running"),
        (JobState::Paused, _) => tr("Paused"),
        (JobState::Finished, _) => tr("Finished"),
    }
}

/// Result line of a finished job.
pub fn outcome_text(o: &Outcome) -> String {
    match o {
        Outcome::Ok(r) => {
            let mut s = format!(
                "{} \u{b7} {} {} \u{b7} {}",
                tr("Done"),
                r.files_done,
                tr("files"),
                format_size(r.bytes_done, SizeFormat::Binary)
            );
            if r.skipped > 0 {
                s.push_str(&format!(" \u{b7} {} {}", r.skipped, tr("skipped")));
            }
            if !r.warnings.is_empty() {
                s.push_str(&format!(" \u{b7} {} {}", r.warnings.len(), tr("warnings")));
            }
            s
        }
        Outcome::Cancelled(r) => format!(
            "{} \u{b7} {} {}",
            tr("Cancelled"),
            r.files_done,
            tr("files done")
        ),
        Outcome::Failed { error, .. } => format!("{}: {error}", tr("Failed")),
        Outcome::Size(t) => format!(
            "{} \u{b7} {} {} \u{b7} {} {}",
            format_size(t.bytes, SizeFormat::Binary),
            t.files,
            tr("files"),
            t.dirs,
            tr("folders")
        ),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration(5), "0:05");
        assert_eq!(duration(65), "1:05");
        assert_eq!(duration(3723), "1:02:03");
    }

    #[test]
    fn fractions() {
        let mut p = Progress {
            bytes_done: 50,
            bytes_total: 200,
            ..Progress::default()
        };
        assert_eq!(fraction(&p), Some(0.25));
        p.planning = true;
        assert_eq!(fraction(&p), None);
        p.planning = false;
        p.bytes_total = 0;
        p.files_total = 4;
        p.files_done = 1;
        assert_eq!(fraction(&p), Some(0.25));
        p.files_total = 0;
        assert_eq!(fraction(&p), None);
        p.bytes_total = 10;
        p.bytes_done = 99;
        assert_eq!(fraction(&p), Some(1.0));
    }

    #[test]
    fn details_line() {
        let p = Progress {
            bytes_done: 1024,
            bytes_total: 4096,
            files_done: 2,
            files_total: 5,
            bytes_per_sec: 2048,
            eta_secs: Some(75),
            ..Progress::default()
        };
        assert_eq!(
            details(&p),
            "1.0 KiB of 4.0 KiB \u{b7} 2.0 KiB/s \u{b7} ETA 1:15 \u{b7} file 3 of 5"
        );
        let scanning = Progress {
            planning: true,
            files_total: 7,
            ..Progress::default()
        };
        assert!(details(&scanning).starts_with("Scanning..."));
    }
}
