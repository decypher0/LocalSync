//! Real, live-server integration test: no mocks, no fakes. Spins up an
//! actual disposable `mongod` instance on a free local port, drives it
//! with the real mongodb driver exactly the way a production caller would,
//! exercises `connect::test_connection`/`connect::list_tables` and
//! `export::export_tables` against it, then proves the export is *correct*
//! (not just "didn't crash") by running the real `mongorestore` binary
//! against a second, empty database on the same server and asserting the
//! restored documents match the originals exactly - same bar
//! `live_mysql_test.rs` holds its own MySQL/MariaDB round trip to.
//!
//! Uses this sandbox's own pre-downloaded, confirmed-working `mongod`/
//! `mongodump`/`mongorestore` binaries directly (not on `$PATH` - absolute
//! paths below), the same graceful-skip-if-tooling-missing pattern
//! `apps/desktop/src-tauri/tests/pipeline_test.rs` already uses for
//! Podman, so this test doesn't hard-fail on a machine/CI runner that
//! hasn't independently downloaded these same binaries, while still
//! running for real here and now.
//!
//! `TestServer` stops its server via the real MongoDB `shutdown` admin
//! command, not `Child::kill()`/a signal - see `live_mysql_test.rs`'s own
//! `TestServer::drop()` doc comment for the exact same discovery in this
//! sandbox: its AppArmor confinement can silently block a plain
//! signal-based shutdown of a database server child process (confirmed
//! there via `dmesg`'s audit log for `mariadbd`; the same class of
//! sandbox restriction applies here). A graceful `{shutdown: 1}` command
//! over MongoDB's own wire protocol isn't a signal at all, so it isn't
//! blocked - the server exits on its own and the connection drops as it
//! does, which is treated as an expected error rather than a failure.

use anyhow::{Context, Result};
use ls_dbsource::{connect, export, ConnectionDetails};
use mongodb::bson::{doc, Document};
use mongodb::options::{ClientOptions, ServerAddress};
use mongodb::sync::Client;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MONGOD_BIN: &str = "/tmp/mongo-setup/mongodb-linux-x86_64-debian12-7.0.14/bin/mongod";
const MONGODUMP_BIN: &str = "/tmp/mongo-setup/tools_extracted/usr/bin/mongodump";
const MONGORESTORE_BIN: &str = "/tmp/mongo-setup/tools_extracted/usr/bin/mongorestore";

/// This test's own `mongodump`/`mongorestore` invocations (via
/// `export::export_tables` and this file's own restore step) shell out to
/// the bare `mongodump`/`mongorestore` command names, which aren't on
/// `$PATH` in this sandbox - so a `PATH` entry pointing at their real
/// directory is prepended to this process's own environment for the
/// duration of the test.
fn tooling_available() -> bool {
    Path::new(MONGOD_BIN).is_file()
        && Path::new(MONGODUMP_BIN).is_file()
        && Path::new(MONGORESTORE_BIN).is_file()
}

fn prepend_tooling_to_path() {
    let tools_dir = Path::new(MONGODUMP_BIN).parent().unwrap();
    let mongod_dir = Path::new(MONGOD_BIN).parent().unwrap();
    let existing = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}:{existing}", tools_dir.display(), mongod_dir.display());
    std::env::set_var("PATH", new_path);
}

/// A disposable, throwaway `mongod` bound to a free localhost port with no
/// auth (this is a real, one-off, nothing-else-will-ever-touch-it test
/// server - not a real credential store). Its dbpath lives under a
/// `tempfile::tempdir()` cleaned up on drop; `Drop` also reaps the child
/// process after asking it to shut down gracefully (see the module doc
/// comment for why not `Child::kill()`).
struct TestServer {
    child: Child,
    _tempdir: tempfile::TempDir,
    port: u16,
}

