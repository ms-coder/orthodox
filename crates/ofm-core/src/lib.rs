//! ofm-core: UI-agnostic domain types and logic (no GTK, no I/O except where documented).
//!
//! File ownership for parallel tasks (see docs/PLAN.md):
//! - T1.1: `panel/**`, `mask.rs`
//! - T1.3: `keychord.rs` (parser/formatter bodies only)
//! - M4: `cmdline.rs`
//! - frozen API (architect only): everything else.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod batch_rename;
pub mod cancel;
pub mod cmdline;
pub mod command;
pub mod commands;
pub mod compare;
pub mod entry;
pub mod keychord;
pub mod location;
pub mod mask;
pub mod menu_expand;
pub mod panel;
pub mod responder;

pub use cancel::CancelToken;
pub use command::{CommandId, CommandInfo, KeyContext};
pub use commands::cmd;
pub use entry::{Entry, EntryKind, TargetKind};
pub use keychord::{Key, KeyChord, KeyChordParseError, Mods};
pub use location::{ListId, Location, LocationKind};
pub use responder::{Pending, Responder, Wait};
