//! Round 17: the Send-side database-source wizard's backend. Three jobs,
//! kept as separate modules so each is independently testable:
//!
//! - [`detect`]: find connection details in a project's own config
//!   (Spring Boot's `application.properties`/`.yml` today), so the
//!   developer is never asked to type in something already written down.
//! - [`connect`]: open a real connection and list the real tables/row
//!   counts, so a table-selection choice is informed, not blind.
//! - [`export`]: turn a developer-selected set of tables into a real,
//!   non-sampled SQL dump.
//!
//! Deliberately has zero dependency on `ls-snapshot` - this crate only
//! knows about "a database", not about snapshots/manifests/tar payloads.
//! `apps/desktop/src-tauri`'s command layer is what wires this crate's
//! output into `ls_snapshot::PendingDump`.

pub mod connect;
pub mod detect;
pub mod export;
mod types;

pub use types::{ConnectionDetails, DetectedConnection, TableInfo};
