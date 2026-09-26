//! The project-session model, end to end over real transfers (same box):
//!
//! - a session is created **once** for a project and its artifact is reused
//!   for later devices and for retries (no rebuild);
//! - every transfer is **ephemeral** - nothing is left registered as an open
//!   connection afterwards;
//! - each device has its **own marker**: a push to device A diffs against
//!   what *A* last received, not against a shared baseline, and moves only
//!   A's marker;
//! - a device that is already current is not sent to at all;
//! - saving is **opt-in**: a discarded, never-saved session leaves nothing
//!   behind, a saved one comes back whole.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::Duration;

use localsync_desktop::commands::{self, FolderPlanDto};
use localsync_desktop::session_commands::{self, SendRequest};
use localsync_desktop::state::AppState;
use tauri::Manager;

fn sh(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn make_project(base: &Path) -> PathBuf {
    let root = base.join("session-project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("README.md"), "v1\n").unwrap();
    std::fs::write(root.join("docker-compose.yml"), "services: {}\n").unwrap();
    sh(&root, &["init", "-q"]);
    sh(&root, &["add", "-A"]);
    sh(&root, &["commit", "-q", "-m", "one"]);
    root
}

fn commit_change(root: &Path, text: &str) {
    std::fs::write(root.join("README.md"), text).unwrap();
    sh(root, &["commit", "-q", "-am", text.trim()]);
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
    // Poll until it accepts connections rather than guessing a fixed delay.
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (format!("ws://127.0.0.1:{port}"), Some(child))
}

/// One send to a fresh receiver app; returns what that receiver was handed.
async fn send_to_receiver(
    sender: &tauri::AppHandle<tauri::test::MockRuntime>,
    url: &str,
    room: &str,
    mut request: SendRequest,
) -> (session_commands::SendResult, commands::IncomingSnapshotInfo) {
    let receiver_app = tauri::test::mock_app();
    receiver_app.manage(AppState::default());
    let receiver_handle = receiver_app.handle().clone();

    request.room_code = room.to_string();
    request.signaling_url = url.to_string();

    let (r_room, r_url) = (room.to_string(), url.to_string());
    let receiver = tokio::spawn(async move {
        let state = receiver_handle.state::<AppState>();
        commands::receive_snapshot(receiver_handle.clone(), state, r_room, r_url).await
    });
    let state = sender.state::<AppState>();
    let sent = session_commands::send_project_session(sender.clone(), state, request)
        .await
        .expect("send_project_session should succeed");
    let info = receiver.await.unwrap().expect("receive_snapshot should succeed");
    (sent, info)
}

fn request(session_id: &str, device_key: Option<&str>, name: &str, since_last: bool) -> SendRequest {
    SendRequest {
        session_id: session_id.to_string(),
        room_code: String::new(),
        signaling_url: String::new(),
        require_accept: false,
        sender_name: String::new(),
        device_key: device_key.map(str::to_string),
        device_name: name.to_string(),
        since_last,
    }
}

#[tokio::test]
async fn a_session_is_created_once_reused_across_devices_and_retries_and_tracks_each_device_separately() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home.path());
    std::env::set_var("XDG_DATA_HOME", home.path().join("data"));
    let base = tempfile::tempdir().unwrap();
    let project = make_project(base.path());
    let (url, _server) = signaling_url(8141).await;

    let sender_app = tauri::test::mock_app();
    sender_app.manage(AppState::default());
    let sender = sender_app.handle().clone();
    let builds = || sender.state::<AppState>().artifact_builds.load(Ordering::SeqCst);

    // ---- create once ----
    let folders = vec![FolderPlanDto { path: project.display().to_string(), dump: None }];
    let view = session_commands::create_project_session(sender.state::<AppState>(), folders, None).await.unwrap();
    let sid = view.id.clone();
    assert_eq!(view.title, "session-project");
    assert_eq!(builds(), 1, "creating the session builds the artifact exactly once");
    let commit1 = view.artifact.as_ref().unwrap().commits[0].commit.clone();

    // ---- a failed attempt, then a retry: the artifact is reused ----
    let dead = session_commands::send_project_session(
        sender.clone(),
        sender.state::<AppState>(),
        SendRequest { room_code: "nobody".into(), signaling_url: "ws://127.0.0.1:1".into(), ..request(&sid, None, "Ghost", false) },
    )
    .await;
    assert!(dead.is_err(), "sending to an unreachable relay must fail");
    let (sent_a, info_a) = send_to_receiver(&sender, &url, "roomA1", request(&sid, None, "Alice", false)).await;
    assert_eq!(builds(), 1, "a failed attempt then a retry must not rebuild the project");
    let key_a = sent_a.device_key.clone();
    assert_eq!(info_a.manifest.git_commit, commit1);
    assert!(info_a.manifest.git_parent_commit.is_none(), "a first send has nothing to diff against");

    // ---- a second device: same session, same artifact, its own record ----
    let (sent_b, _info_b) = send_to_receiver(&sender, &url, "roomB1", request(&sid, None, "Bob", false)).await;
    assert_eq!(builds(), 1, "a second device reuses the session's artifact");
    let key_b = sent_b.device_key.clone();
    assert_ne!(key_a, key_b);
    assert_eq!(sent_b.view.devices.len(), 2, "one session, two devices - not two sessions");

    // ---- ephemeral: nothing was left registered as an open connection ----
    {
        let state = sender.state::<AppState>();
        assert!(state.connected_receivers.lock().unwrap().is_empty(), "no receiver may stay connected");
        assert!(state.outgoing_conn.lock().unwrap().is_none());
        assert_eq!(state.project_sessions.lock().unwrap().len(), 1);
    }

    // ---- the project moves on; a refresh notices and rebuilds once ----
    commit_change(&project, "v2\n");
    let refreshed = session_commands::refresh_project_session(sender.state::<AppState>(), sid.clone()).await.unwrap();
    assert_eq!(builds(), 2);
    let commit2 = refreshed.artifact.as_ref().unwrap().commits[0].commit.clone();
    assert_ne!(commit1, commit2);
    assert!(refreshed.devices.iter().all(|d| !d.up_to_date), "both devices are now behind");

    // ---- push to A: diffed against A's own marker (commit1), only A moves ----
    let (pushed_a, info_push_a) =
        send_to_receiver(&sender, &url, "roomA2", request(&sid, Some(&key_a), "Alice", true)).await;
    assert!(!pushed_a.up_to_date);
    assert_eq!(info_push_a.manifest.git_commit, commit2);
    assert_eq!(
        info_push_a.manifest.git_parent_commit.as_deref(),
        Some(commit1.as_str()),
        "a push must diff against what that device last received"
    );
    assert!(info_push_a.diff.entries.iter().any(|e| e.path.ends_with("README.md")), "the reviewed diff shows the change");
    let marker = |v: &session_commands::ProjectSessionView, k: &str| {
        v.devices.iter().find(|d| d.key == k).unwrap().marker.as_ref().unwrap().commits[0].commit.clone()
    };
    assert_eq!(marker(&pushed_a.view, &key_a), commit2, "A's marker moved");
    assert_eq!(marker(&pushed_a.view, &key_b), commit1, "B's marker did not");

    // ---- B is still behind and gets its own diff point (commit1), not A's ----
    commit_change(&project, "v3\n");
    let (_pushed_b, info_push_b) =
        send_to_receiver(&sender, &url, "roomB2", request(&sid, Some(&key_b), "Bob", true)).await;
    let commit3 = info_push_b.manifest.git_commit.clone();
    assert_ne!(commit3, commit2);
    assert_eq!(info_push_b.manifest.git_parent_commit.as_deref(), Some(commit1.as_str()));

    // ---- a device that is already current is not connected to at all ----
    // (the relay is deliberately unreachable: if this tried to connect it
    // would fail, so success proves no connection was attempted)
    let none = session_commands::send_project_session(
        sender.clone(),
        sender.state::<AppState>(),
        SendRequest { room_code: "unused".into(), signaling_url: "ws://127.0.0.1:1".into(), ..request(&sid, Some(&key_b), "Bob", true) },
    )
    .await
    .expect("an up-to-date device needs no connection at all");
    assert!(none.up_to_date);
    assert!(!none.view.devices.iter().find(|d| d.key == key_a).unwrap().up_to_date, "A is still behind");

    // ---- save is opt-in: a never-saved session leaves nothing behind ----
    assert!(localsync_desktop::session_history::find_project(&sid).unwrap().is_none(), "nothing saved yet");
    session_commands::discard_project_session(sender.state::<AppState>(), sid.clone()).unwrap();
    {
        let state = sender.state::<AppState>();
        assert!(state.project_sessions.lock().unwrap().is_empty());
        assert!(state.artifacts.lock().unwrap().is_empty(), "a discarded session's artifacts go with it");
    }
    let gone = session_commands::open_saved_project_session(sender.state::<AppState>(), sid.clone()).await;
    assert!(gone.is_err(), "a discarded, never-saved session cannot be reopened");

    // ---- ...and a saved one comes back whole, with its per-device markers ----
    let folders = vec![FolderPlanDto { path: project.display().to_string(), dump: None }];
    let second = session_commands::create_project_session(sender.state::<AppState>(), folders, Some("My project".into())).await.unwrap();
    let sid2 = second.id.clone();
    let (sent_c, _) = send_to_receiver(&sender, &url, "roomC1", request(&sid2, None, "Carol", false)).await;
    let key_c = sent_c.device_key.clone();
    session_commands::save_project_session(sender.state::<AppState>(), sid2.clone()).unwrap();

    // A saved session stays current on disk as it is used (they opted in)...
    let (sent_d, _) = send_to_receiver(&sender, &url, "roomD1", request(&sid2, None, "Dave", false)).await;
    let saved = localsync_desktop::session_history::find_project(&sid2).unwrap().unwrap();
    assert_eq!(saved.devices.len(), 2);

    // ...and after closing it (as at app exit) it reopens with its history.
    session_commands::discard_project_session(sender.state::<AppState>(), sid2.clone()).unwrap();
    let reopened = session_commands::open_saved_project_session(sender.state::<AppState>(), sid2.clone()).await.unwrap();
    assert_eq!(reopened.title, "My project");
    assert_eq!(reopened.devices.len(), 2);
    assert!(reopened.devices.iter().any(|d| d.key == key_c && d.marker.is_some()));
    assert!(reopened.devices.iter().any(|d| d.key == sent_d.device_key));
    assert!(reopened.saved);
    assert!(reopened.artifact.is_some(), "reopening rebuilds the artifact from the project on disk");

    session_commands::delete_saved_project_session(sid2.clone()).unwrap();
    assert!(localsync_desktop::session_history::find_project(&sid2).unwrap().is_none());
}
