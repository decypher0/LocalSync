//! Applying an update to a received session that ran, with real Podman:
//!
//! - the database: an update with the same dump reuses the receiver's volume
//!   (its own data stays); a changed dump with "keep" still runs on that old
//!   volume - not on a new, freshly seeded one; "replace" imports the
//!   sender's new dump fresh.
//! - a new version that doesn't come up healthy is taken down again, the
//!   update stays waiting, and Roll back brings the previous version back up.
//!
//! Network-free: snapshots are the sender session's real artifact (what a
//! Cloud drop uploads and a P2P send carries), handed to the receiver's real
//! `finalize_received_snapshot`. Skips where podman/podman-compose are
//! absent. One `#[tokio::test]`: it sets the process-wide `HOME` and
//! `LOCALSYNC_HEALTH_TIMEOUT_SECS`.

use std::path::Path;
use std::process::Command;

use ls_composegen::{BuildTool, ComposeSpec, DatabaseSpec, DbEngine, DbEnvPreset, JavaPackaging, Runtime};
use localsync_desktop::commands::{self, DumpPlanDto, FolderPlanDto, IncomingSnapshotInfo};
use localsync_desktop::receiver_session_commands::{self, RunReceivedResult};
use localsync_desktop::session_commands;
use localsync_desktop::state::AppState;
use tauri::Manager;

type Handle = tauri::AppHandle<tauri::test::MockRuntime>;

const APP_PORT: u16 = 18171;
const WEB_PORT: u16 = 18172;