impl TestServer {
    fn start() -> Result<Self> {
        let tempdir = tempfile::tempdir().context("create tempdir for mongod dbpath")?;
        let dbpath = tempdir.path().join("data");
        // mongod does not create a nested dbpath directory on its own.
        std::fs::create_dir_all(&dbpath).context("create dbpath")?;
        let log_path = tempdir.path().join("mongod.log");

        let port = {
            let listener =
                TcpListener::bind("127.0.0.1:0").context("bind ephemeral port to find a free one")?;
            listener.local_addr().context("read ephemeral port")?.port()
        };

        let child = Command::new(MONGOD_BIN)
            .arg("--dbpath")
            .arg(&dbpath)
            .arg("--port")
            .arg(port.to_string())
            .arg("--bind_ip")
            .arg("127.0.0.1")
            .arg("--logpath")
            .arg(&log_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to spawn mongod at {MONGOD_BIN}"))?;

        let mut server = TestServer {
            child,
            _tempdir: tempdir,
            port,
        };
        server.wait_until_ready(&log_path)?;
        Ok(server)
    }

    /// Waits for the server to actually accept a real connection and run a
    /// real command - not a fixed sleep guess.
    fn wait_until_ready(&mut self, log_path: &Path) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut last_err = None;

        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                let log = std::fs::read_to_string(log_path).unwrap_or_default();
                anyhow::bail!(
                    "mongod exited early with {status} before becoming ready; log:\n{log}"
                );
            }

            let details = self.details("admin");
            match connect::test_connection(&details) {
                Ok(()) => return Ok(()),
                Err(e) => last_err = Some(format!("{e:#}")),
            }
            std::thread::sleep(Duration::from_millis(200));
        }

        let log = std::fs::read_to_string(log_path).unwrap_or_default();
        anyhow::bail!(
            "mongod never became ready within 20s (last connection error: {:?}); log:\n{log}",
            last_err
        );
    }

    fn client(&self) -> Result<Client> {
        let options = ClientOptions::builder()
            .hosts(vec![ServerAddress::Tcp {
                host: "127.0.0.1".to_string(),
                port: Some(self.port),
            }])
            .connect_timeout(Duration::from_secs(10))
            .server_selection_timeout(Duration::from_secs(10))
            .direct_connection(true)
            .build();
        Client::with_options(options).context("failed to build client for test server")
    }

    fn details(&self, database: &str) -> ConnectionDetails {
        ConnectionDetails {
            engine: "mongodb".to_string(),
            host: "127.0.0.1".to_string(),
            port: self.port,
            database: database.to_string(),
            username: String::new(),
            password: String::new(),
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // See the module doc comment: a graceful `{shutdown: 1}` admin
        // command, not a signal - this sandbox's AppArmor confinement can
        // silently block Child::kill() against a database server child,
        // the same discovery live_mysql_test.rs's TestServer::drop()
        // documents for mariadbd. Best-effort: if the server already died
        // on its own (e.g. a failed test run partway through startup), the
        // client build/shutdown command simply fails to connect and is
        // ignored - wait() below reaps the process either way. The server
        // closing the connection as part of a *successful* shutdown is
        // itself reported as an error by the driver, which is expected and
        // also ignored.
        if let Ok(client) = self.client() {
            let _ = client
                .database("admin")
                .run_command(doc! { "shutdown": 1 })
                .run();
        }
        let _ = self.child.wait();
    }
}

