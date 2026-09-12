//! Real, live-server integration test for `engines::postgres` - no mocks,
//! no fakes. Spins up an actual disposable `postgres` server on a free
//! local port with a real password (`scram-sha-256` auth, Postgres 18's
//! own default - not `trust`, so a wrong-password test below is actually
//! meaningful), drives it with the real `postgres` crate and the real
//! `pg_dump` binary exactly the way a production caller would, then proves
//! the export is *correct* (not just "didn't crash") by replaying the
//! generated dump into a second, empty database on the same server and
//! asserting the reimported content matches exactly - including a quoted/
//! escaped string, a real NULL, and a JSONB value.
//!
//! Uses this sandbox's real `initdb`/`postgres` binaries directly (from the
//! `postgresql-18` package already installed here), not Podman/containers -
//! same reasoning `live_mysql_test.rs` documents for MariaDB. Unlike that
//! server, a direct `postgres` child process was confirmed *not* to be
//! blocked by this sandbox's AppArmor confinement (verified directly: a
//! real `pg_ctl stop` cleanly stopped a real running instance here) - so
//! `Child::kill()` in `Drop` is fine, no SQL-level shutdown workaround
//! needed the way MariaDB's `TestServer` requires.

use anyhow::{Context, Result};
use ls_dbsource::{connect, export, ConnectionDetails};
use postgres::{Client, NoTls};
use std::io::Write;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PG_BIN: &str = "/usr/lib/postgresql/18/bin";

struct TestServer {
    child: Child,
    _tempdir: tempfile::TempDir,
    port: u16,
    password: String,
}

impl TestServer {
    fn start() -> Result<Self> {
        let tempdir = tempfile::tempdir().context("create tempdir for postgres data dir")?;
        let datadir = tempdir.path().join("data");
        let socket_dir = tempdir.path().join("sockets");
        std::fs::create_dir_all(&socket_dir).context("create socket dir")?;

        let password = "Test-round18-pw-1234!".to_string();
        let pwfile = tempdir.path().join("pwfile");
        std::fs::write(&pwfile, &password).context("write pwfile")?;

        let initdb = format!("{PG_BIN}/initdb");
        let install = Command::new(&initdb)
            .arg("-D")
            .arg(&datadir)
            .arg("-U")
            .arg("postgres")
            .arg("--pwfile")
            .arg(&pwfile)
            .arg("--auth=scram-sha-256")
            .output()
            .with_context(|| format!("failed to run {initdb} - is postgresql-18 installed in this environment?"))?;
        if !install.status.success() {
            anyhow::bail!(
                "initdb failed (status {}):\nstdout: {}\nstderr: {}",
                install.status,
                String::from_utf8_lossy(&install.stdout),
                String::from_utf8_lossy(&install.stderr)
            );
        }

        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").context("bind ephemeral port to find a free one")?;
            listener.local_addr().context("read ephemeral port")?.port()
        };

