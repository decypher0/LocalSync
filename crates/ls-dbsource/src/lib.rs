//! Rounds 17-18: the Send-side database-source wizard's backend. Kept as
//! separate modules so each is independently testable:
//!
//! - [`detect`]: find connection details in a project's own config
//!   (Spring Boot's `application.properties`/`.yml` today), so the
//!   developer is never asked to type in something already written down.
//! - [`engines`]: one real, working implementation per supported database
//!   engine (MySQL/MariaDB, PostgreSQL, MongoDB) - genuinely different
//!   tooling per engine (a native driver crate for MySQL/PostgreSQL's own
//!   connect/list; `pg_dump`/`mongodump` for PostgreSQL/MongoDB's export;
//!   MongoDB's own driver for its connect/list), never a fallback to
//!   another engine's logic for one that isn't implemented.
//! - [`connect`]/[`export`]: thin dispatchers over `engines`, keyed by
//!   `ConnectionDetails.engine` - the only two entry points anything
//!   outside this crate needs to call.
//!
//! Deliberately has zero dependency on `ls-snapshot` - this crate only
//! knows about "a database", not about snapshots/manifests/tar payloads.
//! `apps/desktop/src-tauri`'s command layer is what wires this crate's
//! output into `ls_snapshot::PendingDump`.

pub mod connect;
pub mod detect;
mod engines;
pub mod export;
pub mod friendly_error;
mod types;

pub use types::{ConnectionDetails, DetectedConnection, TableInfo, SUPPORTED_ENGINES};
