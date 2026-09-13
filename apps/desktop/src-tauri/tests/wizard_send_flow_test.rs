//! Round 17: proves the guided database-source wizard's full path through
//! the real Tauri command layer — not just the lower-level crate functions
//! `ls-snapshot`'s and `ls-dbsource`'s own tests already exercise directly.
//!
//! Two real, separate proofs, matching the round's suggested subagent
//! breakdown's own "prove the full wizard flow" item:
//!
//! - `share_snapshot_wizard_bundles_multiple_folders_without_a_database`:
//!   two independent project folders, no database at all, sent and
//!   received together as one snapshot — proves multi-folder packaging and
//!   the diff-review fix (`ls_security::diff_summary`'s round-17 multi-
//!   folder merge) via the real command layer.
//! - `full_wizard_flow_against_a_real_local_mysql_instance`: a Spring-Boot-
//!   shaped folder whose own `application.properties` points at a real,
//!   disposable local MariaDB instance (no existing dump — the exact
//!   scenario this round's own definition of done asks for) — drives
//!   `detect_db_connection` -> `test_db_connection` -> `list_db_tables` ->
//!   `export_db_tables` -> `share_snapshot_wizard`, then receives it and
//!   confirms the real exported dump's bytes and hash made it into the
//!   manifest and payload intact.
//!
//! - `a_single_folder_wizard_send_can_actually_be_run` (round 30): a real
//!   bug found in real cross-machine testing - a project sent through this
//!   exact wizard path failed on Run with "snapshot payload has no
//!   docker-compose.yml" even though it genuinely had one, because
//!   `ls_snapshot::create_snapshot_multi` nests every folder's files under
//!   its own label unconditionally (even for exactly one folder), while
//!   `ls_containers::run_snapshot` only ever looked at the payload's own
//!   top level. Neither `pipeline_test.rs` nor `pipeline_node_test.rs` (the
//!   only tests that call `run_snapshot` at all) go through this wizard
//!   path - they use the older, single-folder `create_snapshot`, which
//!   never nests anything - so nothing ever caught this. This test drives
//!   the exact real path a click-through does: share_snapshot_wizard ->
//!   receive_snapshot -> run_snapshot -> stop_session, with one folder and
//!   a real, minimal docker-compose.yml.
//!
//! `ls-dbsource`'s own `live_mysql_test.rs` already proves the export
//! mechanics are byte-for-byte correct (round-trip reimport) — this file's
//! job is proving the *wizard's command layer* wires that correctly into a
//! real snapshot, not re-proving export correctness itself.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use localsync_desktop::{commands, state::AppState};
use tauri::Manager;

fn sh(dir: &Path, cmd: &str, args: &[&str]) {
    let status = Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {cmd} {args:?}: {e}"));
    assert!(status.success(), "{cmd} {args:?} failed in {}", dir.display());
}

fn make_git_folder(base: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = base.join(name);
    std::fs::create_dir_all(&root).unwrap();
    for (rel, content) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
    }
    sh(&root, "git", &["init", "-q"]);
    sh(&root, "git", &["add", "-A"]);
    sh(
        &root,
        "git",
        &["-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-q", "-m", "init"],
    );
    root
}

async fn signaling_url(port: u16) -> (String, Option<tokio::process::Child>) {
    if let Ok(url) = std::env::var("LS_NET_TEST_SIGNALING_URL") {
        return (url, None);
    }
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../apps/signaling-server/index.js");
    let child = tokio::process::Command::new("node")
        .arg(script)
        .env("PORT", port.to_string())
        .kill_on_drop(true)
        .spawn()
        .expect("failed to spawn `node` for the signaling server");
    tokio::time::sleep(Duration::from_millis(300)).await;
    (format!("ws://127.0.0.1:{port}"), Some(child))
}

fn isolate_sender_home() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::set_var(home_var, dir.path());
    dir
}

