//! A Cloud drop carries the sender session's prepared artifact - the same
//! bytes a P2P send carries - so a project set up in the wizard (a generated
//! compose file + Dockerfile, a MySQL dump, no docker-compose.yml of its own)
//! arrives runnable, database included. And a granted Cloud drop is recorded
//! on the sender session as a device, like a P2P send.
//!
//! Everything except Google Drive is real: the payload is built by the same
//! `cloud_drop_payload` the command uploads, handed to the receiver's real
//! `finalize_received_snapshot` (what `request_cloud_drop_access` does with
//! the downloaded bytes) and run with real Podman; the handoff is real
//! WebRTC over the local relay; the grant's recording is the same
//! `record_cloud_delivery` `respond_to_cloud_access_request` calls after the
//! Drive grant. Skips the Run where podman/podman-compose are absent.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use ls_composegen::{BuildTool, ComposeSpec, DatabaseSpec, DbEngine, DbEnvPreset, JavaPackaging, Runtime};
use localsync_desktop::commands::{self, DumpPlanDto, FolderPlanDto};
use localsync_desktop::state::{AppState, CloudDelivery};
use localsync_desktop::{receiver_session_commands, session_commands};
use tauri::Manager;

type Handle = tauri::AppHandle<tauri::test::MockRuntime>;

const PORT: u16 = 18151;

fn podman_stack_available() -> bool {
    let ok = |bin: &str| Command::new(bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    ok("podman") && ok("podman-compose")
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git").args(["-c", "user.name=T", "-c", "user.email=t@example.com"]).args(args).current_dir(dir).status().unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn app() -> Handle {
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    app.handle().clone()
}

async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..300 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for: {what}");
}

