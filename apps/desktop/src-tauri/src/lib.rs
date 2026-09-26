//! Library target, existing alongside `main.rs`'s binary target for one
//! reason: `tests/send_flow_test.rs` needs to call `commands::share_snapshot`
//! / `commands::receive_snapshot` directly (the actual Tauri command
//! functions, not the lower-level crate calls `pipeline_test.rs` already
//! exercises), and an integration test can only reach a crate's `pub`
//! surface through its library target — a binary-only crate has none.
//! `main.rs` uses these same modules via this lib rather than declaring its
//! own copies.

pub mod commands;
pub mod compose_wizard;
pub mod project_session;
pub mod session_commands;
pub mod send_log;
pub mod session_history;
pub mod state;
