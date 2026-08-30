//! Round 12: proves the consent gate holds on the specific path this round's
//! Send-tab UI actually promotes - "Previously connected" -> Push update,
//! reusing round 11's still-open connection rather than a fresh room code
//! - when the receiver has *also* explicitly remembered the sender as a
//! known peer (round 10's `remember_peer`, what the review screen's "Save"
//! button calls). Neither "the connection didn't need a fresh room code"
//! nor "the sender is a recognized peer" may add up to anything running
//! without an explicit Run: both facts are purely informational to the UI,
//! never a shortcut past it.
//!
//! Composes round 10 and round 11's own proofs into the one path round 12
//! actually built a UI entry point for, which neither prior test exercised
//! on its own: `known_peer_test.rs` never reconnects an existing
//! connection, and `multi_receiver_session_test.rs` never remembers the
//! sender as a peer first.

use std::path::Path;
use std::process::Command;
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
            "commit", "-q", "-m", "reconnect consent gate test snapshot",
        ],
    );
    (dir, project_dir)
}

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
async fn reconnect_to_a_remembered_peer_still_only_holds_never_runs() {
    let sample_project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../sample-project");
    let sender_home = tempfile::tempdir().unwrap();
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::set_var(home_var, sender_home.path());

    let receiver_data_dir = tempfile::tempdir().unwrap();
    let data_dir_var = if cfg!(windows) { "APPDATA" } else { "XDG_DATA_HOME" };
    std::env::set_var(data_dir_var, receiver_data_dir.path());

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
    let receiver_app = tauri::test::mock_app();
    receiver_app.manage(AppState::default());
    let receiver = receiver_app.handle().clone();

    // ---- initial connect: the normal room-code flow, once ----
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
    send_task.await.expect("task panicked").expect("initial share_snapshot should succeed");
    let first_info = recv_task.await.expect("task panicked").expect("initial receive_snapshot should succeed");

    // First time seeing this sender: not recognized yet - nobody's called
    // remember_peer, exactly like a real first-ever receive.
    assert!(first_info.recognized_peer.is_none(), "a first-ever receive must not already show as recognized");

    // ---- the receiver does what the "Save" button on the review screen
    // does: explicitly remembers this sender, under a name, as round 10
    // intends - this must be a deliberate user action, never automatic. ----
    commands::remember_peer(first_info.sender_pubkey_hex.clone(), "Trusted Laptop".to_string())
        .expect("remember_peer should succeed");

    // ---- this is round 12's actual UI entry point: "Previously connected"
    // -> Push update, reusing the still-open connection from round 11's
    // roster - no fresh room code, no re-typing anything. ----
    let roster = commands::list_connected_receivers(sender.state::<AppState>())
        .expect("list_connected_receivers should succeed");
    assert_eq!(roster.len(), 1, "the receiver should still be in the sender's roster after the initial send");
    assert_eq!(roster[0].peer_id, room, "the roster entry's peer_id is the room code the connection was made under");

    let update_events: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let update_events_l = update_events.clone();
    use tauri::Listener;
    receiver.listen("snapshot-updated", move |event| {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(event.payload()) {
            update_events_l.lock().unwrap().push(v);
        }
    });

    let pushed_id = commands::push_update(sender.clone(), sender.state::<AppState>(), room.clone())
        .await
        .expect("push_update (the Send-tab 'Previously connected' reconnect action) should succeed");
    assert_eq!(pushed_id, first_info.snapshot_id, "unchanged project resent should carry the same snapshot id");

    let got_it = wait_until(Duration::from_secs(10), || !update_events.lock().unwrap().is_empty()).await;
    assert!(got_it, "receiver should have received the update pushed over the reused connection");

    // ---- the whole point: the pushed update DOES show up as recognized
    // (round 10 still works through a reconnect), but recognition changes
    // display only - it never runs anything. ----
    let events = update_events.lock().unwrap();
    let event = &events[0];
    let recognized_name = event
        .get("recognized_peer")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str());
    assert_eq!(
        recognized_name,
        Some("Trusted Laptop"),
        "the pushed update should show as coming from the peer we explicitly remembered, got: {event:?}"
    );

    assert!(
        receiver.state::<AppState>().verified.lock().unwrap().contains_key(&pushed_id),
        "the pushed update must be held for review, exactly like any other receive"
    );
    assert!(
        receiver.state::<AppState>().sessions.lock().unwrap().is_empty(),
        "neither reconnecting via the roster nor recognizing the sender may auto-run anything - \
         Run is still the only door to run_snapshot"
    );
    assert!(
        sender.state::<AppState>().sessions.lock().unwrap().is_empty(),
        "the sender never runs anything at all"
    );
}
