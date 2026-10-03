//! Removing a received session from the app never leaves its containers
//! running with nothing in the UI pointing at them:
//! - `discard_received_session` (closing a tab) stops them first;
//! - `close_session` (Home's "Close session") also stops the version that was
//!   still running when an update landed on the session.
//!
//! Real sender -> relay -> receiver flow (same box) and real Podman; skips
//! where podman/podman-compose are absent.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use localsync_desktop::commands::{self, FolderPlanDto, IncomingSnapshotInfo};
use localsync_desktop::session_commands::{self, SendRequest};
use localsync_desktop::state::AppState;
use localsync_desktop::{receiver_session_commands, session_list_commands};
use tauri::Manager;

type Handle = tauri::AppHandle<tauri::test::MockRuntime>;

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

fn make_project(base: &Path, name: &str, port: u16) -> PathBuf {
    let root = base.join(name);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("README.md"), "v1\n").unwrap();
    std::fs::write(
        root.join("docker-compose.yml"),
        format!(
            "services:\n  web:\n    image: docker.io/library/python:3.12-slim\n    command: [\"python\", \"-m\", \"http.server\", \"80\"]\n    ports:\n      - \"{port}:80\"\n"
        ),
    )
    .unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "one"]);
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
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (format!("ws://127.0.0.1:{port}"), Some(child))
}

fn app() -> Handle {
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    app.handle().clone()
}

/// Sends `sid` and receives it (a fresh receive, or an armed update).
async fn push(sender: &Handle, receiver: &Handle, url: &str, room: &str, sid: &str) -> IncomingSnapshotInfo {
    let request = SendRequest {
        session_id: sid.to_string(),
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

async fn run(receiver: &Handle, id: &str, work: &Path) {
    receiver_session_commands::run_received_session(receiver.clone(), receiver.state::<AppState>(), id.to_string(), work.display().to_string())
        .await
        .expect("Run");
}

#[tokio::test]
async fn removing_a_running_session_never_leaves_its_containers_running() {
    if !podman_stack_available() {
        eprintln!("podman/podman-compose not on PATH - skipping");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home.path());
    std::env::set_var("XDG_DATA_HOME", home.path().join("data"));
    let base = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let (url, _server) = signaling_url(8175).await;
    let (sender, receiver) = (app(), app());

    // ---- 1. closing the tab of a running session (discard_received_session) ----
    let project = make_project(base.path(), "discard-running", 18141);
    let sid = session_commands::create_project_session(
        sender.state::<AppState>(),
        vec![FolderPlanDto { path: project.display().to_string(), dump: None, compose: None }],
        None,
    )
    .await
    .unwrap()
    .id;
    let info = push(&sender, &receiver, &url, "discardroom1", &sid).await;
    let (title, commit) = (info.manifest.project_name.clone(), info.manifest.git_commit.clone());
    run(&receiver, &info.received_session_id, work.path()).await;
    assert!(ls_containers::project_running(&title, &commit).unwrap(), "precondition: running");
    receiver_session_commands::discard_received_session(receiver.state::<AppState>(), info.received_session_id.clone())
        .await
        .expect("discard");
    assert!(!ls_containers::project_running(&title, &commit).unwrap(), "discarding a running session must stop its containers");
    assert!(!receiver.state::<AppState>().received_sessions.lock().unwrap().contains_key(&info.received_session_id));

    // ---- 2. Home's Close session, after an update landed while the old version ran ----
    let project2 = make_project(base.path(), "close-after-update", 18142);
    let sid2 = session_commands::create_project_session(
        sender.state::<AppState>(),
        vec![FolderPlanDto { path: project2.display().to_string(), dump: None, compose: None }],
        None,
    )
    .await
    .unwrap()
    .id;
    let first = push(&sender, &receiver, &url, "closeroom1", &sid2).await;
    let rid = first.received_session_id.clone();
    let (title2, old_commit) = (first.manifest.project_name.clone(), first.manifest.git_commit.clone());
    run(&receiver, &rid, work.path()).await;
    receiver_session_commands::arm_received_session_for_update(receiver.state::<AppState>(), rid.clone()).unwrap();
    std::fs::write(project2.join("README.md"), "v2\n").unwrap();
    git(&project2, &["commit", "-qam", "two"]);
    let update = push(&sender, &receiver, &url, "closeroom2", &sid2).await;
    assert_eq!(update.received_session_id, rid, "the update landed on the armed session");
    assert!(ls_containers::project_running(&title2, &old_commit).unwrap(), "precondition: the old version still runs");
    session_list_commands::close_session(receiver.state::<AppState>(), rid.clone()).await.expect("close");
    assert!(
        !ls_containers::project_running(&title2, &old_commit).unwrap(),
        "closing must also stop the version that was running when the update landed"
    );
}
