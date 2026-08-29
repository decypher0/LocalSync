//! Proves round 11 goals 1-3 end to end, same-box, three real processes'
//! worth of state (one sender `AppState`, two independent receiver
//! `AppState`s) connected over a real `ls_net::host_ephemeral_relay()`
//! instance (no Node needed — see `remote_relay_mode_test.rs` for why that's
//! a legitimate stand-in, not a shortcut: identical wire protocol to
//! `apps/signaling-server`).
//!
//! - Two receivers connect to the same sender via two independent room
//!   codes; `list_connected_receivers` shows both (goal 1).
//! - A push targeted at receiver 1 (`push_update`) reaches receiver 1 and
//!   *only* receiver 1 - receiver 2 gets nothing (goal 2, the actual point
//!   of "targeted").
//! - Receiver 2 sends a pull request; the sender sees a real `pull-request`
//!   event, accepts it (`respond_to_pull_request`), and receiver 2 receives
//!   the update through the normal diff/consent-gate pipeline - held, not
//!   auto-run (goal 3).
//!
//! Every pushed/pulled update goes through `finalize_received_snapshot` -
//! the exact same function a fresh `receive_snapshot` uses - so this also
//! re-confirms (alongside `known_peer_test.rs`) that nothing about session
//! persistence auto-runs anything: `AppState.sessions` stays empty
//! throughout this whole test.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
            "commit", "-q", "-m", "multi-receiver test snapshot",
        ],
    );
    (dir, project_dir)
}