fn podman_stack_available() -> bool {
    let ok = |bin: &str| Command::new(bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    ok("podman") && ok("podman-compose")
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git").args(["-c", "user.name=T", "-c", "user.email=t@example.com"]).args(args).current_dir(dir).status().unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

fn app() -> Handle {
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    app.handle().clone()
}

fn volume_exists(name: &str) -> bool {
    Command::new("podman").args(["volume", "exists", name]).status().map(|s| s.success()).unwrap_or(false)
}

fn http_ok(port: u16) -> bool {
    std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
}

fn dump(ids: &[u32]) -> String {
    let values = ids.iter().map(|i| format!("({i})")).collect::<Vec<_>>().join(",");
    format!("SET FOREIGN_KEY_CHECKS=0;\nCREATE TABLE `items` (`id` int NOT NULL, PRIMARY KEY (`id`));\nINSERT INTO `items` VALUES {values};\nSET FOREIGN_KEY_CHECKS=1;\n")
}

/// The sender session's current artifact, received.
async fn receive(sender: &Handle, receiver: &Handle, sid: &str) -> IncomingSnapshotInfo {
    let (_, artifact) = session_commands::cloud_drop_payload(&sender.state::<AppState>(), sid).await.expect("the sender's artifact");
    let snapshot: ls_snapshot::Snapshot = serde_json::from_slice(&artifact.bytes).unwrap();
    commands::finalize_received_snapshot(&receiver.state::<AppState>(), snapshot).expect("verified and filed")
}

async fn apply(receiver: &Handle, id: &str, db_choice: Option<&str>) -> Result<RunReceivedResult, String> {
    receiver_session_commands::apply_received_update(receiver.clone(), receiver.state::<AppState>(), id.to_string(), db_choice.map(String::from)).await
}

fn mysql(project: &str, sql: &str) -> String {
    let out = Command::new("podman")
        .args(["exec", &format!("{project}_mysql_1"), "mysql", "-uroot", "-plocalsync_root_pw", "-N", "-e", sql, "appdb"])
        .output()
        .unwrap();
    assert!(out.status.success(), "mysql: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).split_whitespace().collect::<Vec<_>>().join(",")
}

fn session_of(receiver: &Handle, id: &str) -> localsync_desktop::received_session::ReceivedSession {
    receiver.state::<AppState>().received_sessions.lock().unwrap().get(id).cloned().unwrap()
}

#[tokio::test]
async fn applying_updates_keeps_or_replaces_the_database_and_rolls_back_a_broken_version() {
    if !podman_stack_available() {
        eprintln!("podman/podman-compose not on PATH - skipping");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home.path());
    std::env::set_var("XDG_DATA_HOME", home.path().join("data"));
    std::env::set_var("LOCALSYNC_HEALTH_TIMEOUT_SECS", "10");
    let base = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let (sender, receiver) = (app(), app());

    // ================= the database across updates =================
    let project = base.path().join("update-db-app");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("index.html"), "<h1>v1</h1>").unwrap();
    git(&project, &["init", "-q"]);
    commit_all(&project, "one");
    let dump_path = base.path().join("appdb.sql");
    std::fs::write(&dump_path, dump(&[1, 2, 3])).unwrap();
    let spec = ComposeSpec {
        runtime: Runtime::Python,
        runtime_version: "3.12".into(),
        build_tool: BuildTool::Pip,
        run_command: format!("python -m http.server {APP_PORT}"),
        port: APP_PORT,
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

    let v1 = receive(&sender, &receiver, &sid).await;
    let id = v1.received_session_id.clone();
    let first_volume = format!("localsync-db-{}", v1.manifest.db_seed_hash);
    let _ = Command::new("podman").args(["volume", "rm", "-f", &first_volume]).output();
    let ran = receiver_session_commands::run_received_session(receiver.clone(), receiver.state::<AppState>(), id.clone(), work.path().display().to_string())
        .await
        .expect("the first version runs");
    assert_eq!(mysql(&ran.running.session_id, "SELECT id FROM items ORDER BY id"), "1,2,3");
    // The receiver's own data, which no update may lose unless asked to.
    mysql(&ran.running.session_id, "INSERT INTO items VALUES (99)");

    // ---- 1. same dump: the volume is reused, the receiver's row is still there ----
    std::fs::write(project.join("index.html"), "<h1>v2</h1>").unwrap();
    commit_all(&project, "two");
    let v2 = receive(&sender, &receiver, &sid).await;
    assert_eq!(v2.received_session_id, id, "the update lands on the session");
    assert_eq!(v2.manifest.db_seed_hash, v1.manifest.db_seed_hash);
    let pending = session_of(&receiver, &id).pending_update.expect("waiting to be applied");
    assert!(!pending.db_changed);
    let applied = apply(&receiver, &id, None).await.expect("apply & restart");
    assert!(applied.running.db_cache_hit, "the existing volume was reused");
    assert!(
        !ls_containers::project_running(&v1.manifest.project_name, &v1.manifest.git_commit).unwrap(),
        "the previous version was taken down"
    );
    assert_eq!(mysql(&applied.running.session_id, "SELECT id FROM items ORDER BY id"), "1,2,3,99");
    let s = session_of(&receiver, &id);
    assert!(s.pending_update.is_none() && s.previous_run.is_none());
    assert_eq!(s.db_volume, first_volume);

    // ---- 2. a changed dump, "keep": still the old volume, not a new one seeded from the new dump ----
    std::fs::write(&dump_path, dump(&[7])).unwrap();
    std::fs::write(project.join("index.html"), "<h1>v3</h1>").unwrap();
    commit_all(&project, "three");
    let v3 = receive(&sender, &receiver, &sid).await;
    assert_ne!(v3.manifest.db_seed_hash, v1.manifest.db_seed_hash, "the dump changed");
    let view = receiver_session_commands::open_saved_received_session(receiver.state::<AppState>(), id.clone()).unwrap();
    let update = view.update.expect("the page offers the update");
    assert!(update.db_changed, "so the page asks keep or replace");
    assert!(update.previous_running, "and says it restarts the running version");
    let kept = apply(&receiver, &id, Some("keep")).await.expect("apply, keeping my data");
    assert_eq!(mysql(&kept.running.session_id, "SELECT id FROM items ORDER BY id"), "1,2,3,99", "the receiver's data stayed");
    let new_dump_volume = format!("localsync-db-{}", v3.manifest.db_seed_hash);
    assert!(!volume_exists(&new_dump_volume), "no volume was seeded from the new dump");
    assert_eq!(session_of(&receiver, &id).db_volume, first_volume);

    // ---- 3. changed again, "replace": the sender's new dump, imported fresh ----
    std::fs::write(&dump_path, dump(&[8, 9])).unwrap();
    std::fs::write(project.join("index.html"), "<h1>v4</h1>").unwrap();
    commit_all(&project, "four");
    let v4 = receive(&sender, &receiver, &sid).await;
    assert!(session_of(&receiver, &id).pending_update.unwrap().db_changed, "measured against the dump of the version that ran");
    let replaced = apply(&receiver, &id, Some("replace")).await.expect("apply, replacing with the sender's database");
    assert_eq!(mysql(&replaced.running.session_id, "SELECT id FROM items ORDER BY id"), "8,9");
    let replaced_volume = format!("localsync-db-{}", v4.manifest.db_seed_hash);
    assert_eq!(session_of(&receiver, &id).db_volume, replaced_volume);
    receiver_session_commands::stop_received_session(receiver.state::<AppState>(), id.clone()).await.expect("stop");
    let stopped = session_of(&receiver, &id);
    assert_eq!(stopped.last_exit, "stopped");
    assert!(!stopped.last_stopped_at.is_empty() && !stopped.last_run_at.is_empty());
    for volume in [&first_volume, &new_dump_volume, &replaced_volume] {
        let _ = Command::new("podman").args(["volume", "rm", "-f", volume]).output();
    }

    // ================= a version that fails its health check, rolled back =================
    let web = base.path().join("update-rollback-app");
    std::fs::create_dir(&web).unwrap();
    let compose = |healthcheck: &str| {
        format!(
            "services:\n  web:\n    image: docker.io/library/python:3.12-slim\n    command: [\"python\", \"-m\", \"http.server\", \"80\"]\n    ports:\n      - \"{WEB_PORT}:80\"\n{healthcheck}"
        )
    };
    std::fs::write(web.join("docker-compose.yml"), compose("")).unwrap();
    git(&web, &["init", "-q"]);
    commit_all(&web, "one");
    let web_sid = session_commands::create_project_session(
        sender.state::<AppState>(),
        vec![FolderPlanDto { path: web.display().to_string(), dump: None, compose: None }],
        None,
    )
    .await
    .unwrap()
    .id;
    let w1 = receive(&sender, &receiver, &web_sid).await;
    let wid = w1.received_session_id.clone();
    receiver_session_commands::run_received_session(receiver.clone(), receiver.state::<AppState>(), wid.clone(), work.path().display().to_string())
        .await
        .expect("the first version runs");
    assert!(http_ok(WEB_PORT));

    std::fs::write(
        web.join("docker-compose.yml"),
        compose("    healthcheck:\n      test: [\"CMD\", \"false\"]\n      interval: 2s\n      retries: 1\n"),
    )
    .unwrap();
    commit_all(&web, "two: never healthy");
    let w2 = receive(&sender, &receiver, &web_sid).await;
    assert_eq!(w2.received_session_id, wid);
    let err = apply(&receiver, &wid, None).await.err().expect("a version that never gets healthy must not be applied");
    eprintln!("--- apply error ---\n{err}");
    assert!(err.contains("health check"), "{err}");
    assert!(!ls_containers::project_running(&w2.manifest.project_name, &w2.manifest.git_commit).unwrap(), "the broken version was taken down");
    assert!(!ls_containers::project_running(&w1.manifest.project_name, &w1.manifest.git_commit).unwrap(), "precondition: Apply had stopped the old one");
    let waiting = session_of(&receiver, &wid);
    assert!(waiting.pending_update.as_ref().is_some_and(|p| p.previous_was_running), "the update is still waiting, and knows what ran");
    assert_eq!(waiting.last_exit, "failed");
    assert!(receiver.state::<AppState>().verified.lock().unwrap().contains_key(&w2.snapshot_id), "held again, so Apply can be retried");

    let back = receiver_session_commands::rollback_received_update(receiver.state::<AppState>(), wid.clone()).await.expect("roll back");
    assert!(back.running, "the previous version is running again");
    assert_eq!(back.git_commit, w1.manifest.git_commit);
    assert!(back.update.is_none());
    assert!(http_ok(WEB_PORT), "and serving");
    assert!(!receiver.state::<AppState>().verified.lock().unwrap().contains_key(&w2.snapshot_id), "the dropped update isn't held any more");
    let again = apply(&receiver, &wid, None).await.err().expect("nothing left to apply");
    assert!(again.contains("no update"), "{again}");
    receiver_session_commands::stop_received_session(receiver.state::<AppState>(), wid.clone()).await.expect("stop");
    assert!(!http_ok(WEB_PORT));
}