#[tokio::test]
async fn a_wizard_project_with_a_mysql_dump_arrives_runnable_and_the_receiver_is_recorded() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home.path());
    std::env::set_var("XDG_DATA_HOME", home.path().join("data"));

    // ---- the sender: no docker-compose.yml, a MySQL dump, set up in the wizard ----
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("cloud-drop-app");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("index.html"), "<h1>app</h1>").unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "one"]);
    let dump_path = base.path().join("appdb.sql");
    std::fs::write(
        &dump_path,
        "SET FOREIGN_KEY_CHECKS=0;\nCREATE TABLE `items` (`id` int NOT NULL, PRIMARY KEY (`id`));\nINSERT INTO `items` VALUES (1),(2),(3);\nSET FOREIGN_KEY_CHECKS=1;\n",
    )
    .unwrap();
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
    let sender = app();
    let sid = session_commands::create_project_session(sender.state::<AppState>(), folders, None).await.unwrap().id;

    // ---- what a Cloud drop uploads ----
    let (filename, artifact) = session_commands::cloud_drop_payload(&sender.state::<AppState>(), &sid).await.unwrap();
    assert!(filename.ends_with(".localsync-snapshot"));
    let snapshot: ls_snapshot::Snapshot = serde_json::from_slice(&artifact.bytes).expect("the receiver's from_slice::<Snapshot> path");
    let services: Vec<&str> = snapshot.manifest.services.iter().map(|s| s.name.as_str()).collect();
    assert!(services.contains(&"app") && services.contains(&"mysql"), "the generated compose file is in it: {services:?}");
    assert_eq!(snapshot.manifest.database_dumps.len(), 1, "the dump is in it");
    assert_eq!(snapshot.manifest.database_dumps[0].engine, "mysql");
    // The bug: packaging the folder on its own (what Cloud drop used to do)
    // has neither - the receiver then had "no docker-compose.yml".
    let raw = ls_snapshot::create_snapshot(&project, None).unwrap();
    assert!(raw.manifest.services.is_empty() && raw.manifest.database_dumps.is_empty(), "the old raw snapshot lacked both");

    // ---- the receiver downloads those bytes and runs them ----
    let receiver = app();
    let info = commands::finalize_received_snapshot(&receiver.state::<AppState>(), snapshot).expect("verified and filed");
    if podman_stack_available() {
        let volume = format!("localsync-db-{}", info.manifest.db_seed_hash);
        let _ = Command::new("podman").args(["volume", "rm", "-f", &volume]).output();
        let work = tempfile::tempdir().unwrap();
        let ran = receiver_session_commands::run_received_session(
            receiver.clone(),
            receiver.state::<AppState>(),
            info.received_session_id.clone(),
            work.path().display().to_string(),
        )
        .await
        .expect("Run works");
        let rows = Command::new("podman")
            .args(["exec", &format!("{}_mysql_1", ran.running.session_id), "mysql", "-uroot", "-plocalsync_root_pw", "-N", "-e", "SELECT COUNT(*) FROM items", "appdb"])
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&rows.stdout).trim().to_string();
        commands::stop_session(receiver.state::<AppState>(), ran.running.session_id.clone()).await.unwrap();
        let _ = Command::new("podman").args(["volume", "rm", "-f", &volume]).output();
        assert_eq!(out, "3", "the dump was imported (stderr: {})", String::from_utf8_lossy(&rows.stderr));
    } else {
        eprintln!("podman/podman-compose not on PATH - skipping the Run");
    }

    // ---- handoff: the receiver enters the code and asks for access ----
    let delivery = CloudDelivery {
        session_id: sid.clone(),
        snapshot_id: artifact.snapshot_id.clone(),
        commits: artifact.commits.clone(),
        bytes: artifact.bytes.len() as u64,
        upload_ms: 2000,
    };
    let handoff = commands::begin_cloud_drop_handoff(
        &sender,
        "local".into(),
        None,
        "drive-file-1".into(),
        ls_clouddrop::retention::Retention::DeleteAfterDownload,
        delivery.clone(),
    )
    .await
    .unwrap();
    let decoded = commands::decode_room_code(handoff.room_code.clone(), None).unwrap();
    let conn = ls_net::connect_as_receiver(&decoded.signaling_url, &decoded.room_id).await.unwrap();
    ls_net::send_control(&conn, &ls_net::ControlMessage::CloudAccessRequest { google_email: "bob@example.com".into() }).await.unwrap();
    until("the access request", || sender.state::<AppState>().cloud_access_requests.lock().unwrap().contains_key(&handoff.room_id)).await;
    let filed = sender.state::<AppState>().cloud_drop_uploads.lock().unwrap().get(&handoff.room_id).map(|u| u.delivery.session_id.clone());
    assert_eq!(filed.as_deref(), Some(sid.as_str()), "the upload knows its sender session");

    // ---- the grant is recorded on the session, like a P2P send ----
    let view = session_commands::record_cloud_delivery(&sender.state::<AppState>(), &delivery, "bob@example.com").unwrap().expect("session open");
    let bob = view.devices.iter().find(|d| d.key == "cloud:bob@example.com").expect("the receiver is a device");
    assert_eq!(bob.name, "bob@example.com");
    assert_eq!(bob.marker.as_ref().map(|m| m.snapshot_id.as_str()), Some(artifact.snapshot_id.as_str()));
    assert!(bob.last_sent_at.is_some() && bob.bytes_per_sec.is_some(), "Last sent and a speed");
    let status = session_commands::project_push_status(sender.state::<AppState>(), sid.clone()).await.unwrap();
    assert_eq!(status.last_device_key.as_deref(), Some("cloud:bob@example.com"));
    assert!(!status.changed, "bob has the current version");
    std::fs::write(project.join("index.html"), "<h1>app v2</h1>").unwrap();
    let status = session_commands::project_push_status(sender.state::<AppState>(), sid.clone()).await.unwrap();
    assert!(status.changed, "an edit is something to push to bob");
    // Saved, so it survives a restart.
    let saved = localsync_desktop::session_history::find_project(&sid).unwrap().expect("saved");
    assert!(saved.device("cloud:bob@example.com").is_some());
    commands::cancel_cloud_drop(sender.state::<AppState>(), handoff.room_id.clone()).unwrap();
}
