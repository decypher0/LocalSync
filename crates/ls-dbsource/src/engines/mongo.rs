//! Real MongoDB support: the mongodb driver's own blocking (`sync`)
//! client for connect-test/`list_tables` (MongoDB calls these
//! "collections" - the shared `test_connection`/`list_tables`/
//! `export_tables` names are this crate's dispatch interface across all
//! three engines, not renamed per engine), and the real `mongodump`/
//! `mongorestore` binaries for export/restore - not a hand-rolled BSON
//! dump the way MySQL's is. Same reasoning `engines::postgres`'s module
//! doc comment gives for using `pg_dump`: BSON dump serialization is
//! exactly the kind of thing worth delegating to the canonical tool
//! rather than reimplementing by hand.
//!
//! `mongodump` can only target *one* collection (`--collection=<name>`)
//! or a whole database (no `--collection` at all) per invocation - it has
//! no "dump exactly these N collections" mode. So `export_tables` runs
//! `mongodump` once per selected collection, all writing into one shared
//! temp directory via `--out=<dir>` (directory mode, not `--archive`),
//! then tars+gzips that whole directory into the single `Vec<u8>` blob
//! this crate's `export_tables` interface expects (matching MySQL's/
//! PostgreSQL's own single-blob return shape). Restoring one of these
//! archives later means: untar it, then run `mongorestore <extracted-dir>`
//! (directory-mode restore - NOT `mongorestore --archive`, since these
//! were produced with `--out`, not `--archive`).

use crate::types::{ConnectionDetails, TableInfo};
use anyhow::{bail, Context, Result};
use mongodb::bson::{doc, Document};
use mongodb::options::{ClientOptions, Credential, ServerAddress};
use mongodb::sync::Client;
use std::process::Command;
use std::time::Duration;

/// Same reasoning as `engines::mysql::CONNECT_TIMEOUT`/
/// `engines::postgres::CONNECT_TIMEOUT`: a bad host/port must fail loudly
/// in single-digit seconds, never hang the Send wizard indefinitely.
/// Applied to both `connect_timeout` (bounds each individual TCP connect
/// attempt) and `server_selection_timeout` (bounds how long the driver
/// will keep retrying server discovery before giving up entirely, which
/// is what actually determines how long a *totally* unreachable host
/// hangs for - its own default is 30s, verified by reading
/// `ClientOptions`'s doc comment in `mongodb-3.9.1/src/client/options.rs`,
/// which is exactly the kind of multi-second hang this round's other two
/// engines already fixed for their own connect paths). The mongodb sync
/// driver builds its `Client` lazily (verified by reading
/// `mongodb::sync::Client::with_options`'s implementation - it never
/// touches the network itself), so both timeouts only actually bite on
/// the first real command run against it, which is exactly what
/// `test_connection`'s `ping` triggers below.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Round 18: same localhost fix as `engines::mysql`/`engines::postgres` -
/// see `engines::mysql`'s doc comment for the real, reproduced root cause
/// (a literal "localhost" can resolve to the IPv6 loopback `::1` while the
/// server only listens on the IPv4 loopback, or vice versa - an OS/DNS-
/// config-dependent ambiguity, not a credentials problem). Substituting
/// the unambiguous address sidesteps it entirely, for this engine too.
fn resolve_host(details: &ConnectionDetails) -> String {
    if details.host.trim().eq_ignore_ascii_case("localhost") {
        "127.0.0.1".to_string()
    } else {
        details.host.clone()
    }
}

/// Builds a real `mongodb::sync::Client` from `details`'s individual
/// fields - never requires the caller to already have assembled a
/// `mongodb://` URI. No credential is attached when `username` is empty,
/// so this also works against a disposable, no-auth test server (or a
/// developer's local dev database that simply has auth disabled).
fn open_client(details: &ConnectionDetails) -> Result<Client> {
    if details.engine != "mongodb" {
        bail!(
            "unsupported database engine '{}' - only \"mongodb\" is implemented here",
            details.engine
        );
    }

    let credential = if details.username.is_empty() {
        None
    } else {
        Some(
            Credential::builder()
                .username(details.username.clone())
                .password(details.password.clone())
                .build(),
        )
    };

    let options = ClientOptions::builder()
        .hosts(vec![ServerAddress::Tcp {
            host: resolve_host(details),
            port: Some(details.port),
        }])
        .credential(credential)
        .connect_timeout(CONNECT_TIMEOUT)
        .server_selection_timeout(CONNECT_TIMEOUT)
        // This app's UX is explicitly "connect to the host/port the
        // developer typed" (same reasoning `engines::mysql::open_connection`
        // documents for its own `prefer_socket(false)`) - direct_connection
        // skips replica-set/topology autodiscovery, which is both
        // unnecessary for a single dev-database target and would otherwise
        // add its own extra round trips before `server_selection_timeout`
        // even starts mattering.
        .direct_connection(true)
        .build();

    Client::with_options(options).context("failed to build MongoDB client from connection details")
}

/// Opens a real client and confirms it actually works with a real `ping`
/// round trip against `details.database` - not just "the Client object
/// was constructed without erroring" (the driver builds its `Client`
/// lazily and does no real network I/O until the first command, per
/// `open_client`'s doc comment).
pub fn test_connection(details: &ConnectionDetails) -> Result<()> {
    let client = open_client(details)?;

    // Round 18: same error-surfacing fix as `engines::mysql`/
    // `engines::postgres` - the real underlying reason (server
    // unreachable, auth failure, timeout, ...) must survive in the error
    // chain, not get swallowed into one generic message. commands.rs
    // converts this anyhow::Error to a String with `{:#}` (the full
    // chain), not `.to_string()`.
    client
        .database(&details.database)
        .run_command(doc! { "ping": 1 })
        .run()
        .with_context(|| {
            format!(
                "failed to connect to {}@{}:{}/{}",
                details.username, details.host, details.port, details.database
            )
        })?;
    Ok(())
}