        let error_log = tempdir.path().join("error.log");
        let child = Command::new(format!("{PG_BIN}/postgres"))
            .arg("-D")
            .arg(&datadir)
            .arg("-p")
            .arg(port.to_string())
            .arg("-h")
            .arg("127.0.0.1")
            .arg("-k")
            .arg(&socket_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&error_log)?))
            .spawn()
            .context("failed to spawn postgres - is it installed and on PATH in this environment?")?;

        let mut server = TestServer { child, _tempdir: tempdir, port, password };
        server.wait_until_ready(&error_log)?;
        Ok(server)
    }

    /// Waits for the server to actually accept a real connection and run a
    /// real query - not a fixed sleep guess, and not a `pgrep`-style
    /// liveness poll (this project's own build notes found those flaky).
    fn wait_until_ready(&mut self, error_log: &Path) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut last_err = None;

        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                let log = std::fs::read_to_string(error_log).unwrap_or_default();
                anyhow::bail!("postgres exited early with {status} before becoming ready; error log:\n{log}");
            }

            let mut config = postgres::Config::new();
            config
                .host("127.0.0.1")
                .port(self.port)
                .user("postgres")
                .password(&self.password)
                .dbname("postgres")
                .connect_timeout(Duration::from_millis(500));
            match config.connect(NoTls) {
                Ok(_) => return Ok(()),
                Err(e) => last_err = Some(e.to_string()),
            }
            std::thread::sleep(Duration::from_millis(200));
        }

        let log = std::fs::read_to_string(error_log).unwrap_or_default();
        anyhow::bail!("postgres never became ready within 20s (last connection error: {last_err:?}); error log:\n{log}");
    }

    fn admin_conn(&self) -> Result<Client> {
        let mut config = postgres::Config::new();
        config
            .host("127.0.0.1")
            .port(self.port)
            .user("postgres")
            .password(&self.password)
            .dbname("postgres")
            .connect_timeout(Duration::from_secs(10));
        config.connect(NoTls).context("failed to connect to test server (admin)")
    }

    fn details(&self, database: &str) -> ConnectionDetails {
        ConnectionDetails {
            engine: "postgres".to_string(),
            host: "127.0.0.1".to_string(),
            port: self.port,
            database: database.to_string(),
            username: "postgres".to_string(),
            password: self.password.clone(),
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // See the module doc comment: a plain kill() is fine here - this
        // sandbox's AppArmor confinement (found for MariaDB, see
        // live_mysql_test.rs) does not extend to a direct `postgres` child
        // process, confirmed directly.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn live_postgres_export_round_trip() -> Result<()> {
    let server = TestServer::start()?;
    eprintln!("STEP: starting server");
    eprintln!("STEP: server ready on port {}", server.port);

    let mut admin = server.admin_conn()?;
    admin.execute("CREATE DATABASE src_db", &[]).context("create src_db")?;
    admin.execute("CREATE DATABASE dst_db", &[]).context("create dst_db")?;
    eprintln!("STEP: admin conn opened");

    let mut src_config = postgres::Config::new();
    src_config
        .host("127.0.0.1")
        .port(server.port)
        .user("postgres")
        .password(&server.password)
        .dbname("src_db")
        .connect_timeout(Duration::from_secs(10));
    let mut src_conn = src_config.connect(NoTls).context("connect to src_db")?;

    src_conn
        .execute(
            "CREATE TABLE widgets (
                id INT PRIMARY KEY,
                name TEXT NOT NULL,
                note TEXT,
                data JSONB
            )",
            &[],
        )
        .context("create widgets table")?;

    src_conn
        .execute(
            "INSERT INTO widgets (id, name, note, data) VALUES ($1, $2, $3, $4)",
            &[&1i32, &"it's a \\ test", &Some("has a note"), &Some(serde_json::json!({"a": 1}))],
        )
        .context("insert row 1")?;
    src_conn
        .execute(
            "INSERT INTO widgets (id, name, note, data) VALUES ($1, $2, $3, $4)",
            &[&2i32, &"second row", &Option::<String>::None, &Option::<serde_json::Value>::None],
        )
        .context("insert row 2")?;
    src_conn.execute("ANALYZE widgets", &[]).context("analyze widgets")?;
    drop(src_conn);
    eprintln!("STEP: src_db set up with 2 rows");

    let src_details = server.details("src_db");

    // --- test_connection: real connect + real SELECT 1. ---
    connect::test_connection(&src_details).context("test_connection against src_db")?;
    eprintln!("STEP: test_connection ok");

    // A wrong password must fail clearly - and, since this server uses
    // real scram-sha-256 auth (not trust), this is a meaningful check, not
    // a no-op.
    let mut bad_password = src_details.clone();
    bad_password.password = "definitely wrong".to_string();
    let err = connect::test_connection(&bad_password).unwrap_err();
    let chain = format!("{err:#}");
    assert!(
        chain.to_lowercase().contains("password"),
        "expected the real Postgres auth failure to survive in the chain, got: {chain}"
    );
    eprintln!("STEP: wrong password rejected with the real reason surfaced");

    // A bad database name must fail clearly too.
    let mut bad_db = src_details.clone();
    bad_db.database = "does_not_exist_db".to_string();
    assert!(connect::test_connection(&bad_db).is_err());
    eprintln!("STEP: bad_details rejected");

    // Round 18's localhost fix, proven for this engine (see
    // engines::mysql::open_connection's doc comment for the real,
    // reproduced root cause).
    let mut localhost_details = src_details.clone();
    localhost_details.host = "localhost".to_string();
    connect::test_connection(&localhost_details)
        .context("connecting via literal 'localhost' must work exactly like 127.0.0.1")?;
    eprintln!("STEP: localhost connects (round 18 fix)");

    // Unsupported engine must be rejected clearly, before any network
    // attempt - "mysql" is now a *real* dispatched engine, so "sqlite"
    // (genuinely unimplemented) is what actually exercises that path.
    let mut wrong_engine = src_details.clone();
    wrong_engine.engine = "sqlite".to_string();
    let err = connect::test_connection(&wrong_engine).unwrap_err();
    assert!(format!("{err:#}").contains("unsupported database engine"));
    assert!(connect::list_tables(&wrong_engine).is_err());
    assert!(export::export_tables(&wrong_engine, &["widgets".to_string()]).is_err());
    eprintln!("STEP: wrong_engine rejected");

    // --- list_tables: real pg_catalog query. ---
    let tables = connect::list_tables(&src_details).context("list_tables against src_db")?;
    let widgets = tables.iter().find(|t| t.name == "widgets").expect("widgets table should be listed");
    assert!(
        widgets.approx_row_count.unwrap_or(0) >= 1,
        "expected a sane (>=1) approx row count for a 2-row table, got {:?}",
        widgets.approx_row_count
    );
    eprintln!("STEP: list_tables ok, {} tables", tables.len());

    // --- export_tables: real pg_dump. ---
    let dump = export::export_tables(&src_details, &["widgets".to_string()]).context("export_tables for widgets")?;
    let dump_text = String::from_utf8(dump.clone()).expect("dump must be valid UTF-8 SQL text");
    assert!(dump_text.to_uppercase().contains("CREATE TABLE"));
    assert!(dump_text.contains("widgets"));
    eprintln!("STEP: export_tables ok, {} bytes", dump.len());

    // A table name with a double quote must be rejected before ever
    // shelling out.
    let evil = vec!["widgets\"; DROP TABLE widgets; --".to_string()];
    assert!(export::export_tables(&src_details, &evil).is_err());
    eprintln!("STEP: negative export case ok");

    // --- The strongest proof: replay the dump into a second, empty
    // database and assert the reimported content matches exactly. ---
    let mut psql = Command::new("psql")
        .arg("-h")
        .arg("127.0.0.1")
        .arg("-p")
        .arg(server.port.to_string())
        .arg("-U")
        .arg("postgres")
        .arg("dst_db")
        .env("PGPASSWORD", &server.password)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn psql to replay the dump")?;
    psql.stdin.take().expect("piped stdin").write_all(&dump).context("write dump to psql stdin")?;
    let replay_output = psql.wait_with_output().context("wait for psql replay to finish")?;
    assert!(
        replay_output.status.success(),
        "replaying the dump into dst_db failed (status {}):\nstdout: {}\nstderr: {}",
        replay_output.status,
        String::from_utf8_lossy(&replay_output.stdout),
        String::from_utf8_lossy(&replay_output.stderr)
    );
    eprintln!("STEP: psql replay finished");

    let mut dst_config = postgres::Config::new();
    dst_config
        .host("127.0.0.1")
        .port(server.port)
        .user("postgres")
        .password(&server.password)
        .dbname("dst_db")
        .connect_timeout(Duration::from_secs(10));
    let mut dst_conn = dst_config.connect(NoTls).context("connect to dst_db")?;
    let rows = dst_conn
        .query("SELECT id, name, note, data FROM widgets ORDER BY id", &[])
        .context("query reimported widgets")?;
    assert_eq!(rows.len(), 2, "expected exactly 2 reimported rows");

    let id0: i32 = rows[0].get(0);
    let name0: String = rows[0].get(1);
    let note0: Option<String> = rows[0].get(2);
    let data0: Option<serde_json::Value> = rows[0].get(3);
    assert_eq!(id0, 1);
    assert_eq!(name0, "it's a \\ test");
    assert_eq!(note0.as_deref(), Some("has a note"));
    assert_eq!(data0, Some(serde_json::json!({"a": 1})));

    let id1: i32 = rows[1].get(0);
    let note1: Option<String> = rows[1].get(2);
    let data1: Option<serde_json::Value> = rows[1].get(3);
    assert_eq!(id1, 2);
    assert_eq!(note1, None, "note must round-trip as a real NULL");
    assert_eq!(data1, None, "data must round-trip as a real NULL");
    eprintln!("STEP: restored documents match the originals exactly");

    Ok(())
}
