//! ofm-app: the GTK 4 / libadwaita front end of OFM. Everything GTK lives in this crate
//! (CLAUDE.md invariant 1); the main thread never does file I/O (invariant 2).
//!
//! The crate is a library (so integration tests in `tests/` can drive it through
//! [`context::AppContext::execute`]) plus the thin binary `ofm` (`main.rs`).
//!
//! Read `OWNERSHIP.md` next to `Cargo.toml` before editing: parallel tasks own disjoint files,
//! and the "frozen" files below are changed only by the architect/lead.
//!
//! Module map (frozen core = T2.0; owner of the rest in brackets):
//! - `app`, `context`, `dispatch`, `keys`, `hooks`, `signal`, `wiring`, `i18n`, `panel`: frozen core
//! - `widgets::file_list`: T2.1
//! - `window`, `theme`, `chrome`: T2.2
//! - `navigation`, `dialogs::mask`, `dialogs::hotlist`: T2.3
//! - `jobs`, `dialogs::{copy,conflict,delete,mkdir,link,properties}`: T3.3
//! - `terminal`: T4.1 (ADR-0003), `cmdline`: T4.2 (ADR-0003)
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod app;
pub mod chrome;
pub mod cmdline;
pub mod compare;
pub mod context;
pub mod desktop;
pub mod dialogs;
pub mod dispatch;
pub mod hooks;
pub mod i18n;
pub mod jobs;
pub mod keys;
pub mod navigation;
pub mod panel;
pub mod quickview;
pub mod signal;
pub mod terminal;
pub mod theme;
pub mod viewer;
pub mod widgets;
pub mod window;
pub mod wiring;