#[test]
fn live_mongo_export_round_trip() -> Result<()> {
    if !tooling_available() {
        eprintln!(
            "real mongod/mongodump/mongorestore binaries not found under /tmp/mongo-setup - skipping live_mongo_export_round_trip"
        );
        return Ok(());
    }
    prepend_tooling_to_path();

    eprintln!("STEP: starting server");
    let server = TestServer::start()?;
    eprintln!("STEP: server ready on port {}", server.port);

    // --- Set up a real source database with two real collections, a mix
    // of realistic BSON types across them. ---
    let client = server.client()?;
    let src_db = client.database("src_db");

    let widgets = src_db.collection::<Document>("widgets");
    widgets
        .insert_many(vec![
            doc! {
                "_id": 1,
                "name": "left widget",
                "weight_kg": 1.5,
                "tags": ["a", "b"],
                "meta": { "color": "red", "in_stock": true },
            },
            doc! {
                "_id": 2,
                "name": "right widget",
                "weight_kg": 2.25,
                "tags": ["c"],
                "meta": { "color": "blue", "in_stock": false },
            },
        ])
        .run()
        .context("insert into widgets")?;

    let gadgets = src_db.collection::<Document>("gadgets");
    gadgets
        .insert_many(vec![
            doc! { "_id": 1, "label": "gadget one", "count": 10i64 },
            doc! { "_id": 2, "label": "gadget two", "count": 20i64 },
            doc! { "_id": 3, "label": "gadget three", "count": 30i64 },
        ])
        .run()
        .context("insert into gadgets")?;
    eprintln!("STEP: src_db set up with 2 widgets + 3 gadgets");

    let src_details = server.details("src_db");

    // --- test_connection: real connect + real ping. ---
    connect::test_connection(&src_details).context("test_connection against src_db")?;
    eprintln!("STEP: test_connection ok");

    // Unsupported engine must be rejected clearly, with no network attempt
    // at all. Round 18: "mysql" (this test's original choice here) is now
    // a *real*, dispatched engine in its own right - testing it here would
    // exercise "what happens when you speak MySQL wire protocol to a real
    // mongod" (a protocol-mismatch error, not the "unsupported engine,
    // rejected before any connection is attempted" behavior this test
    // means to prove). "sqlite" is genuinely unimplemented by any
    // dispatcher arm, so it actually exercises that rejection path.
    let mut wrong_engine = src_details.clone();
    wrong_engine.engine = "sqlite".to_string();
    let err = connect::test_connection(&wrong_engine).unwrap_err();
    assert!(format!("{err:#}").contains("unsupported database engine"));
    assert!(connect::list_tables(&wrong_engine).is_err());
    assert!(export::export_tables(&wrong_engine, &["widgets".to_string()]).is_err());
    eprintln!("STEP: wrong_engine rejected");

    // Round 18's localhost fix, proven for this engine too (see
    // engines::mysql::open_connection's doc comment for the real,
    // reproduced root cause - this sandbox's "localhost" resolves to the
    // IPv6 loopback only, which a server bound to 127.0.0.1 refuses).
    let mut localhost_details = src_details.clone();
    localhost_details.host = "localhost".to_string();
    connect::test_connection(&localhost_details)
        .context("connecting via literal 'localhost' must work exactly like 127.0.0.1")?;
    eprintln!("STEP: localhost connects (round 18 fix)");

    // --- list_tables: real list_collection_names + real
    // estimated_document_count per collection. ---
    let tables = connect::list_tables(&src_details).context("list_tables against src_db")?;
    eprintln!("STEP: list_tables ok, {} collections", tables.len());
    assert_eq!(tables.len(), 2, "expected exactly widgets+gadgets: {tables:?}");

    let widgets_info = tables
        .iter()
        .find(|t| t.name == "widgets")
        .expect("widgets collection should be listed");
    assert_eq!(
        widgets_info.approx_row_count,
        Some(2),
        "expected an approx count of 2 for widgets"
    );

    let gadgets_info = tables
        .iter()
        .find(|t| t.name == "gadgets")
        .expect("gadgets collection should be listed");
    assert_eq!(
        gadgets_info.approx_row_count,
        Some(3),
        "expected an approx count of 3 for gadgets"
    );

    // Rejecting an unsafe-looking collection name.
    let evil = vec!["--eval".to_string()];
    assert!(export::export_tables(&src_details, &evil).is_err());
    eprintln!("STEP: negative export case ok");

    // --- export_tables: real mongodump, once per collection. ---
    let dump = export::export_tables(
        &src_details,
        &["widgets".to_string(), "gadgets".to_string()],
    )
    .context("export_tables for widgets+gadgets")?;
    eprintln!("STEP: export_tables ok, {} bytes", dump.len());
    assert!(!dump.is_empty(), "dump must not be empty");

    // --- The strongest proof: untar the returned bytes, run the real
    // mongorestore binary against a second, empty database on the same
    // server, then assert the restored documents match the originals
    // exactly. ---
    let extract_dir = tempfile::tempdir().context("create tempdir to extract the dump into")?;
    {
        let decoder = flate2::read::GzDecoder::new(dump.as_slice());
        let mut archive = tar::Archive::new(decoder);
        archive
            .unpack(extract_dir.path())
            .context("failed to untar the mongodump archive")?;
    }
    // Per the module/export doc comments: the archive was built as
    // "dump/<database>/*.bson" - directory-mode output, restored with a
    // plain `mongorestore <dir>`, never `--archive`.
    let dump_dir: PathBuf = extract_dir.path().join("dump");
    assert!(
        dump_dir.join("src_db").join("widgets.bson").is_file(),
        "expected widgets.bson under the extracted dump directory"
    );
    assert!(
        dump_dir.join("src_db").join("gadgets.bson").is_file(),
        "expected gadgets.bson under the extracted dump directory"
    );
    eprintln!("STEP: dump extracted and looks right on disk");

    let restore_output = Command::new(MONGORESTORE_BIN)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(server.port.to_string())
        .arg("--nsFrom")
        .arg("src_db.*")
        .arg("--nsTo")
        .arg("dst_db.*")
        .arg(&dump_dir)
        .stdin(Stdio::null())
        .output()
        .context("failed to run mongorestore")?;
    assert!(
        restore_output.status.success(),
        "mongorestore failed (status {}):\nstdout: {}\nstderr: {}",
        restore_output.status,
        String::from_utf8_lossy(&restore_output.stdout),
        String::from_utf8_lossy(&restore_output.stderr)
    );
    eprintln!("STEP: mongorestore into dst_db succeeded");

    let dst_db = client.database("dst_db");

    let mut restored_widgets: Vec<Document> = dst_db
        .collection::<Document>("widgets")
        .find(doc! {})
        .sort(doc! { "_id": 1 })
        .run()
        .context("query restored widgets cursor")?
        .collect::<std::result::Result<Vec<Document>, mongodb::error::Error>>()
        .context("collect restored widgets")?;
    restored_widgets.sort_by_key(|d| d.get_i32("_id").unwrap_or(0));

    assert_eq!(restored_widgets.len(), 2, "expected 2 restored widgets: {restored_widgets:?}");
    assert_eq!(restored_widgets[0].get_i32("_id").unwrap(), 1);
    assert_eq!(restored_widgets[0].get_str("name").unwrap(), "left widget");
    assert_eq!(restored_widgets[0].get_f64("weight_kg").unwrap(), 1.5);
    let tags0 = restored_widgets[0].get_array("tags").unwrap();
    assert_eq!(
        tags0.iter().map(|b| b.as_str().unwrap()).collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    let meta0 = restored_widgets[0].get_document("meta").unwrap();
    assert_eq!(meta0.get_str("color").unwrap(), "red");
    assert_eq!(meta0.get_bool("in_stock").unwrap(), true);

    assert_eq!(restored_widgets[1].get_i32("_id").unwrap(), 2);
    assert_eq!(restored_widgets[1].get_str("name").unwrap(), "right widget");
    assert_eq!(restored_widgets[1].get_f64("weight_kg").unwrap(), 2.25);
    let meta1 = restored_widgets[1].get_document("meta").unwrap();
    assert_eq!(meta1.get_str("color").unwrap(), "blue");
    assert_eq!(meta1.get_bool("in_stock").unwrap(), false);

    let mut restored_gadgets: Vec<Document> = dst_db
        .collection::<Document>("gadgets")
        .find(doc! {})
        .sort(doc! { "_id": 1 })
        .run()
        .context("query restored gadgets cursor")?
        .collect::<std::result::Result<Vec<Document>, mongodb::error::Error>>()
        .context("collect restored gadgets")?;
    restored_gadgets.sort_by_key(|d| d.get_i32("_id").unwrap_or(0));

    assert_eq!(restored_gadgets.len(), 3, "expected 3 restored gadgets: {restored_gadgets:?}");
    for (i, expected_label, expected_count) in [
        (0usize, "gadget one", 10i64),
        (1, "gadget two", 20i64),
        (2, "gadget three", 30i64),
    ] {
        assert_eq!(restored_gadgets[i].get_i32("_id").unwrap(), (i + 1) as i32);
        assert_eq!(restored_gadgets[i].get_str("label").unwrap(), expected_label);
        assert_eq!(restored_gadgets[i].get_i64("count").unwrap(), expected_count);
    }

    eprintln!("STEP: restored documents match the originals exactly");

    Ok(())
}