#[tokio::test]
async fn share_snapshot_wizard_bundles_multiple_folders_without_a_database() {
    let _home = isolate_sender_home();
    let base = tempfile::tempdir().unwrap();
    let a = make_git_folder(base.path(), "orders-service", &[("README.md", "orders\n")]);
    let b = make_git_folder(base.path(), "billing-service", &[("README.md", "billing\n")]);

    let (url, _server) = signaling_url(8126).await;
    let room = format!("wizard-multi-folder-{}", std::process::id());

    let sender_app = tauri::test::mock_app();
    sender_app.manage(AppState::default());
    let sender_handle = sender_app.handle().clone();

    let receiver_app = tauri::test::mock_app();
    receiver_app.manage(AppState::default());
    let receiver_handle = receiver_app.handle().clone();

    let folders = vec![
        commands::FolderPlanDto { path: a.display().to_string(), dump: None },
        commands::FolderPlanDto { path: b.display().to_string(), dump: None },
    ];

    let sender_room = room.clone();
    let sender_url = url.clone();
    let sender_task = tokio::spawn(async move {
        let state = sender_handle.state::<AppState>();
        commands::share_snapshot_wizard(sender_handle.clone(), state, folders, sender_room, sender_url).await
    });

    let receiver_room = room.clone();
    let receiver_url = url.clone();
    let receiver_task = tokio::spawn(async move {
        let state = receiver_handle.state::<AppState>();
        commands::receive_snapshot(receiver_handle.clone(), state, receiver_room, receiver_url).await
    });

    let snapshot_id = sender_task.await.unwrap().expect("share_snapshot_wizard should succeed");
    let info = receiver_task.await.unwrap().expect("receive_snapshot should succeed");

    assert_eq!(info.snapshot_id, snapshot_id);
    assert_eq!(info.manifest.project_name, "orders-service+billing-service");
    assert_eq!(info.manifest.folders.len(), 2);
    assert_eq!(info.manifest.folders[0].name, "orders-service");
    assert_eq!(info.manifest.folders[1].name, "billing-service");
    assert!(info.manifest.database_dumps.is_empty());

    // The diff.rs round-17 fix: without it, receive_snapshot itself would
    // have failed outright (diff_summary bails when there's no top-level
    // diff_stat.json), so simply getting here already proves the fix - this
    // also checks the *content* is real and correctly folder-prefixed.
    assert!(!info.diff.entries.is_empty(), "a first-time multi-folder send should report added files");
    assert!(info.diff.entries.iter().any(|e| e.path == "orders-service/README.md"));
    assert!(info.diff.entries.iter().any(|e| e.path == "billing-service/README.md"));
}

// ---------- real local MariaDB instance for the second test ----------

struct TestServer {
    child: Child,
    _tempdir: tempfile::TempDir,
    port: u16,
}

impl TestServer {
    fn start() -> Self {
        let tempdir = tempfile::tempdir().unwrap();
        let datadir = tempdir.path().join("data");
        std::fs::create_dir_all(&datadir).unwrap();
        let socket_path = tempdir.path().join("mysqld.sock");
        let error_log = tempdir.path().join("error.log");

        let install = Command::new("mariadb-install-db")
            .arg("--no-defaults")
            .arg(format!("--datadir={}", datadir.display()))
            .arg("--auth-root-authentication-method=normal")
            .arg("--skip-test-db")
            .output()
            .expect("mariadb-install-db should be on PATH in this environment");
        assert!(install.status.success(), "mariadb-install-db failed: {:?}", install);

        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };

        let child = Command::new("mariadbd")
            .arg("--no-defaults")
            .arg(format!("--datadir={}", datadir.display()))
            .arg(format!("--socket={}", socket_path.display()))
            .arg(format!("--port={port}"))
            .arg("--bind-address=127.0.0.1")
            .arg("--skip-grant-tables")
            .arg(format!("--log-error={}", error_log.display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("mariadbd should be on PATH in this environment");

        let mut server = TestServer { child, _tempdir: tempdir, port };
        server.wait_until_ready(&error_log);
        server
    }

    fn wait_until_ready(&mut self, error_log: &Path) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                let log = std::fs::read_to_string(error_log).unwrap_or_default();
                panic!("mariadbd exited early with {status} before becoming ready; log:\n{log}");
            }
            let ok = Command::new("mysql")
                .args(["-h127.0.0.1", "-P", &self.port.to_string(), "-uroot", "-e", "SELECT 1"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if ok {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        panic!("mariadbd never became ready within 20s");
    }

    fn sh_sql(&self, database: Option<&str>, sql: &str) {
        let mut args = vec!["-h127.0.0.1".to_string(), "-P".to_string(), self.port.to_string(), "-uroot".to_string()];
        if let Some(db) = database {
            args.push(db.to_string());
        }
        let mut child = Command::new("mysql")
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("mysql CLI should be on PATH");
        child.stdin.take().unwrap().write_all(sql.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "mysql -e failed: {}", String::from_utf8_lossy(&out.stderr));
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Graceful SQL SHUTDOWN, not Child::kill(): this sandbox's mariadbd
        // binary runs under an AppArmor profile that denies *any* process -
        // including its own direct parent - from delivering it a signal at
        // all (confirmed via dmesg's audit log: "apparmor DENIED ...
        // profile=mariadbd ... signal=kill/term"). kill() fails silently and
        // the wait() below would otherwise hang forever on a process that
        // can never die from a signal in this environment. SHUTDOWN isn't a
        // signal - it travels over the real MySQL wire protocol, so it
        // isn't subject to that mediation at all.
        let opts = mysql::OptsBuilder::new()
            .ip_or_hostname(Some("127.0.0.1"))
            .tcp_port(self.port)
            .user(Some("root"))
            .tcp_connect_timeout(Some(Duration::from_secs(2)));
        if let Ok(mut conn) = mysql::Conn::new(opts) {
            use mysql::prelude::Queryable;
            let _ = conn.query_drop("SHUTDOWN");
        }
        let _ = self.child.wait();
    }
}

fn unpack_tar_gz(payload: &[u8]) -> std::collections::HashMap<String, Vec<u8>> {
    let decoder = flate2::read::GzDecoder::new(payload);
    let mut archive = tar::Archive::new(decoder);
    let mut out = std::collections::HashMap::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).unwrap();
        out.insert(path, buf);
    }
    out
}

