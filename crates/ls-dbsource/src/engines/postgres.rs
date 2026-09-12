//! Real PostgreSQL support: a native connection (the `postgres` crate,
//! the same blocking-client shape `engines::mysql` already uses) for
//! connect-test/list_tables, and the real `pg_dump` binary for export -
//! not a hand-rolled SQL dump the way MySQL's is. `pg_dump` already knows
//! how to correctly serialize every real Postgres type (arrays, JSONB,
//! ranges, enums, ...) byte-for-byte; reimplementing that by hand would be
//! both a lot of surface area and a real correctness risk for anything
//! this crate's own author didn't think to test. Restoring one of these
//! dumps later is a plain `psql < dump.sql` (plain-text SQL format, same
//! restore shape as MySQL's own dump), unlike MongoDB's binary `mongodump`
//! archives.

use crate::types::{ConnectionDetails, TableInfo};
use anyhow::{bail, Context, Result};
use postgres::{Client, NoTls};
use std::process::Command;
use std::time::Duration;

/// Same reasoning as `engines::mysql::CONNECT_TIMEOUT`: a bad host/port/
/// firewall must fail loudly in single-digit seconds, never hang the Send
/// wizard indefinitely. Only applied to the connection itself
/// (test_connection/list_tables) - `export_tables` shells out to
/// `pg_dump`, whose own runtime is proportional to real data size and has
/// no sane fixed bound to impose (unlike a connectivity check, which
/// should be fast-or-fail).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Round 18: same localhost fix as `engines::mysql` - see that module's
/// doc comment for the real, reproduced root cause (a literal "localhost"
/// can resolve to the IPv6 loopback `::1` while the server only listens on
/// the IPv4 loopback, or vice versa - an OS/DNS-config-dependent ambiguity,
/// not a credentials problem). Substituting the unambiguous address
/// sidesteps it entirely, for this engine too.
fn resolve_host(details: &ConnectionDetails) -> String {
    if details.host.trim().eq_ignore_ascii_case("localhost") {
        "127.0.0.1".to_string()
    } else {
        details.host.clone()
    }
}

fn open_connection(details: &ConnectionDetails) -> Result<Client> {
    if details.engine != "postgres" {
        bail!(
            "unsupported database engine '{}' - only \"postgres\" is implemented here",
            details.engine
        );
    }

    let mut config = postgres::Config::new();
    config
        .host(&resolve_host(details))
        .port(details.port)
        .user(&details.username)
        .password(&details.password)
        .dbname(&details.database)
        .connect_timeout(CONNECT_TIMEOUT);

    // Round 18: same error-swallowing fix as MySQL's - the real underlying
    // reason (connection refused, wrong password, unknown database, ...)
    // must survive into the UI, not just this outer "failed to connect"
    // wrapper. commands.rs converts this anyhow::Error to a String with
    // `{:#}` (the full chain), not `.to_string()` (only the outermost
    // context) - verified against real failures, same as MySQL's.
    config.connect(NoTls).with_context(|| {
        format!(
            "failed to connect to {}@{}:{}/{}",
            details.username, details.host, details.port, details.database
        )
    })
}

/// Opens a real connection and confirms it actually works with a real
/// `SELECT 1` round trip.
pub fn test_connection(details: &ConnectionDetails) -> Result<()> {
    let mut client = open_connection(details)?;
    let row = client
        .query_one("SELECT 1", &[])
        .context("connected, but the test query (SELECT 1) failed")?;
    let value: i32 = row.get(0);
    if value != 1 {
        bail!("connected, but the test query (SELECT 1) returned an unexpected result");
    }
    Ok(())
}

/// Lists every real table in the database's own (non-system) schemas, with
/// a cheap approximate row count from `pg_class.reltuples` - Postgres's own
/// planner statistic, the same "estimate, not an expensive full scan"
/// tradeoff `engines::mysql::list_tables` makes with
/// `information_schema.tables.table_rows`. `reltuples` is `-1` for a table
/// that's never been analyzed (including one that was just created and is
/// still empty) - reported as `None` rather than a misleading "-1 rows".
pub fn list_tables(details: &ConnectionDetails) -> Result<Vec<TableInfo>> {
    let mut client = open_connection(details)?;
    let rows = client
        .query(
            "SELECT c.relname, c.reltuples::bigint \
             FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind = 'r' \
               AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
             ORDER BY c.relname",
            &[],
        )
        .context("failed to list tables from pg_catalog")?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let name: String = row.get(0);
            let reltuples: i64 = row.get(1);
            TableInfo {
                name,
                approx_row_count: if reltuples < 0 { None } else { Some(reltuples as u64) },
            }
        })
        .collect())
}

/// A name passed to `pg_dump --table=` unquoted is matched as a *pattern*
/// (supports `*`/`?` wildcards) rather than an exact identifier - quoting
/// it (per `pg_dump`'s own documented convention) forces an exact match.
/// A literal double-quote in the name would break that quoting, so it's
/// rejected up front - the same "this is a real correctness/safety
/// boundary, not decoration" reasoning `engines::mysql_dump`'s backtick
/// check documents.
fn quoted_table_arg(name: &str) -> Result<String> {
    if name.contains('"') {
        bail!("table name '{name}' contains a double quote and cannot be safely passed to pg_dump - refusing to export it");
    }
    Ok(format!("\"{name}\""))
}

/// Full (never sampled/limited) export of exactly `tables`, via the real
/// `pg_dump` binary - plain-text SQL format, restorable with a plain
/// `psql < dump.sql`. Every selected table is passed as its own
/// `--table=` argument (`pg_dump` supports repeating this flag; all
/// matches are included in one dump).
pub fn export_tables(details: &ConnectionDetails, tables: &[String]) -> Result<Vec<u8>> {
    if details.engine != "postgres" {
        bail!(
            "unsupported database engine '{}' - only \"postgres\" is implemented here",
            details.engine
        );
    }

    let mut cmd = Command::new("pg_dump");
    cmd.arg("--host")
        .arg(resolve_host(details))
        .arg("--port")
        .arg(details.port.to_string())
        .arg("--username")
        .arg(&details.username)
        .arg("--no-password") // never block on an interactive password prompt
        .arg("--format=plain")
        .arg("--no-owner")
        .arg("--no-privileges")
        .env("PGPASSWORD", &details.password)
        // Never inherit stdin - same hardening reasoning as
        // ls-snapshot/bundle.rs's git_bytes: a GUI app has no controlling
        // terminal, so any child that unexpectedly waits on stdin must not
        // be able to hang this call forever.
        .stdin(std::process::Stdio::null());

    for name in tables {
        cmd.arg("--table").arg(quoted_table_arg(name)?);
    }
    cmd.arg(&details.database);

    let output = cmd
        .output()
        .context("failed to run pg_dump - is it installed and on PATH?")?;
    if !output.status.success() {
        bail!(
            "pg_dump failed (status {}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}
