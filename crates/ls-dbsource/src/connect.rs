//! Opens a real connection to a developer's live MySQL/MariaDB and lists
//! its tables. Deliberately conservative about time: a developer typing in
//! a wrong host/port, or pointing at a server behind a firewall that just
//! drops packets, must fail with a clear error in a few seconds - not hang
//! the Send wizard forever. See `CONNECT_TIMEOUT` below.

use crate::types::{ConnectionDetails, TableInfo};
use anyhow::{bail, Context, Result};
use mysql::prelude::Queryable;
use mysql::{Conn, OptsBuilder};
use std::time::Duration;

/// Finite, generous-but-bounded timeout for every network leg of talking to
/// the developer's database (initial TCP connect, and each read/write on
/// the connection). A bad host/port/firewall must fail loudly in single-
/// digit seconds, never hang the Send wizard indefinitely - the same
/// reasoning `crates/ls-snapshot/src/bundle.rs`'s `GIT_TIMEOUT` applies to
/// spawning `git`.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Builds a real `mysql::Conn` from `details`, with `CONNECT_TIMEOUT`
/// applied to connect/read/write, after rejecting anything but the one
/// engine this crate implements this round.
fn open_connection(details: &ConnectionDetails) -> Result<Conn> {
    if details.engine != "mysql" {
        bail!(
            "unsupported database engine '{}' - only \"mysql\" (MySQL/MariaDB) is implemented",
            details.engine
        );
    }

    let opts = OptsBuilder::new()
        .ip_or_hostname(Some(details.host.clone()))
        .tcp_port(details.port)
        .user(Some(details.username.clone()))
        .pass(Some(details.password.clone()))
        .db_name(Some(details.database.clone()))
        .tcp_connect_timeout(Some(CONNECT_TIMEOUT))
        .read_timeout(Some(CONNECT_TIMEOUT))
        .write_timeout(Some(CONNECT_TIMEOUT));

    Conn::new(opts).with_context(|| {
        format!(
            "failed to connect to {}@{}:{}/{}",
            details.username, details.host, details.port, details.database
        )
    })
}

/// Opens a real connection and confirms it actually works end-to-end with a
/// `SELECT 1` round trip, rather than just trusting that the TCP handshake
/// succeeded. Returns a clear error (never a panic) for a bad host, port,
/// credentials, or a connection that times out.
pub fn test_connection(details: &ConnectionDetails) -> Result<()> {
    let mut conn = open_connection(details)?;
    let value: Option<i32> = conn
        .query_first("SELECT 1")
        .context("connected, but the test query (SELECT 1) failed")?;
    if value != Some(1) {
        bail!("connected, but the test query (SELECT 1) returned an unexpected result");
    }
    Ok(())
}

/// Lists every table in `details.database`, each with a cheap approximate
/// row count sourced from `information_schema` statistics (not a real
/// `SELECT COUNT(*)`, which would be a full table scan on an unindexed
/// table). Sorted by table name so the result is deterministic regardless
/// of the server's own internal ordering.
pub fn list_tables(details: &ConnectionDetails) -> Result<Vec<TableInfo>> {
    let mut conn = open_connection(details)?;

    let rows: Vec<(String, Option<u64>)> = conn
        .exec(
            "SELECT table_name, table_rows \
             FROM information_schema.tables \
             WHERE table_schema = ? \
             ORDER BY table_name",
            (&details.database,),
        )
        .context("failed to list tables from information_schema")?;

    Ok(rows
        .into_iter()
        .map(|(name, approx_row_count)| TableInfo {
            name,
            approx_row_count,
        })
        .collect())
}
