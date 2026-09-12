//! Dispatches to the real, engine-specific export implementation in
//! [`crate::engines`]. Each engine's export uses that engine's own real
//! tooling, not a shared SQL-shaped implementation stretched to fit:
//! MySQL/MariaDB gets a hand-written `CREATE`+`INSERT` dump (round 17,
//! functionally equivalent to `mysqldump`'s own output); PostgreSQL shells
//! out to the real `pg_dump`; MongoDB shells out to the real `mongodump`
//! (whose native output is BSON, not SQL - restoring it later needs
//! `mongorestore`, never a SQL-style restore, which is exactly why this
//! isn't reimplemented by hand the way MySQL's is).

use crate::engines::{mongo, mysql_dump, postgres};
use crate::types::{ConnectionDetails, SUPPORTED_ENGINES};
use anyhow::{bail, Result};

/// Full (never sampled/limited) export of exactly `tables` (or, for
/// MongoDB, collections), via whichever engine `details.engine` names.
pub fn export_tables(details: &ConnectionDetails, tables: &[String]) -> Result<Vec<u8>> {
    match details.engine.as_str() {
        "mysql" => mysql_dump::export_tables(details, tables),
        "postgres" => postgres::export_tables(details, tables),
        "mongodb" => mongo::export_tables(details, tables),
        other => bail!(
            "unsupported database engine '{other}' - supported engines are: {}",
            SUPPORTED_ENGINES.join(", ")
        ),
    }
}