/// Polls `check` until it returns `true` or `timeout` elapses. Used instead
/// of a fixed sleep for every async event-delivery wait below - real
/// control-channel round trips vary in how long they take, and a fixed
/// sleep would either be flaky (too short) or needlessly slow the test (too
/// long padded for worst case).
async fn wait_until(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if check() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn multiple_receivers_targeted_push_and_pull_request() {
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

    let sender_app = tauri::test::mock_app();
    sender_app.manage(AppState::default());
    let sender = sender_app.handle().clone();

    // Sender-side: collect pull-request notices as they arrive.
    let pull_requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let pull_requests_for_listener = pull_requests.clone();
    sender.listen("pull-request", move |event| {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(event.payload()) {
            if let Some(peer_id) = v.get("peer_id").and_then(|p| p.as_str()) {
                pull_requests_for_listener.lock().unwrap().push(peer_id.to_string());
            }
        }
    });

    // ---- receiver 1 connects ----
    let receiver1_app = tauri::test::mock_app();
    receiver1_app.manage(AppState::default());
    let receiver1 = receiver1_app.handle().clone();
    let room1 = ls_net::generate_room_id();

    let (sender_h, room1c, url1, path1) = (sender.clone(), room1.clone(), signaling_url.clone(), project_path.clone());
    let send1 = tokio::spawn(async move {
        let state = sender_h.state::<AppState>();
        commands::share_snapshot(sender_h.clone(), state, path1, room1c, url1).await
    });
    let (receiver1_h, room1c, url1) = (receiver1.clone(), room1.clone(), signaling_url.clone());
    let recv1 = tokio::spawn(async move {
        let state = receiver1_h.state::<AppState>();
        commands::receive_snapshot(receiver1_h.clone(), state, room1c, url1).await
    });
    send1.await.expect("task panicked").expect("share_snapshot to receiver 1 should succeed");
    let info1 = recv1.await.expect("task panicked").expect("receive_snapshot on receiver 1 should succeed");

    // ---- receiver 2 connects, independently ----
    let receiver2_app = tauri::test::mock_app();
    receiver2_app.manage(AppState::default());
    let receiver2 = receiver2_app.handle().clone();
    let room2 = ls_net::generate_room_id();
    assert_ne!(room1, room2, "two independent share_snapshot calls must not collide on room id");

    let (sender_h, room2c, url2, path2) = (sender.clone(), room2.clone(), signaling_url.clone(), project_path.clone());
    let send2 = tokio::spawn(async move {
        let state = sender_h.state::<AppState>();
        commands::share_snapshot(sender_h.clone(), state, path2, room2c, url2).await
    });
    let (receiver2_h, room2c, url2) = (receiver2.clone(), room2.clone(), signaling_url.clone());
    let recv2 = tokio::spawn(async move {
        let state = receiver2_h.state::<AppState>();
        commands::receive_snapshot(receiver2_h.clone(), state, room2c, url2).await
    });
    send2.await.expect("task panicked").expect("share_snapshot to receiver 2 should succeed");
    let _info2 = recv2.await.expect("task panicked").expect("receive_snapshot on receiver 2 should succeed");

    // ---- goal 1: both receivers show up in the sender's roster ----
    let roster = commands::list_connected_receivers(sender.state::<AppState>())
        .expect("list_connected_receivers should succeed");
    assert_eq!(roster.len(), 2, "both receivers should be tracked at once, got: {roster:?}");
    let roster_ids: Vec<&str> = roster.iter().map(|r| r.peer_id.as_str()).collect();
    assert!(roster_ids.contains(&room1.as_str()));
    assert!(roster_ids.contains(&room2.as_str()));

    // ---- goal 2: push_update targeted at receiver 1 reaches only receiver 1 ----
    let r1_updates: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let r1_updates_l = r1_updates.clone();
    receiver1.listen("snapshot-updated", move |event| {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(event.payload()) {
            if let Some(id) = v.get("snapshot_id").and_then(|s| s.as_str()) {
                r1_updates_l.lock().unwrap().push(id.to_string());
            }
        }
    });
    let r2_updates: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let r2_updates_l = r2_updates.clone();
    receiver2.listen("snapshot-updated", move |event| {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(event.payload()) {
            if let Some(id) = v.get("snapshot_id").and_then(|s| s.as_str()) {
                r2_updates_l.lock().unwrap().push(id.to_string());
            }
        }
    });

    let pushed_id = commands::push_update(sender.clone(), sender.state::<AppState>(), room1.clone())
        .await
        .expect("push_update targeted at receiver 1 should succeed");
    assert_eq!(pushed_id, info1.snapshot_id, "unchanged project resent should carry the same snapshot id");

    let r1_got_it = wait_until(Duration::from_secs(10), || !r1_updates.lock().unwrap().is_empty()).await;
    assert!(r1_got_it, "receiver 1 should have received the targeted push");

    // Real negative assertion, not just "didn't check": wait the same real
    // amount of time a positive case would take, then confirm nothing
    // arrived at receiver 2 - a push targeted at one peer must not reach
    // any other connected receiver.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        r2_updates.lock().unwrap().is_empty(),
        "a push targeted at receiver 1 must not reach receiver 2 - this is what makes it 'targeted' \
         rather than a broadcast"
    );

    // ---- goal 3: receiver 2 pull-requests, sender accepts, receiver 2 gets the update ----
    commands::send_pull_request(receiver2.state::<AppState>())
        .await
        .expect("send_pull_request should succeed");

    let sender_saw_it =
        wait_until(Duration::from_secs(10), || pull_requests.lock().unwrap().contains(&room2)).await;
    assert!(sender_saw_it, "sender should see a real pull-request event naming receiver 2's peer_id");

    let accepted_id = commands::respond_to_pull_request(sender.clone(), sender.state::<AppState>(), room2.clone(), true)
        .await
        .expect("respond_to_pull_request(accept) should succeed")
        .expect("accepting should return the pushed snapshot's id");
    assert_eq!(accepted_id, info1.snapshot_id, "unchanged project resent should still carry the same snapshot id");

    let r2_got_it = wait_until(Duration::from_secs(10), || !r2_updates.lock().unwrap().is_empty()).await;
    assert!(r2_got_it, "receiver 2 should have received the update after the sender accepted its pull request");

    // ---- non-negotiable: nothing here ever auto-ran anything ----
    assert!(
        sender.state::<AppState>().sessions.lock().unwrap().is_empty(),
        "the sender never runs anything at all - run_snapshot only exists receiver-side"
    );
    assert!(
        receiver1.state::<AppState>().sessions.lock().unwrap().is_empty(),
        "a pushed update must be held for review, never auto-run"
    );
    assert!(
        receiver2.state::<AppState>().sessions.lock().unwrap().is_empty(),
        "a pull-accepted update must be held for review, never auto-run"
    );
    // And each held update really is sitting there waiting for an explicit
    // Run, under the id the push/accept call itself returned.
    assert!(receiver1.state::<AppState>().verified.lock().unwrap().contains_key(&pushed_id));
    assert!(receiver2.state::<AppState>().verified.lock().unwrap().contains_key(&accepted_id));
}
