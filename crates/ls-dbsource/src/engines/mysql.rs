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
/// applied to connect/read/write.
///
/// Round 18: a real, reproduced connection bug and its fix. A developer's
/// screenshot showed `Test connection & continue` failing with
/// `failed to connect to root@localhost:3306/xusom` on *both* Windows and
/// Linux against what looked like correct credentials. Reproduced directly
/// in this sandbox with a real MariaDB bound to `127.0.0.1` only: connecting
/// with `host = "localhost"` failed with `Connection refused (os error
/// 111)`, while `host = "127.0.0.1"` succeeded immediately with identical
/// credentials. Root cause, confirmed (not assumed) via this crate's own
/// resolution here: `getent hosts localhost` resolves to `::1` (IPv6) only
/// - a real, common dual-stack ambiguity (Windows ships the same `::1
/// localhost` entry by default) - so the `mysql` crate's initial TCP
/// connect targets `[::1]:<port>`, which a server bound to the IPv4
/// loopback address refuses outright, before any auth or database-
/// selection ever happens. This is a *different* mechanism than the other
/// classic "MySQL treats literal localhost specially" gotcha this round
/// was asked to check (the mysql crate's own `prefer_socket` default,
/// which upgrades a loopback TCP connection to a Unix socket *after* it
/// already connected) - that one has its own graceful fallback-to-TCP
/// built in (verified by reading the crate's source,
/// `Conn::new`/`can_improved`) and was not, in the end, this bug. Fixed
/// here by substituting the unambiguous loopback address for a literal
/// "localhost" before ever building the connection, sidestepping the
/// whole resolution-order question - and `prefer_socket(false)` is set
/// regardless, since this app's UX is explicitly "connect to the host and
/// port the developer typed", and silently upgrading to whatever Unix
/// socket path the server happens to report risks landing on a
/// *different* MySQL instance than the one actually listening on that
/// port if more than one is running on the same machine.
pub(crate) fn open_connection(details: &ConnectionDetails) -> Result<Conn> {
    open_connection_impl(details, true)
}

/// Round 22: same connection, minus pinning to `details.database` as the
/// session's default schema. Needed for [`list_databases`] - the whole
/// point of browsing available schemas is that `details.database` (a
/// developer-typed or auto-detected guess) might not exist yet, or might
/// be wrong, so requiring it up front would defeat the feature before it
/// could show anything. A connection that only needs host/port/user/pass
/// to succeed is also a genuinely more honest "does this connection work
/// at all" check than one that also silently depends on a specific
/// database already existing.
fn open_connection_impl(details: &ConnectionDetails, pin_database: bool) -> Result<Conn> {
    if details.engine != "mysql" {
        bail!(
            "unsupported database engine '{}' - only \"mysql\" (MySQL/MariaDB) is implemented here",
            details.engine
        );
    }

    let host = if details.host.trim().eq_ignore_ascii_case("localhost") {
        "127.0.0.1".to_string()
    } else {
        details.host.clone()
    };

    let mut builder = OptsBuilder::new()
        .ip_or_hostname(Some(host))
        .tcp_port(details.port)
        .user(Some(details.username.clone()))
        .pass(Some(details.password.clone()))
        .prefer_socket(false)
        .tcp_connect_timeout(Some(CONNECT_TIMEOUT))
        .read_timeout(Some(CONNECT_TIMEOUT))
        .write_timeout(Some(CONNECT_TIMEOUT));
    if pin_database {
        builder = builder.db_name(Some(details.database.clone()));
    }

    // Round 22: `friendly_error::connect_failure` prepends one clean,
    // human sentence (e.g. "No database server appears to be listening at
    // that host and port") in front of the exact same full raw driver
    // error round 18's `{:#}` fix already made sure survives - real click-
    // through found that surviving-but-unfiltered chain still read like
    // "DriverError { Could not connect ... }" (the mysql crate's own error
    // enum name, easily misread as "the driver is missing"), which is
    // technically not swallowed but not actually clear either.
    Conn::new(builder).map_err(|e| {
        crate::friendly_error::connect_failure(&details.username, &details.host, details.port, &details.database, e)
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

/// Round 22: the real list of databases on the server - queried right
/// after a real, successful connection (see `open_connection_impl`'s doc
/// comment on why this doesn't pin to `details.database`), so the wizard
/// can show the developer what's actually there instead of trusting a
/// typed-in name blindly. `SHOW DATABASES` needs no special privilege
/// beyond a normal login; the handful of MySQL/MariaDB-internal schemas
/// are filtered out since a developer's own project database is never
/// one of them.
pub fn list_databases(details: &ConnectionDetails) -> Result<Vec<String>> {
    let mut conn = open_connection_impl(details, false)?;
    let names: Vec<String> = conn.query("SHOW DATABASES").context("failed to list databases")?;
    const INTERNAL: &[&str] = &["information_schema", "mysql", "performance_schema", "sys"];
    let mut names: Vec<String> = names.into_iter().filter(|n| !INTERNAL.contains(&n.as_str())).collect();
    names.sort();
    Ok(names)
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
