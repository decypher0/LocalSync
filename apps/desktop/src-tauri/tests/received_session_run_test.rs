//! A fresh (non-update) receive followed immediately by Run, using only what
//! the UI gets back from the receive command - the exact case that broke:
//! the UI keyed the Run on `snapshot_id` (or the room code), while
//! `run_received_session` looks sessions up by the `ReceivedSession` id,
//! so Run failed with "no open received session with id ...".
//!
//! Both receive paths, over a real relay + WebRTC data channel:
//! - room code: `commands::receive_snapshot`
//! - discovery: the receiver's standing room gets a `ConnectionRequest`
//!   (what `set_discoverable`'s listener parks in `pending_connection_requests`)
//!   and the person accepts via `commands::respond_to_connection_request`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use localsync_desktop::commands::{self, FolderPlanDto, IncomingSnapshotInfo};
use localsync_desktop::receiver_session_commands;
use localsync_desktop::session_commands::{self, SendRequest};
use localsync_desktop::state::{AppState, PendingConnectionRequest};
use tauri::Manager;

type Handle = tauri::AppHandle<tauri::test::MockRuntime>;

fn sh(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn make_project(base: &Path, name: &str, port: u16) -> PathBuf {
    let root = base.join(name);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("README.md"), "v1\n").unwrap();
    std::fs::write(
        root.join("docker-compose.yml"),
        format!("services:\n  web:\n    image: docker.io/library/nginx:alpine\n    ports:\n      - \"{port}:80\"\n"),
    )
    .unwrap();
    sh(&root, &["init", "-q"]);
    sh(&root, &["add", "-A"]);
    sh(&root, &["commit", "-q", "-m", "one"]);
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

fn podman_stack_available() -> bool {
    let ok = |bin: &str| Command::new(bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    ok("podman") && ok("podman-compose")
}

fn app() -> Handle {
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    app.handle().clone()
}

async fn sender_session(sender: &Handle, project: &Path) -> String {
    let folders = vec![FolderPlanDto { path: project.display().to_string(), dump: None, compose: None }];
    session_commands::create_project_session(sender.state::<AppState>(), folders, None).await.unwrap().id
}

fn request(session_id: &str, room: &str, url: &str, require_accept: bool) -> SendRequest {
    SendRequest {
        session_id: session_id.to_string(),
        room_code: room.to_string(),
        signaling_url: url.to_string(),
        require_accept,
        sender_name: "Alice".to_string(),
        device_key: None,
        device_name: "Bob".to_string(),
        since_last: false,
    }
}

/// What the UI does right after a receive: Run, keyed by the id the receive
/// returned. Also proves the old key (`snapshot_id`) is not a session id.
async fn run_like_the_ui(receiver: &Handle, info: &IncomingSnapshotInfo, port: u16) {
    assert!(!info.received_session_id.is_empty(), "the receive must report its ReceivedSession id");
    assert_ne!(info.received_session_id, info.snapshot_id, "different values - which is why keying by snapshot_id broke");
    assert!(
        receiver.state::<AppState>().received_sessions.lock().unwrap().contains_key(&info.received_session_id),
        "received_session_id must name the session the receive created"
    );

    let wrong = receiver_session_commands::run_received_session(
        receiver.clone(),
        receiver.state::<AppState>(),
        info.snapshot_id.clone(),
        std::env::temp_dir().display().to_string(),
    )
    .await;
    assert!(
        matches!(&wrong, Err(e) if e.contains("no open received session")),
        "keying Run by snapshot_id is the bug: {:?}",
        wrong.as_ref().err()
    );

    if !podman_stack_available() {
        eprintln!("podman/podman-compose not on PATH - skipping the Run itself");
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let run = receiver_session_commands::run_received_session(
        receiver.clone(),
        receiver.state::<AppState>(),
        info.received_session_id.clone(),
        work.path().display().to_string(),
    )
    .await
    .expect("Run right after a fresh receive, keyed by received_session_id, must succeed");
    assert!(run.running.service_ports.iter().any(|(svc, p)| svc == "web" && p == &format!("{port}:80")), "{:?}", run.running.service_ports);
    commands::stop_session(receiver.state::<AppState>(), run.running.session_id.clone()).await.unwrap();
}

/// One home for every test in this file: they run in parallel, and each
/// pointing HOME somewhere new mid-run made them race writing the sender
/// identity ("identity key file ... is corrupt").
fn isolate_home() -> &'static Path {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let home = HOME.get_or_init(|| {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home.path());
        std::env::set_var("XDG_DATA_HOME", home.path().join("data"));
        home
    });
    home.path()
}

#[tokio::test]
async fn a_fresh_room_code_receive_can_be_run_straight_away() {
    let _home = isolate_home();
    let base = tempfile::tempdir().unwrap();
    let project = make_project(base.path(), "fresh-room-code-project", 8095);
    let (url, _server) = signaling_url(8171).await;
    let (sender, receiver) = (app(), app());
    let sid = sender_session(&sender, &project).await;

    let r = receiver.clone();
    let r_url = url.clone();
    let recv = tokio::spawn(async move {
        commands::receive_snapshot(r.clone(), r.state::<AppState>(), "freshroom".to_string(), r_url).await
    });
    session_commands::send_project_session(sender.clone(), sender.state::<AppState>(), request(&sid, "freshroom", &url, false))
        .await
        .expect("send");
    let info = recv.await.unwrap().expect("receive_snapshot");

    run_like_the_ui(&receiver, &info, 8095).await;
}

#[tokio::test]
async fn a_fresh_discovery_accept_can_be_run_straight_away() {
    let _home = isolate_home();
    let base = tempfile::tempdir().unwrap();
    let project = make_project(base.path(), "fresh-discovery-project", 8096);
    let (url, _server) = signaling_url(8172).await;
    let (sender, receiver) = (app(), app());
    let sid = sender_session(&sender, &project).await;
    let room = "standingroom".to_string();

    // The receiver's side, as set_discoverable's listener does it: wait in the
    // standing room, take the ConnectionRequest, park it as pending - then the
    // person clicks Accept.
    let r = receiver.clone();
    let (r_url, r_room) = (url.clone(), room.clone());
    let recv = tokio::spawn(async move {
        let conn = ls_net::connect_as_receiver(&r_url, &r_room).await.expect("receiver joins its room");
        let sender_name = match ls_net::recv_control(&conn).await.expect("a control message") {
            ls_net::ControlMessage::ConnectionRequest { sender_name } => sender_name,
            other => panic!("expected a ConnectionRequest, got {other:?}"),
        };
        r.state::<AppState>()
            .pending_connection_requests
            .lock()
            .unwrap()
            .insert(r_room.clone(), PendingConnectionRequest { conn: std::sync::Arc::new(conn), sender_name });
        commands::respond_to_connection_request(r.clone(), r.state::<AppState>(), r_room, true).await
    });
    session_commands::send_project_session(sender.clone(), sender.state::<AppState>(), request(&sid, &room, &url, true))
        .await
        .expect("send with require_accept");
    let info = recv.await.unwrap().expect("respond_to_connection_request").expect("accepted -> info");

    run_like_the_ui(&receiver, &info, 8096).await;
}
