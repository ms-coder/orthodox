//! ofm-core
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

#[cfg(test)]
mod tests {
    #[test]
    fn dry_run() {
        assert_eq!(1 + 1, 2);
    }
}
