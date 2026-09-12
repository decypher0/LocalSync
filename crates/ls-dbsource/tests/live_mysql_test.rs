//! Real, live-server integration test: no mocks, no fakes. Spins up an
//! actual disposable `mariadbd` instance on a free local port, drives it
//! with the real `mysql` crate exactly the way a production caller would,
//! exercises `connect::list_tables`/`connect::test_connection` and
//! `export::export_tables` against it, then proves the export is *correct*
//! (not just "didn't crash") by replaying the generated dump into a second,
//! empty database on the same server and asserting the reimported content
//! matches the original exactly - including the quote/backslash string,
//! the NULL, and the exact bytes of a binary column.
//!
//! Uses the sandbox's real `mariadbd`/`mariadb-install-db` binaries
//! directly (not Podman/containers - this project has already documented
//! container-based test flakiness elsewhere, so a plain child process is
//! the more reliable choice here).
//!
//! `TestServer` stops its server via a real `SHUTDOWN` SQL command, not
//! `Child::kill()` - found the hard way, in this exact sandbox: its
//! `mariadbd` binary runs under an AppArmor profile that denies *any*
//! process (including its own direct parent) from delivering it a signal at
//! all - confirmed via `dmesg`'s audit log, `apparmor="DENIED" ...
//! profile="mariadbd" ... signal=kill/term`. `Child::kill()` fails silently
//! (its `Result` was being ignored) and the matching `Child::wait()` then
//! blocks forever on a process that can never die from a signal, hanging
//! the whole test with no output - not a bug in this crate's connect/export
//! logic, which a real, obtained-through-painful-debugging root cause. A
//! graceful `SHUTDOWN` over the real MySQL wire protocol isn't a signal at
//! all (mediated by AppArmor's "signal" rule, not its network path), so it
//! isn't blocked - the server exits on its own, same as it would for any
//! real client asking it to shut down.

use anyhow::{Context, Result};
use ls_dbsource::{connect, export, ConnectionDetails};
use mysql::prelude::Queryable;
use mysql::{Conn, OptsBuilder};
use std::io::Write;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A disposable, throwaway `mariadbd` bound to a free localhost port, with
/// `--skip-grant-tables` (simplest way to get a working, no-password root
/// connection to a server nothing else will ever touch - not a real
/// credential). Its datadir lives under a `tempfile::tempdir()` that is
/// cleaned up on drop; `Drop` also kills the child process itself so it
/// doesn't linger past the test.
struct TestServer {
    child: Child,
    _tempdir: tempfile::TempDir,
    port: u16,
}

impl TestServer {
    fn start() -> Result<Self> {
        let tempdir = tempfile::tempdir().context("create tempdir for mariadb datadir")?;
        let datadir = tempdir.path().join("data");
        std::fs::create_dir_all(&datadir).context("create datadir")?;
        let socket_path = tempdir.path().join("mysqld.sock");
        let pid_file = tempdir.path().join("mysqld.pid");
        let error_log = tempdir.path().join("error.log");

        let install = Command::new("mariadb-install-db")
            .arg("--no-defaults")
            .arg(format!("--datadir={}", datadir.display()))
            .arg("--auth-root-authentication-method=normal")
            .arg("--skip-test-db")
            .output()
            .context(
                "failed to run mariadb-install-db - is it installed and on PATH in this environment?",
            )?;
        if !install.status.success() {
            anyhow::bail!(
                "mariadb-install-db failed (status {}):\nstdout: {}\nstderr: {}",
                install.status,
                String::from_utf8_lossy(&install.stdout),
                String::from_utf8_lossy(&install.stderr)
            );
        }

        // Bind an ephemeral port to find one that's genuinely free, then
        // drop the listener before mariadbd binds it itself - avoids the
        // flakiness of a hardcoded port already being in use.
        let port = {
            let listener =
                TcpListener::bind("127.0.0.1:0").context("bind ephemeral port to find a free one")?;
            listener.local_addr().context("read ephemeral port")?.port()
        };

        let child = Command::new("mariadbd")
            .arg("--no-defaults")
            .arg(format!("--datadir={}", datadir.display()))
            .arg(format!("--socket={}", socket_path.display()))
            .arg(format!("--port={port}"))
            .arg("--bind-address=127.0.0.1")
            .arg("--skip-grant-tables")
            .arg(format!("--pid-file={}", pid_file.display()))
            .arg(format!("--log-error={}", error_log.display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to spawn mariadbd - is it installed and on PATH in this environment?")?;

        let mut server = TestServer {
            child,
            _tempdir: tempdir,
            port,
        };
        server.wait_until_ready(&error_log)?;
        Ok(server)
    }

    /// Waits for the server to actually accept a real connection and run a
    /// real query - not a fixed sleep guess, and not a `pgrep`-style
    /// liveness poll (this project's own build notes found those flaky).
    /// A `kill -0`-style PID liveness check is used only to fail fast with
    /// a useful error log if the process has already died; the actual
    /// readiness signal is "can I connect and run SELECT 1".
    fn wait_until_ready(&mut self, error_log: &Path) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut last_err = None;

        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                let log = std::fs::read_to_string(error_log).unwrap_or_default();
                anyhow::bail!(
                    "mariadbd exited early with {status} before becoming ready; error log:\n{log}"
                );
            }

            let opts = OptsBuilder::new()
                .ip_or_hostname(Some("127.0.0.1"))
                .tcp_port(self.port)
                .user(Some("root"))
                .tcp_connect_timeout(Some(Duration::from_millis(500)))
                .read_timeout(Some(Duration::from_millis(500)))
                .write_timeout(Some(Duration::from_millis(500)));
            match Conn::new(opts).and_then(|mut c| c.query_drop("SELECT 1")) {
                Ok(()) => return Ok(()),
                Err(e) => last_err = Some(e.to_string()),
            }
            std::thread::sleep(Duration::from_millis(200));
        }

