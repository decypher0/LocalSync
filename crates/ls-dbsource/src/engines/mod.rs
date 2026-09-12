//! One module per supported database engine, each implementing the same
//! three operations (`test_connection`, `list_tables`, `export_tables`)
//! against the real, engine-specific tooling - never a silent fallback to
//! another engine's implementation. `crate::connect`/`crate::export` are
//! the only things outside this module that call into these; they just
//! dispatch on `ConnectionDetails.engine`.

pub mod mongo;
pub mod mysql;
pub mod mysql_dump;
pub mod postgres;
