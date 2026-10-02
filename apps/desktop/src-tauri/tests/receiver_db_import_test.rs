//! The receiver's Run with a database dump that fails to import: it must
//! fail with the database's real error and discard the half-imported volume,
//! so the next Run re-imports instead of finding an existing data directory
//! (MySQL then skips its init scripts) and passing silently on a broken
//! database. A dump that imports keeps its volume as the cache, as always.
//!
//! Real sender -> relay -> receiver flow (same box) and real Podman; skips
//! where podman/podman-compose are absent.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use ls_composegen::{BuildTool, ComposeSpec, DatabaseSpec, DbEngine, DbEnvPreset, JavaPackaging, Runtime};
use localsync_desktop::commands::{self, DumpPlanDto, FolderPlanDto, IncomingSnapshotInfo};
use localsync_desktop::receiver_session_commands;
use localsync_desktop::session_commands::{self, SendRequest};
use localsync_desktop::state::AppState;
use tauri::Manager;

type Handle = tauri::AppHandle<tauri::test::MockRuntime>;

const PORT: u16 = 18131;

fn podman_stack_available() -> bool {
    let ok = |bin: &str| Command::new(bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    ok("podman") && ok("podman-compose")
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
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
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (format!("ws://127.0.0.1:{port}"), Some(child))
}

/// `children` references `parents`, created later: without FK checks off,
/// MySQL 8 refuses it (ERROR 1824). `fixed` wraps it the way the exporter now
/// does.
fn fk_order_dump(fixed: bool) -> String {
    let body = "-- Table: children\n\
        DROP TABLE IF EXISTS `children`;\n\
        CREATE TABLE `children` (`id` int NOT NULL, `parent_id` int NOT NULL, PRIMARY KEY (`id`), KEY `fk` (`parent_id`), \
        CONSTRAINT `fk` FOREIGN KEY (`parent_id`) REFERENCES `parents` (`id`)) ENGINE=InnoDB;\n\
        INSERT INTO `children` (`id`,`parent_id`) VALUES (10,1);\n\
        -- Table: parents\n\
        DROP TABLE IF EXISTS `parents`;\n\
        CREATE TABLE `parents` (`id` int NOT NULL, PRIMARY KEY (`id`)) ENGINE=InnoDB;\n\
        INSERT INTO `parents` (`id`) VALUES (1);\n";
    if fixed {
        format!("SET FOREIGN_KEY_CHECKS=0;\n{body}SET FOREIGN_KEY_CHECKS=1;\n")
    } else {
        body.to_string()
    }
}

fn app() -> Handle {
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    app.handle().clone()
}

fn volume_exists(name: &str) -> bool {
    Command::new("podman").args(["volume", "exists", name]).status().map(|s| s.success()).unwrap_or(false)
}

/// Sends `project` (with its MySQL dump at `dump_path`) from a fresh
/// project session and receives it - an unarmed, fresh receive each time.
async fn push(sender: &Handle, receiver: &Handle, url: &str, room: &str, project: &Path, dump_path: &Path) -> IncomingSnapshotInfo {
    let spec = ComposeSpec {
        runtime: Runtime::Python,
        runtime_version: "3.12".into(),
        build_tool: BuildTool::Pip,
        run_command: format!("python -m http.server {PORT}"),
        port: PORT,
        artifact_path: None,
        java_packaging: JavaPackaging::Jar,
        tomcat_version: None,
        database: Some(DatabaseSpec { engine: DbEngine::Mysql, version: "8.0".into(), database: "appdb".into() }),
        db_env_preset: DbEnvPreset::Standard,
        extras: vec![],
        env: vec![],
    };
    let folders = vec![FolderPlanDto {
        path: project.display().to_string(),
        dump: Some(DumpPlanDto { schema: "appdb".into(), file_path: dump_path.display().to_string(), engine: "mysql".into() }),
        compose: Some(spec),
    }];
    let sid = session_commands::create_project_session(sender.state::<AppState>(), folders, None).await.unwrap().id;
    let request = SendRequest {
        session_id: sid,
        room_code: room.to_string(),
        signaling_url: url.to_string(),
        require_accept: false,
        sender_name: "Alice".into(),
        device_key: None,
        device_name: "Bob".into(),
        since_last: false,
    };
    let r = receiver.clone();
    let (r_room, r_url) = (room.to_string(), url.to_string());
    let recv = tokio::spawn(async move { commands::receive_snapshot(r.clone(), r.state::<AppState>(), r_room, r_url).await });
    session_commands::send_project_session(sender.clone(), sender.state::<AppState>(), request).await.expect("send");
    recv.await.unwrap().expect("receive")
}

async fn run(receiver: &Handle, info: &IncomingSnapshotInfo, work: &Path) -> Result<receiver_session_commands::RunReceivedResult, String> {
    receiver_session_commands::run_received_session(
        receiver.clone(),
        receiver.state::<AppState>(),
        info.received_session_id.clone(),
        work.display().to_string(),
    )
    .await
}

#[tokio::test]
async fn a_failed_first_boot_import_discards_its_volume_so_the_next_run_imports_again() {
    if !podman_stack_available() {
        eprintln!("podman/podman-compose not on PATH - skipping");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home.path());
    std::env::set_var("XDG_DATA_HOME", home.path().join("data"));
    let base = tempfile::tempdir().unwrap();
    let project: PathBuf = base.path().join("receiver-db-import");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("index.html"), "<h1>app</h1>").unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "one"]);
    let dump_path = base.path().join("appdb.sql");
    let (url, _server) = signaling_url(8173).await;
    let (sender, receiver) = (app(), app());
    let work = tempfile::tempdir().unwrap();

    // ---- 1. a dump that fails to import: the Run fails, with MySQL's own error ----
    std::fs::write(&dump_path, fk_order_dump(false)).unwrap();
    let broken = push(&sender, &receiver, &url, "dbroom1", &project, &dump_path).await;
    let volume = format!("localsync-db-{}", broken.manifest.db_seed_hash);
    assert!(!volume_exists(&volume), "precondition: a fresh volume for this dump");
    let err = run(&receiver, &broken, work.path()).await.err().expect("a database that crashed importing its dump must fail the Run");
    eprintln!("--- first Run error ---\n{err}");
    assert!(err.contains("The database (`mysql`) stopped"), "attributed to the database: {err}");
    assert!(err.contains("Failed to open the referenced table"), "MySQL's real error is in it: {err}");
    assert!(!volume_exists(&volume), "the half-imported volume must be discarded, not kept as a cache");

    // ---- 2. Run the same snapshot again: the import really runs again (and fails again) ----
    // Before the fix the second Run found the half-imported volume, skipped
    // the init scripts, and came up "fine" on a broken database.
    let err2 = run(&receiver, &broken, work.path()).await.err().expect("the retry must re-import - and so fail again - not reuse a broken volume");
    assert!(err2.contains("Failed to open the referenced table"), "{err2}");
    assert!(!volume_exists(&volume));

    // ---- 3. the dump is fixed and pushed again: Run succeeds, data is there, volume is kept ----
    std::fs::write(&dump_path, fk_order_dump(true)).unwrap();
    let fixed = push(&sender, &receiver, &url, "dbroom2", &project, &dump_path).await;
    let good_volume = format!("localsync-db-{}", fixed.manifest.db_seed_hash);
    let ran = run(&receiver, &fixed, work.path()).await.expect("a dump that imports runs fine");
    let rows = Command::new("podman")
        .args(["exec", &format!("{}_mysql_1", ran.running.session_id), "mysql", "-uroot", "-plocalsync_root_pw", "-N", "-e"])
        .arg("SELECT COUNT(*) FROM children; SELECT COUNT(*) FROM parents;")
        .arg("appdb")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&rows.stdout);
    commands::stop_session(receiver.state::<AppState>(), ran.running.session_id.clone()).await.unwrap();
    assert_eq!(out.split_whitespace().collect::<Vec<_>>(), ["1", "1"], "the dump really imported: {out}");
    assert!(volume_exists(&good_volume), "a database that imported fine keeps its volume (the cache)");

    let _ = Command::new("podman").args(["volume", "rm", "-f", &good_volume]).status();
}
