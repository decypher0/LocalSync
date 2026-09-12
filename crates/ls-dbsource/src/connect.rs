//! Dispatches to the real, engine-specific implementation in
//! [`crate::engines`] - the only two operations anything outside this
//! crate needs before deciding what to export: "does this connection
//! actually work" and "what's really in there".

use crate::engines::{mongo, mysql, postgres};
use crate::types::{ConnectionDetails, TableInfo, SUPPORTED_ENGINES};
use anyhow::{bail, Result};

fn unsupported_engine(engine: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "unsupported database engine '{engine}' - supported engines are: {}",
        SUPPORTED_ENGINES.join(", ")
    )
}

/// Opens a real connection and confirms it actually works end-to-end
/// (a real round-trip query, not just a TCP handshake), for whichever
/// engine `details.engine` names. Returns a clear error (never a panic,
/// never a silent fallback to a different engine's logic) for a bad host,
/// port, credentials, missing database, or an unsupported engine name.
pub fn test_connection(details: &ConnectionDetails) -> Result<()> {
    match details.engine.as_str() {
        "mysql" => mysql::test_connection(details),
        "postgres" => postgres::test_connection(details),
        "mongodb" => mongo::test_connection(details),
        other => bail!(unsupported_engine(other)),
    }
}

/// Lists every table (or, for MongoDB, collection) in `details.database`,
/// with a cheap-to-fetch approximate count so the developer can make an
/// informed selection before anything is exported.
pub fn list_tables(details: &ConnectionDetails) -> Result<Vec<TableInfo>> {
    match details.engine.as_str() {
        "mysql" => mysql::list_tables(details),
        "postgres" => postgres::list_tables(details),
        "mongodb" => mongo::list_tables(details),
        other => bail!(unsupported_engine(other)),
    }
}

/// Round 22 goal 3: the real list of databases/schemas on the server -
/// queried right after a real, successful connection, deliberately without
/// requiring `details.database` to already be a real, existing database
/// (each engine's own `list_databases` connects in whatever way that
/// engine allows without pinning to it - see each one's doc comment). The
/// wizard uses this to let the developer pick the real, correct
/// database/schema instead of trusting a typed-in or auto-detected name
/// blindly.
pub fn list_databases(details: &ConnectionDetails) -> Result<Vec<String>> {
    match details.engine.as_str() {
        "mysql" => mysql::list_databases(details),
        "postgres" => postgres::list_databases(details),
        "mongodb" => mongo::list_databases(details),
        other => bail!(unsupported_engine(other)),
    }
}