        let log = std::fs::read_to_string(error_log).unwrap_or_default();
        anyhow::bail!(
            "mariadbd never became ready within 20s (last connection error: {:?}); error log:\n{log}",
            last_err
        );
    }

    fn admin_conn(&self) -> Result<Conn> {
        self.conn_to(None)
    }

    fn conn_to(&self, database: Option<&str>) -> Result<Conn> {
        let opts = OptsBuilder::new()
            .ip_or_hostname(Some("127.0.0.1"))
            .tcp_port(self.port)
            .user(Some("root"))
            .db_name(database)
            .tcp_connect_timeout(Some(Duration::from_secs(10)))
            .read_timeout(Some(Duration::from_secs(10)))
            .write_timeout(Some(Duration::from_secs(10)));
        Conn::new(opts).context("failed to connect to test server")
    }

    fn details(&self, database: &str) -> ConnectionDetails {
        ConnectionDetails {
            engine: "mysql".to_string(),
            host: "127.0.0.1".to_string(),
            port: self.port,
            database: database.to_string(),
            username: "root".to_string(),
            password: String::new(),
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // See the module doc comment: a graceful SQL SHUTDOWN, not a signal
        // - Child::kill() is silently blocked by this sandbox's AppArmor
        // mariadbd profile, and the wait() below would otherwise hang
        // forever waiting for a process nothing can signal. Best-effort: if
        // the server already died on its own (e.g. a failed test run
        // partway through startup, before it was ever ready), this simply
        // fails to connect and is ignored - wait() below reaps it either way.
        let opts = OptsBuilder::new()
            .ip_or_hostname(Some("127.0.0.1"))
            .tcp_port(self.port)
            .user(Some("root"))
            .tcp_connect_timeout(Some(Duration::from_secs(2)));
        if let Ok(mut conn) = Conn::new(opts) {
            let _ = conn.query_drop("SHUTDOWN");
        }
        let _ = self.child.wait();
    }
}

