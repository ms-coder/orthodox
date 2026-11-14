//! Translation hook. Every user-facing string goes through [`tr`] so German/Russian can be added
//! after v1.0 without touching call sites. For now it is the identity function.

/// Translate a user-facing string (identity in v1.0).
pub fn tr(msg: &'static str) -> &'static str {
    msg
}
