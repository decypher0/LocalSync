use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Everything needed to open a real connection to a developer's database.
/// Comes either from [`crate::detect`] parsing a project's own config, or
/// from the developer typing it in by hand when detection fails.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionDetails {
    /// Only "mysql" is actually exercised end-to-end this round (the
    /// developer's real case). Kept as a String rather than an enum so a
    /// future engine can be added without a breaking wire-format change -
    /// `connect`/`export` reject anything but "mysql" today with a clear
    /// error rather than silently misinterpreting it.
    pub engine: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: String,
}

/// The result of successfully parsing connection details out of a project's
/// own config file - shown to the developer before anything is used, so
/// they can see exactly where these values came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedConnection {
    pub details: ConnectionDetails,
    /// Path to the config file the details were parsed from, e.g.
    /// ".../src/main/resources/application.properties" - shown to the
    /// developer so "auto-detected" is never a black box.
    pub source_file: PathBuf,
}

/// One real table from the developer's live database, with a cheap-to-fetch
/// summary so they can make an informed selection before anything is
/// exported. `approx_row_count` comes from `information_schema` statistics
/// (fast, always available) rather than a real `SELECT COUNT(*)` (exact but
/// a full table scan on an unindexed table) - explicitly an estimate, not a
/// promise of exactness, which is fine for "give the developer a sense of
/// what's in here" and does not affect what actually gets exported (the
/// export itself is always the full, real table content, never sampled).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableInfo {
    pub name: String,
    pub approx_row_count: Option<u64>,
}