#[test]
fn live_mysql_export_round_trip() -> Result<()> {
    eprintln!("STEP: starting server");
    let server = TestServer::start()?;
    eprintln!("STEP: server ready on port {}", server.port);

    // --- Set up a real source database with a real, varied table. ---
    let mut admin = server.admin_conn()?;
    eprintln!("STEP: admin conn opened");
    admin
        .query_drop("CREATE DATABASE src_db")
        .context("create src_db")?;
    admin
        .query_drop("CREATE DATABASE dst_db")
        .context("create dst_db")?;

    let mut src_conn = server.conn_to(Some("src_db"))?;
    src_conn
        .query_drop(
            "CREATE TABLE widgets (
                id INT PRIMARY KEY,
                name VARCHAR(255) NOT NULL,
                note VARCHAR(255) NULL,
                payload VARBINARY(64) NULL
            )",
        )
        .context("create widgets table")?;

    // Real non-UTF8-safe-if-mishandled bytes: includes 0xFF/0xFE (not valid
    // UTF-8 on their own), an embedded NUL, and the ASCII bytes for a
    // single quote and a backslash - exactly the bytes that would corrupt
    // naive string-based SQL escaping, which is why binary columns must go
    // through the hex-literal path instead.
    let binary_payload: Vec<u8> = vec![0x00, 0x01, 0x02, 0xFF, 0xFE, b'\'', b'\\', 0x00, 0x41];
    let other_payload: Vec<u8> = vec![0x00, 0x00, 0x41, 0x42];

    src_conn
        .exec_drop(
            "INSERT INTO widgets (id, name, note, payload) VALUES (?, ?, ?, ?)",
            (
                1,
                "it's a \\ test",
                Some("has a note"),
                Some(binary_payload.clone()),
            ),
        )
        .context("insert row 1")?;
    src_conn
        .exec_drop(
            "INSERT INTO widgets (id, name, note, payload) VALUES (?, ?, ?, ?)",
            (
                2,
                "second row",
                Option::<String>::None,
                Option::<Vec<u8>>::None,
            ),
        )
        .context("insert row 2")?;
    src_conn
        .exec_drop(
            "INSERT INTO widgets (id, name, note, payload) VALUES (?, ?, ?, ?)",
            (3, "third row", Some("plain note"), Some(other_payload.clone())),
        )
        .context("insert row 3")?;

    // Nudge information_schema's row-count estimate to be accurate for
    // this tiny table, so the assertion below is meaningful rather than
    // trivially true.
    src_conn
        .query_drop("ANALYZE TABLE widgets")
        .context("analyze widgets")?;
    drop(src_conn);
    eprintln!("STEP: src_db set up with 3 rows");

    let src_details = server.details("src_db");

    // --- test_connection: real connect + real SELECT 1. ---
    connect::test_connection(&src_details).context("test_connection against src_db")?;
    eprintln!("STEP: test_connection ok");

    // A bad database name must fail clearly, not hang or panic.
    let mut bad_details = src_details.clone();
    bad_details.database = "does_not_exist_db".to_string();
    // Connecting with an unknown db_name fails at connect time for MySQL/
    // MariaDB, which is exactly the "clear error, not a panic" behavior
    // being exercised here.
    assert!(connect::test_connection(&bad_details).is_err());
    eprintln!("STEP: bad_details rejected");

    // Unsupported engine must be rejected clearly, with no network attempt
    // at all. Round 18: "postgres" (this test's original choice here) is
    // now a *real*, dispatched engine in its own right - testing it here
    // would exercise "what happens when you speak Postgres wire protocol
    // to a real MySQL server" (a protocol-mismatch error, still technically
    // `is_err()`, but not the "unsupported engine, rejected before any
    // connection is attempted" behavior this test means to prove) rather
    // than what it's meant to. "sqlite" is genuinely unimplemented by any
    // dispatcher arm, so it actually exercises that rejection path.
    let mut wrong_engine = src_details.clone();
    wrong_engine.engine = "sqlite".to_string();
    let err = connect::test_connection(&wrong_engine).unwrap_err();
    assert!(format!("{err:#}").contains("unsupported database engine"));
    assert!(connect::list_tables(&wrong_engine).is_err());
    assert!(export::export_tables(&wrong_engine, &["widgets".to_string()]).is_err());
    eprintln!("STEP: wrong_engine rejected");

    // Round 18's actual reproduced bug and its fix: a literal "localhost"
    // must connect exactly as well as the unambiguous loopback address -
    // this sandbox's own `getent hosts localhost` resolves to the IPv6
    // loopback only, which is exactly the real, confirmed root cause (see
    // engines::mysql::open_connection's doc comment). Before the fix, this
    // assertion failed with "Connection refused (os error 111)".
    let mut localhost_details = src_details.clone();
    localhost_details.host = "localhost".to_string();
    connect::test_connection(&localhost_details)
        .context("connecting via literal 'localhost' must work exactly like 127.0.0.1")?;
    eprintln!("STEP: localhost connects (round 18 fix)");

    // Round 18's other fix: the real underlying reason must survive in the
    // error chain, not just an outer "failed to connect" wrapper - this is
    // what commands.rs's `{:#}` (not `.to_string()`) actually depends on
    // being true here.
    let chain = format!("{:#}", connect::test_connection(&bad_details).unwrap_err());
    assert!(
        chain.contains("Unknown database") || chain.contains("does_not_exist_db"),
        "expected the real MySQL error to survive in the chain, got: {chain}"
    );
    eprintln!("STEP: real error reason survives in the chain (round 18 fix)");

    // --- list_tables: real information_schema query. ---
    let tables = connect::list_tables(&src_details).context("list_tables against src_db")?;
    eprintln!("STEP: list_tables ok, {} tables", tables.len());

    // --- Round 22 goal 3: real database listing, connecting without
    // pinning to src_details.database (proving the connection genuinely
    // doesn't require it to already be correct). ---
    let mut no_db_pin = src_details.clone();
    no_db_pin.database = "does_not_exist_at_all".to_string();
    let databases = connect::list_databases(&no_db_pin).context("list_databases despite a nonexistent 'database' field")?;
    assert!(databases.contains(&"src_db".to_string()), "expected src_db among {databases:?}");
    assert!(databases.contains(&"dst_db".to_string()), "expected dst_db among {databases:?}");
    assert!(
        !databases.iter().any(|d| d == "information_schema" || d == "mysql" || d == "performance_schema" || d == "sys"),
        "internal MySQL schemas must be filtered out: {databases:?}"
    );
    eprintln!("STEP: list_databases ok, {} databases (round 22)", databases.len());
    let widgets = tables
        .iter()
        .find(|t| t.name == "widgets")
        .expect("widgets table should be listed");
    assert!(
        widgets.approx_row_count.unwrap_or(0) >= 1,
        "expected a sane (>=1) approx row count for a 3-row table, got {:?}",
        widgets.approx_row_count
    );

    // --- export_tables: real SHOW CREATE TABLE + real streamed SELECT *. ---
    let dump = export::export_tables(&src_details, &["widgets".to_string()])
        .context("export_tables for widgets")?;
    eprintln!("STEP: export_tables ok, {} bytes", dump.len());
    let dump_text = String::from_utf8(dump.clone()).expect("dump must be valid UTF-8 SQL text");

    assert!(dump_text.contains("-- Table: widgets"));
    assert!(dump_text.contains("DROP TABLE IF EXISTS `widgets`;"));
    assert!(dump_text.to_uppercase().contains("CREATE TABLE"));
    assert!(dump_text.contains("INSERT INTO `widgets`"));
    // Proof the hex-literal path for binary columns was actually taken.
    assert!(
        dump_text.contains("X'"),
        "expected at least one X'...' hex literal for the VARBINARY column:\n{dump_text}"
    );
    // Proof the quote/backslash string was escaped, not left to break the
    // statement.
    assert!(dump_text.contains("it\\'s a \\\\ test"));

    // Rejecting a table name containing a backtick.
    let evil = vec!["widgets`; DROP TABLE widgets; --".to_string()];
    assert!(export::export_tables(&src_details, &evil).is_err());

    // A table that doesn't exist must bubble up a clear error, not be
    // silently skipped.
    let missing = vec!["does_not_exist_table".to_string()];
    assert!(export::export_tables(&src_details, &missing).is_err());
    eprintln!("STEP: negative export cases ok");

    // --- The strongest proof: replay the dump into a second, empty
    // database and assert the reimported content matches exactly. ---
    let mut mysql_cli = Command::new("mysql")
        .arg("-h127.0.0.1")
        .arg("-P")
        .arg(server.port.to_string())
        .arg("-uroot")
        .arg("dst_db")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn the mysql CLI to replay the dump")?;
    eprintln!("STEP: mysql CLI spawned");
    mysql_cli
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(&dump)
        .context("write dump to mysql CLI stdin")?;
    eprintln!("STEP: dump written to mysql CLI stdin");
    let replay_output = mysql_cli
        .wait_with_output()
        .context("wait for mysql CLI replay to finish")?;
    eprintln!("STEP: mysql CLI replay finished");
    assert!(
        replay_output.status.success(),
        "replaying the dump into dst_db failed (status {}):\nstdout: {}\nstderr: {}",
        replay_output.status,
        String::from_utf8_lossy(&replay_output.stdout),
        String::from_utf8_lossy(&replay_output.stderr)
    );
    eprintln!("STEP: replay status ok");

    eprintln!("STEP: about to open dst_conn");
    let mut dst_conn = server.conn_to(Some("dst_db"))?;
    eprintln!("STEP: dst_conn opened");
    type ReimportedRow = (i64, String, Option<String>, Option<Vec<u8>>);
    let rows: Vec<ReimportedRow> = dst_conn
        .query("SELECT id, name, note, payload FROM widgets ORDER BY id")
        .context("query reimported widgets")?;
    eprintln!("STEP: dst_conn queried, {} rows", rows.len());

    assert_eq!(rows.len(), 3, "expected exactly 3 reimported rows: {rows:?}");

    assert_eq!(rows[0].0, 1);
    assert_eq!(rows[0].1, "it's a \\ test");
    assert_eq!(rows[0].2.as_deref(), Some("has a note"));
    assert_eq!(
        rows[0].3.as_deref(),
        Some(binary_payload.as_slice()),
        "binary column must round-trip byte-for-byte"
    );

    assert_eq!(rows[1].0, 2);
    assert_eq!(rows[1].1, "second row");
    assert_eq!(rows[1].2, None, "note must round-trip as a real NULL");
    assert_eq!(rows[1].3, None, "payload must round-trip as a real NULL");

    assert_eq!(rows[2].0, 3);
    assert_eq!(rows[2].1, "third row");
    assert_eq!(rows[2].2.as_deref(), Some("plain note"));
    assert_eq!(rows[2].3.as_deref(), Some(other_payload.as_slice()));

    Ok(())
}
