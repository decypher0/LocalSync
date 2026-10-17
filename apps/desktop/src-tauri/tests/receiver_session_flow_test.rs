//! The receiver-side session model, end to end over real transfers (same
//! box), mirroring `project_session_flow_test.rs`'s style for the sending
//! half:
//!
//! - receiving a project creates a `ReceivedSession` - a receiver-side
//!   workspace, not just a held snapshot;
//! - running it brings containers up (`ls_containers::run_snapshot`'s normal
//!   path);
//! - re-running it **after simulating an app restart** (no `VerifiedSnapshot`
//!   left in memory) still works, via `ls_containers::run_existing` against
//!   what a prior run already unpacked to disk;
//! - a push of the same project from the same sender lands on the *same*
//!   session as an update - automatically, nothing to arm - and is applied
//!   there, while a different project from that sender still creates a
//!   session of its own;
//! - saving is opt-in, and a saved session's `compose_dir` survives a
//!   simulated restart (fresh `AppState`) so it can be re-run from disk.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use localsync_desktop::commands::{self, FolderPlanDto};
use localsync_desktop::receiver_session_commands;
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

fn make_project(base: &Path, name: &str) -> PathBuf {
    let root = base.join(name);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("README.md"), "v1\n").unwrap();
    std::fs::write(
        root.join("docker-compose.yml"),
        "services:\n  web:\n    image: docker.io/library/python:3.12-slim\n    command: [\"python\", \"-m\", \"http.server\", \"80\"]\n    ports:\n      - \"8099:80\"\n",
    )
    .unwrap();
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
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (format!("ws://127.0.0.1:{port}"), Some(child))
}