/// Lists every real collection in `details.database`, with a cheap
/// approximate document count per collection from
/// `estimated_document_count` - MongoDB's own metadata-based estimate
/// (not a real full-scan `count_documents`), the same "estimate, not a
/// promise of exactness" tradeoff `TableInfo::approx_row_count`'s own doc
/// comment establishes for the other two engines. If the estimate call
/// fails for a given collection (e.g. it's actually a view, which has no
/// meaningful stored document count), `None` is reported rather than
/// falling back to a slow, exact `count_documents` scan - consistent with
/// that same doc comment's stated tradeoff. Sorted by name for
/// determinism, matching `engines::mysql`/`engines::postgres`.
pub fn list_tables(details: &ConnectionDetails) -> Result<Vec<TableInfo>> {
    let client = open_client(details)?;
    let db = client.database(&details.database);

    let mut names = db.list_collection_names().run().with_context(|| {
        format!(
            "failed to list collections from {}@{}:{}/{}",
            details.username, details.host, details.port, details.database
        )
    })?;
    names.sort();

    Ok(names
        .into_iter()
        .map(|name| {
            let approx_row_count = db
                .collection::<Document>(&name)
                .estimated_document_count()
                .run()
                .ok();
            TableInfo {
                name,
                approx_row_count,
            }
        })
        .collect())
}

/// `mongodump --collection` takes the name as a literal argv value via
/// `Command::arg` - no shell is ever involved, and (unlike `pg_dump -t`,
/// see `engines::postgres::quoted_table_arg`'s doc comment) it does no
/// pattern-matching that quoting would need to defeat. The one real risk
/// worth guarding here is a name that could be misread as a *flag* rather
/// than a positional/option value by mongodump's own arg parser if it
/// happens to start with `-` - rejected up front rather than trusting
/// `--collection <name>` syntax to always save us.
fn validate_collection_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("collection name must not be empty");
    }
    if name.starts_with('-') {
        bail!(
            "collection name '{name}' starts with '-' and could be misread as a command-line flag - refusing to export it"
        );
    }
    Ok(())
}

/// Full (never sampled/limited) export of exactly `tables` (MongoDB
/// collections), via the real `mongodump` binary, one invocation per
/// collection (see the module doc comment for why), all writing into one
/// shared temp directory. That directory is then tarred+gzipped into the
/// single returned blob.
///
/// Deliberately no fixed timeout on the `mongodump` subprocesses - same
/// reasoning `engines::postgres::export_tables` documents for `pg_dump`:
/// a connectivity check should be fast-or-fail (`CONNECT_TIMEOUT` above),
/// but an actual data export's runtime is proportional to real data size
/// and has no sane fixed bound to impose.
pub fn export_tables(details: &ConnectionDetails, tables: &[String]) -> Result<Vec<u8>> {
    if details.engine != "mongodb" {
        bail!(
            "unsupported database engine '{}' - only \"mongodb\" is implemented here",
            details.engine
        );
    }
    for name in tables {
        validate_collection_name(name)?;
    }

    let tempdir = tempfile::tempdir().context("create temp directory for mongodump output")?;
    let host = resolve_host(details);

    for name in tables {
        let mut cmd = Command::new("mongodump");
        cmd.arg("--host")
            .arg(&host)
            .arg("--port")
            .arg(details.port.to_string())
            .arg("--db")
            .arg(&details.database)
            .arg("--collection")
            .arg(name)
            .arg("--out")
            .arg(tempdir.path())
            // Never inherit stdin - same hardening reasoning as
            // `engines::postgres::export_tables`/ls-snapshot's git_bytes:
            // a GUI app has no controlling terminal, so a child that
            // unexpectedly waits on stdin (e.g. an interactive password
            // prompt) must not be able to hang this call forever.
            .stdin(std::process::Stdio::null());

        if !details.username.is_empty() {
            cmd.arg("--username").arg(&details.username);
            // mongodump only supports a password via a plain
            // `--password=<value>` CLI arg (or an interactive prompt,
            // which would hang this call given the closed stdin above) -
            // an accepted limitation of the tool itself, same as
            // `pg_dump`'s `--username` being a plain visible process
            // argument too.
            cmd.arg(format!("--password={}", details.password));
        }

        let output = cmd.output().with_context(|| {
            format!("failed to run mongodump for collection '{name}' - is it installed and on PATH?")
        })?;
        if !output.status.success() {
            bail!(
                "mongodump failed for collection '{name}' (status {}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
    }

    // Tar+gzip the whole temp directory (every selected collection's real
    // `.bson`/`.metadata.json.gz` files, siblings under
    // `<tempdir>/<database>/`) into one blob under a stable "dump/" prefix
    // - matching mongodump's own default `--out dump` naming convention,
    // so restoring later is the unsurprising `mongorestore dump/` against
    // the untarred archive.
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar_builder = tar::Builder::new(gz);
    tar_builder
        .append_dir_all("dump", tempdir.path())
        .context("failed to tar the mongodump output directory")?;
    let gz = tar_builder
        .into_inner()
        .context("failed to finalize the mongodump tar archive")?;
    let bytes = gz.finish().context("failed to finalize the mongodump tar.gz")?;

    Ok(bytes)
}
