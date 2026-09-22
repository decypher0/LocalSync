//! Goal 4 (this round): proves the plumbing for the new "receiver-connecting"
//! event end to end at the real command layer - `commands::share_snapshot`
//! emits it (with the right `session_id`) right after `ls_net::connect_as_sender`
//! opens the data channel, and strictly *before* any `share-progress` event
//! (which only starts once `send_payload` begins moving bytes). Before this
//! round, there was no signal at all between "a receiver joined" and "the
//! transfer finished" (`state.connected_receivers`, what
//! `list_connected_receivers` reads, was only populated after `send_payload`
//! returned) - this is the gap the new event closes.
//!
//! Same same-box pattern `multi_receiver_session_test.rs` uses: a real
//! `ls_net::host_ephemeral_relay()` (no Node dependency) plus the real
//! `commands::share_snapshot`/`commands::receive_snapshot` command-layer
//! functions, driven through `tauri::test::mock_app()`.
//!
//! What this test does NOT (and per this project's own established
//! convention, cannot) prove: what a human actually sees on screen. The
//! frontend rendering (`apps/desktop/src/app.js`'s `send-receiver-status-wrap`,
//! wired up in `renderSendSessionDetail`/`performSendAttempt`) needs a real
//! running app to verify visually.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use localsync_desktop::{commands, state::AppState};
use tauri::{Listener, Manager};

fn sh(dir: &Path, cmd: &str, args: &[&str]) {
    let status = Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {cmd} {args:?}: {e}"));
    assert!(status.success(), "{cmd} {args:?} failed in {}", dir.display());
}

fn make_git_project_from(source: &Path, name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let project_dir = dir.path().join(name);
    std::fs::create_dir(&project_dir).unwrap();
    sh(
        Path::new("."),
        "cp",
        &["-r", &format!("{}/.", source.display()), &project_dir.display().to_string()],
    );
    sh(&project_dir, "git", &["init", "-q"]);
    sh(&project_dir, "git", &["add", "-A"]);
    sh(
        &project_dir,
        "git",
        &[
            "-c", "user.name=Test", "-c", "user.email=test@example.com",
            "commit", "-q", "-m", "receiver-connecting event test snapshot",
        ],
    );
    (dir, project_dir)
}

#[tokio::test]
async fn receiver_connecting_fires_with_right_session_id_before_any_transfer_progress() {
    let sample_project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../sample-project");
    let sender_home = tempfile::tempdir().unwrap();
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::set_var(home_var, sender_home.path());

    let (_tempdir, project_dir) = make_git_project_from(&sample_project, "sample-project");
    let project_path = project_dir.display().to_string();

    let (relay_port, _relay_task) = ls_net::host_ephemeral_relay()
        .await
        .expect("failed to start embedded relay");
    let signaling_url = format!("ws://127.0.0.1:{relay_port}");
    let room = ls_net::generate_room_id();

    let sender_app = tauri::test::mock_app();
    sender_app.manage(AppState::default());
    let sender = sender_app.handle().clone();

    // Records event names in arrival order, tagged with whatever session_id
    // each one actually carried - the ordering (not just presence) is the
    // point: "receiver-connecting" must arrive before the first
    // "share-progress", proving it really does signal a joined receiver
    // ahead of transfer completion rather than alongside/after it.
    let arrival_order: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

    let order_for_rc = arrival_order.clone();
    sender.listen("receiver-connecting", move |event| {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(event.payload()) {
            if let Some(session_id) = v.get("session_id").and_then(|s| s.as_str()) {
                order_for_rc.lock().unwrap().push(("receiver-connecting".to_string(), session_id.to_string()));
            }
        }
    });

    let order_for_progress = arrival_order.clone();
    sender.listen("share-progress", move |event| {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(event.payload()) {
            if let Some(session_id) = v.get("session_id").and_then(|s| s.as_str()) {
                order_for_progress.lock().unwrap().push(("share-progress".to_string(), session_id.to_string()));
            }
        }
    });

    let receiver_app = tauri::test::mock_app();
    receiver_app.manage(AppState::default());
    let receiver = receiver_app.handle().clone();

    let (sender_h, room_c, url_c, path_c) = (sender.clone(), room.clone(), signaling_url.clone(), project_path.clone());
    let send_task = tokio::spawn(async move {
        let state = sender_h.state::<AppState>();
        commands::share_snapshot(sender_h.clone(), state, path_c, room_c, url_c).await
    });
    let (receiver_h, room_c, url_c) = (receiver.clone(), room.clone(), signaling_url.clone());
    let recv_task = tokio::spawn(async move {
        let state = receiver_h.state::<AppState>();
        commands::receive_snapshot(receiver_h.clone(), state, room_c, url_c).await
    });

    send_task.await.expect("sender task panicked").expect("share_snapshot should succeed");
    recv_task.await.expect("receiver task panicked").expect("receive_snapshot should succeed");

    let order = arrival_order.lock().unwrap();
    assert!(
        !order.is_empty(),
        "expected at least one recorded event, got none - the listeners never fired"
    );

    let first_rc_index = order.iter().position(|(name, _)| name == "receiver-connecting");
    assert!(
        first_rc_index.is_some(),
        "expected a real receiver-connecting event, got only: {order:?}"
    );
    let first_rc_index = first_rc_index.unwrap();
    assert_eq!(
        order[first_rc_index].1, room,
        "receiver-connecting's session_id should be the room code, matching what share-progress/list_connected_receivers use"
    );

    let first_progress_index = order.iter().position(|(name, _)| name == "share-progress");
    if let Some(first_progress_index) = first_progress_index {
        assert!(
            first_rc_index < first_progress_index,
            "receiver-connecting should fire strictly before the first share-progress event \
             (a real peer connecting is a distinct, earlier moment than transfer progress \
             starting to move) - got order: {order:?}"
        );
    }
    // (No share-progress event at all is also consistent with the fix: a
    // small enough snapshot can complete in one send_payload chunk whose
    // single progress callback still fires after receiver-connecting - the
    // ordering assertion above already covers that case when it does fire.)
}