#[tokio::test]
async fn full_wizard_flow_against_a_real_local_mysql_instance() {
    let _home = isolate_sender_home();
    let server = TestServer::start();
    server.sh_sql(None, "CREATE DATABASE inventory_db");
    server.sh_sql(
        Some("inventory_db"),
        "CREATE TABLE items (id INT PRIMARY KEY, name VARCHAR(64) NOT NULL); \
         INSERT INTO items (id, name) VALUES (1, 'widget'), (2, 'gadget');",
    );

    // A Spring-Boot-shaped folder, no existing dump — exactly the scenario
    // this round's own definition of done describes. The port is real and
    // dynamic (this disposable test server's own), everything else matches
    // the developer's real application.properties shape.
    let base = tempfile::tempdir().unwrap();
    let project = make_git_folder(
        base.path(),
        "inventory-service",
        &[
            ("README.md", "inventory service\n"),
            (
                "src/main/resources/application.properties",
                &format!(
                    "spring.datasource.url=jdbc:mysql://127.0.0.1:{}/inventory_db\nspring.datasource.username=root\n",
                    server.port
                ),
            ),
        ],
    );

    // ---- Step 3: auto-detection, via the real command. ----
    let detected = commands::detect_db_connection(project.display().to_string())
        .unwrap()
        .expect("application.properties should be auto-detected");
    assert_eq!(detected.details.host, "127.0.0.1");
    assert_eq!(detected.details.port, server.port);
    assert_eq!(detected.details.database, "inventory_db");

    // ---- Step 4 ("No, connect and export"): real connection + real table list. ----
    commands::test_db_connection(detected.details.clone()).await.expect("test_db_connection should succeed");
    let tables = commands::list_db_tables(detected.details.clone()).await.expect("list_db_tables should succeed");
    assert!(tables.iter().any(|t| t.name == "items"));

    // ---- Real, full export via the actual command (writes a real file to disk). ----
    let exported = commands::export_db_tables(detected.details.clone(), vec!["items".to_string()])
        .await
        .expect("export_db_tables should succeed");
    assert!(exported.size_bytes > 0);
    let dump_bytes_on_disk = std::fs::read(&exported.file_path).unwrap();
    assert!(String::from_utf8_lossy(&dump_bytes_on_disk).contains("INSERT INTO `items`"));

    // ---- Step 6: package + send via the real wizard command, exactly the
    // shape app.js builds after a successful export. ----
    let (url, _server_proc) = signaling_url(8127).await;
    let room = format!("wizard-full-db-flow-{}", std::process::id());

    let sender_app = tauri::test::mock_app();
    sender_app.manage(AppState::default());
    let sender_handle = sender_app.handle().clone();

    let receiver_app = tauri::test::mock_app();
    receiver_app.manage(AppState::default());
    let receiver_handle = receiver_app.handle().clone();

    let folders = vec![commands::FolderPlanDto {
        path: project.display().to_string(),
        dump: Some(commands::DumpPlanDto {
            schema: detected.details.database.clone(),
            file_path: exported.file_path.clone(),
            engine: detected.details.engine.clone(),
        }),
    }];

    let sender_room = room.clone();
    let sender_url = url.clone();
    let sender_task = tokio::spawn(async move {
        let state = sender_handle.state::<AppState>();
        commands::share_snapshot_wizard(sender_handle.clone(), state, folders, sender_room, sender_url).await
    });
    let receiver_room = room.clone();
    let receiver_url = url.clone();
    let receiver_task = tokio::spawn(async move {
        let state = receiver_handle.state::<AppState>();
        commands::receive_snapshot(receiver_handle.clone(), state, receiver_room, receiver_url).await
    });

    let snapshot_id = sender_task.await.unwrap().expect("share_snapshot_wizard should succeed");
    let info = receiver_task.await.unwrap().expect("receive_snapshot should succeed");
    assert_eq!(info.snapshot_id, snapshot_id);

    // ---- The manifest carries exactly one, correctly-tagged dump entry. ----
    assert_eq!(info.manifest.database_dumps.len(), 1);
    let entry = &info.manifest.database_dumps[0];
    assert_eq!(entry.folder, "inventory-service");
    assert_eq!(entry.schema, "inventory_db");
    assert_eq!(entry.dump_file, "db-dumps/inventory-service/inventory_db.sql");
    assert_eq!(entry.engine, "mysql", "round 18: the manifest must record which engine this dump came from");
    let expected_hash = {
        use sha2::{Digest, Sha256};
        Sha256::digest(&dump_bytes_on_disk).iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    assert_eq!(entry.hash, expected_hash, "manifest hash must match the real exported dump's bytes");

    // ---- And the payload the receiver actually holds contains those exact
    // bytes at that exact path - not just a manifest claim. ----
    let verified = receiver_app.state::<AppState>();
    let held = verified.verified.lock().unwrap();
    let vs = held.get(&info.snapshot_id).expect("receive_snapshot should stash the verified snapshot");
    let files = unpack_tar_gz(&vs.snapshot().payload);
    let payload_dump = files
        .get("db-dumps/inventory-service/inventory_db.sql")
        .expect("payload should contain the packaged dump at the manifest's own dump_file path");
    assert_eq!(payload_dump, &dump_bytes_on_disk);
}

fn podman_stack_available() -> bool {
    let ok = |bin: &str| {
        Command::new(bin)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    ok("podman") && ok("podman-compose")
}

/// Round 30 regression test - see this file's own module doc comment for
/// the full root cause. Before the fix, this failed at the `run_snapshot`
/// step with exactly the error reported from real cross-machine testing:
/// "snapshot payload has no docker-compose.yml", despite the sent project
/// genuinely having one right here in its own git-tracked files.
#[tokio::test]
async fn a_single_folder_wizard_send_can_actually_be_run() {
    if !podman_stack_available() {
        eprintln!("podman/podman-compose not on PATH — skipping a_single_folder_wizard_send_can_actually_be_run");
        return;
    }

    let _home = isolate_sender_home();
    let base = tempfile::tempdir().unwrap();
    // busybox's own tiny httpd, not a real app image: this test's whole
    // point is proving docker-compose.yml itself survives the wizard's
    // packaging and is where podman-compose can actually find it - not
    // re-proving a slow real build, which pipeline_test.rs/
    // pipeline_node_test.rs already do for the older, non-wizard send
    // path. nginx:alpine was tried first and rejected for a real reason,
    // not a style preference: run_snapshot's own ls_security::
    // default_policy() forces `read_only: true` with only `/tmp` mounted
    // writable (compose::apply_policy), and nginx's default startup needs
    // to mkdir under /var/cache/nginx - it exits immediately with "mkdir()
    // ... failed (30: Read-only file system)" under that exact policy,
    // confirmed directly by reproducing it with plain podman-compose
    // outside this test entirely. Pointing busybox's own httpd at /tmp
    // (the one path this policy already keeps writable) sidesteps that
    // rather than fighting it - a real, pre-existing, separate policy/
    // image-compatibility question (should apply_policy mount more than
    // just /tmp?) that's out of this round's own scope to fix.
    let compose_yaml = r#"
services:
  web:
    image: docker.io/library/busybox:latest
    command: ["httpd", "-f", "-p", "80", "-h", "/tmp"]
    ports:
      - "18080:80"
"#;
    let project = make_git_folder(base.path(), "single-service", &[("docker-compose.yml", compose_yaml)]);

    let (url, _server) = signaling_url(8129).await;
    let room = format!("wizard-single-folder-run-{}", std::process::id());

    let sender_app = tauri::test::mock_app();
    sender_app.manage(AppState::default());
    let sender_handle = sender_app.handle().clone();

    let receiver_app = tauri::test::mock_app();
    receiver_app.manage(AppState::default());
    let receiver_handle = receiver_app.handle().clone();

    let folders = vec![commands::FolderPlanDto { path: project.display().to_string(), dump: None }];

    let sender_room = room.clone();
    let sender_url = url.clone();
    let sender_task = tokio::spawn(async move {
        let state = sender_handle.state::<AppState>();
        commands::share_snapshot_wizard(sender_handle.clone(), state, folders, sender_room, sender_url).await
    });
    let receiver_room = room.clone();
    let receiver_url = url.clone();
    let receive_handle = receiver_handle.clone();
    let receiver_task = tokio::spawn(async move {
        let state = receive_handle.state::<AppState>();
        commands::receive_snapshot(receive_handle.clone(), state, receiver_room, receiver_url).await
    });

    let snapshot_id = sender_task.await.unwrap().expect("share_snapshot_wizard should succeed");
    let info = receiver_task.await.unwrap().expect("receive_snapshot should succeed");
    assert_eq!(info.snapshot_id, snapshot_id);
    // A single-folder wizard send still records the folder breakdown -
    // this is exactly the `manifest.folders` entry run_snapshot now has to
    // account for.
    assert_eq!(info.manifest.folders.len(), 1);
    assert_eq!(info.manifest.folders[0].name, "single-service");

    // ---- The actual regression: this used to fail here with "snapshot
    // payload has no docker-compose.yml". ----
    let work_dir = tempfile::tempdir().unwrap();
    let run_state = receiver_app.state::<AppState>();
    let run_info = commands::run_snapshot(
        receiver_handle.clone(),
        run_state,
        info.snapshot_id.clone(),
        work_dir.path().display().to_string(),
    )
    .await
    .expect("run_snapshot should bring up a single-folder wizard send's real docker-compose.yml");

    assert!(
        run_info.service_ports.iter().any(|(svc, ports)| svc == "web" && ports.contains("18080")),
        "expected the web service's published port in {:?}",
        run_info.service_ports
    );

    // Not just "no error" - the container is actually up and accepting
    // connections, same real-reachability bar pipeline_test.rs holds itself
    // to for the non-wizard send path.
    let reachable = {
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut ok = false;
        while Instant::now() < deadline {
            if TcpStream::connect("127.0.0.1:18080").is_ok() {
                ok = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        ok
    };

    let stop_state = receiver_app.state::<AppState>();
    let stop_result = commands::stop_session(stop_state, run_info.session_id.clone()).await;
    assert!(stop_result.is_ok(), "stop_session should tear the project down: {stop_result:?}");

    assert!(reachable, "busybox httpd on port 18080 never accepted a TCP connection within the timeout");
}