/// Same check `run_retry_test.rs`/`run_progress_test.rs` already use to
/// decide whether their own container-dependent assertions can run at all -
/// binary availability only. Whether `podman-compose up` can actually
/// *succeed* in a given sandbox (networking, cgroups, ...) is a separate,
/// environment-specific question those same existing tests don't shield
/// against either.
fn podman_stack_available() -> bool {
    let ok = |bin: &str| Command::new(bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    ok("podman") && ok("podman-compose")
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

/// One send from `sender`'s project session to `receiver`'s (persistent,
/// shared across calls - unlike `project_session_flow_test.rs`'s per-send
/// fresh receiver) `AppState`, over a real embedded relay + WebRTC data
/// channel.
async fn send_and_receive(
    sender: &tauri::AppHandle<tauri::test::MockRuntime>,
    receiver: &tauri::AppHandle<tauri::test::MockRuntime>,
    url: &str,
    room: &str,
    mut request: SendRequest,
) -> (session_commands::SendResult, commands::IncomingSnapshotInfo) {
    request.room_code = room.to_string();
    request.signaling_url = url.to_string();

    let receiver_handle = receiver.clone();
    let (r_room, r_url) = (room.to_string(), url.to_string());
    let recv_task = tokio::spawn(async move {
        let state = receiver_handle.state::<AppState>();
        commands::receive_snapshot(receiver_handle.clone(), state, r_room, r_url).await
    });
    let sent = session_commands::send_project_session(sender.clone(), sender.state::<AppState>(), request)
        .await
        .expect("send_project_session should succeed");
    let info = recv_task.await.unwrap().expect("receive_snapshot should succeed");
    (sent, info)
}

#[tokio::test]
async fn receiver_session_model_end_to_end() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home.path());
    std::env::set_var("XDG_DATA_HOME", home.path().join("data"));
    let base = tempfile::tempdir().unwrap();
    let project = make_project(base.path(), "receiver-session-project");
    let (url, _server) = signaling_url(8161).await;

    let sender_app = tauri::test::mock_app();
    sender_app.manage(AppState::default());
    let sender = sender_app.handle().clone();

    let receiver_app = tauri::test::mock_app();
    receiver_app.manage(AppState::default());
    let receiver = receiver_app.handle().clone();

    let folders = vec![FolderPlanDto { path: project.display().to_string(), dump: None, compose: None }];
    let view = session_commands::create_project_session(sender.state::<AppState>(), folders, None).await.unwrap();
    let sid = view.id.clone();

    // ---- 1. receiving a project creates a ReceivedSession ----
    let (_sent1, info1) = send_and_receive(&sender, &receiver, &url, "roomA", request(&sid, None, "Alice", false)).await;
    let received_id = {
        let state = receiver.state::<AppState>();
        let sessions = state.received_sessions.lock().unwrap();
        assert_eq!(sessions.len(), 1, "receiving a project must create exactly one ReceivedSession");
        let (id, session) = sessions.iter().next().unwrap();
        assert_eq!(session.snapshot_id, info1.snapshot_id);
        assert_eq!(session.sender_pubkey_hex, info1.sender_pubkey_hex);
        assert_eq!(session.title, "receiver-session-project");
        assert_eq!(session.compose_dir, "", "nothing has been run yet");
        id.clone()
    };

    // ---- 2 & 3: run, then simulate an app restart, then run again ----
    if podman_stack_available() {
        let work_dir = tempfile::tempdir().unwrap();
        let run1 = receiver_session_commands::run_received_session(
            receiver.clone(),
            receiver.state::<AppState>(),
            received_id.clone(),
            work_dir.path().display().to_string(),
        )
        .await
        .expect("first run should succeed via the run_snapshot path (VerifiedSnapshot still held)");
        assert!(run1.running.service_ports.iter().any(|(svc, port)| svc == "web" && port == "8099:80"));
        commands::stop_session(receiver.state::<AppState>(), run1.running.session_id.clone())
            .await
            .expect("stop_session should tear the first run down");

        // The actual point of this round: a successful run already consumed
        // (removed) the held VerifiedSnapshot from state.verified - exactly
        // like commands::run_snapshot does for a plain receive - so by this
        // point the "nothing left in memory" condition a real app restart
        // would produce already holds, with no need to fake it further.
        assert!(
            !receiver.state::<AppState>().verified.lock().unwrap().contains_key(&info1.snapshot_id),
            "a successful run must consume the held snapshot, same as commands::run_snapshot"
        );

        let run2 = receiver_session_commands::run_received_session(
            receiver.clone(),
            receiver.state::<AppState>(),
            received_id.clone(),
            work_dir.path().display().to_string(),
        )
        .await
        .expect("second run should succeed via run_existing with zero connection and no held VerifiedSnapshot");
        assert_eq!(
            run2.running.session_id, run1.running.session_id,
            "the same session's title+commit must map to the same compose project"
        );
        commands::stop_session(receiver.state::<AppState>(), run2.running.session_id.clone())
            .await
            .expect("stop_session should tear the re-run down");
    } else {
        eprintln!("skipping container run/restart assertions: podman/podman-compose not on PATH");
    }

    // ---- 4. the same project pushed again lands on the existing session ----
    let ran = podman_stack_available();
    commit_change(&project, "v2\n");
    let (_sent2, info2) = send_and_receive(&sender, &receiver, &url, "roomB", request(&sid, None, "Alice", false)).await;
    assert_ne!(info2.snapshot_id, info1.snapshot_id, "the project actually moved, so this is a genuinely new push");
    assert_eq!(info2.received_session_id, received_id, "an update to a project this device has lands on its session");
    {
        let state = receiver.state::<AppState>();
        let sessions = state.received_sessions.lock().unwrap();
        assert_eq!(sessions.len(), 1, "an update must not create a second session");
        let session = sessions.get(&received_id).unwrap();
        assert_eq!(session.snapshot_id, info2.snapshot_id, "the existing session's version moved to the new push");
        assert_eq!(session.git_commit, info2.manifest.git_commit);
        assert_eq!(session.pending_update.is_some(), ran, "a session that ran waits for the update to be applied");
    }
    if ran {
        let applied = receiver_session_commands::apply_received_update(
            receiver.clone(),
            receiver.state::<AppState>(),
            received_id.clone(),
            None,
        )
        .await
        .expect("applying the update runs the new version");
        commands::stop_session(receiver.state::<AppState>(), applied.running.session_id.clone())
            .await
            .expect("stop_session should tear the applied update down");
        let state = receiver.state::<AppState>();
        let session = state.received_sessions.lock().unwrap().get(&received_id).cloned().unwrap();
        assert!(session.pending_update.is_none(), "an applied update is the session's version now");
        assert!(session.compose_dir.contains(&info2.manifest.git_commit), "runs from the new version's folder: {}", session.compose_dir);
    }

    // ---- a different project from the same sender is a session of its own ----
    let other = make_project(base.path(), "receiver-session-other");
    let other_folders = vec![FolderPlanDto { path: other.display().to_string(), dump: None, compose: None }];
    let other_sid = session_commands::create_project_session(sender.state::<AppState>(), other_folders, None).await.unwrap().id;
    let (_sent3, info3) = send_and_receive(&sender, &receiver, &url, "roomC", request(&other_sid, None, "Alice", false)).await;
    assert_ne!(info3.received_session_id, received_id, "a different project is not an update");
    assert_eq!(receiver.state::<AppState>().received_sessions.lock().unwrap().len(), 2);

    // ---- 5. save -> simulate a full restart (fresh AppState) -> reopen ----
    receiver_session_commands::save_received_session(receiver.state::<AppState>(), received_id.clone())
        .expect("saving the updated session should succeed");
    let saved = localsync_desktop::session_history::find_received(&received_id)
        .unwrap()
        .expect("the session should now be saved");
    assert_eq!(saved.id, received_id);
    assert_eq!(saved.snapshot_id, info2.snapshot_id);

    let fresh_app = tauri::test::mock_app();
    fresh_app.manage(AppState::default());
    let fresh = fresh_app.handle().clone();
    let reopened =
        receiver_session_commands::open_saved_received_session(fresh.state::<AppState>(), received_id.clone())
            .expect("reopening a saved received session should succeed against a fresh AppState");
    assert_eq!(reopened.id, received_id);
    assert!(reopened.saved);
    assert_eq!(reopened.snapshot_id, saved.snapshot_id);

    if podman_stack_available() && !saved.compose_dir.is_empty() {
        // The compose_dir a prior successful run wrote survived the save,
        // and a fresh AppState (simulating a real restart) can run from it
        // with zero connection involved and no VerifiedSnapshot anywhere.
        let run3 = receiver_session_commands::run_received_session(
            fresh.clone(),
            fresh.state::<AppState>(),
            received_id.clone(),
            saved.work_dir.clone(),
        )
        .await
        .expect("a reopened saved session should run via run_existing against its saved compose_dir");
        commands::stop_session(fresh.state::<AppState>(), run3.running.session_id.clone())
            .await
            .expect("stop_session should tear down the reopened session's run");
    } else {
        // No prior run ever completed for this session (podman unavailable
        // for the earlier steps), so compose_dir is still empty - running it
        // must fail with a clear, actionable message, not panic or hang.
        let result = receiver_session_commands::run_received_session(
            fresh.clone(),
            fresh.state::<AppState>(),
            received_id.clone(),
            "/tmp/unused-workdir".to_string(),
        )
        .await;
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("running a session with no held snapshot and no compose_dir must fail clearly"),
        };
        assert!(err.contains("receive it again"), "{err}");
    }

    receiver_session_commands::delete_saved_received_session(received_id.clone()).unwrap();
    assert!(localsync_desktop::session_history::find_received(&received_id).unwrap().is_none());
}
