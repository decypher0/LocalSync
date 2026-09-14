//! Round 28: reproduces (and then proves fixed) the exact scenario the
//! round's bug report describes - starting a second, *different* send while
//! one session is already active - which round 11's own
//! `multi_receiver_session_test.rs` never actually exercised: that test's
//! two `share_snapshot` calls are fully sequential (`send1.await` happens
//! before `send2` is even spawned), so it only proves two sessions can
//! *coexist afterward*, never that two sends can be genuinely in flight *at
//! the same time* from the same sender. This file spawns both send tasks
//! before awaiting either - true overlap, not "one after the other, fast."

use std::path::Path;
use std::process::Command;

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
        &["-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-q", "-m", "concurrent-session test snapshot"],
    );
    (dir, project_dir)
}

/// Round 28 goal A3: two genuinely *concurrent* sends, different projects,
/// different receivers, from one sender - both send tasks are spawned
/// before either is awaited, and both receive tasks likewise, so their real
/// work (bundling, signing, connecting, transferring) actually overlaps in
/// wall-clock time rather than running back to back. If starting a second
/// send while the first is still active silently clobbered shared state
/// (a single "current session" assumption anywhere in the command layer),
/// this is the shape of test that would catch it - `multi_receiver_session_test.rs`
/// cannot, by construction (see its own `send1.await` before `send2` is spawned).
#[tokio::test]
async fn two_concurrent_sends_different_projects_different_receivers() {
    let sender_home = tempfile::tempdir().unwrap();
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::set_var(home_var, sender_home.path());

    let sample_a = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../sample-project");
    let sample_b = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../sample-project-node");
    let (_tmp_a, project_a) = make_git_project_from(&sample_a, "sample-project");
    let (_tmp_b, project_b) = make_git_project_from(&sample_b, "sample-project-node");

    let (relay_port, _relay_task) = ls_net::host_ephemeral_relay().await.expect("failed to start embedded relay");
    let signaling_url = format!("ws://127.0.0.1:{relay_port}");

    let sender_app = tauri::test::mock_app();
    sender_app.manage(AppState::default());
    let sender = sender_app.handle().clone();

    let receiver_a_app = tauri::test::mock_app();
    receiver_a_app.manage(AppState::default());
    let receiver_a = receiver_a_app.handle().clone();

    let receiver_b_app = tauri::test::mock_app();
    receiver_b_app.manage(AppState::default());
    let receiver_b = receiver_b_app.handle().clone();

    let room_a = ls_net::generate_room_id();
    let room_b = ls_net::generate_room_id();
    assert_ne!(room_a, room_b, "two independent sends must not collide on room id");

    // ---- spawn BOTH sends, and BOTH receives, before awaiting any of them ----
    // This is the actual point of this test: send_a and send_b's real work
    // (git bundling, signing, signaling connect, WebRTC handshake, transfer)
    // genuinely races each other on the same sender AppState/process, not
    // "started one after the other so fast it doesn't matter."
    let send_a = {
        let (sender_h, room, url, path) = (sender.clone(), room_a.clone(), signaling_url.clone(), project_a.display().to_string());
        tokio::spawn(async move {
            let state = sender_h.state::<AppState>();
            commands::share_snapshot(sender_h.clone(), state, path, room, url).await
        })
    };
    let send_b = {
        let (sender_h, room, url, path) = (sender.clone(), room_b.clone(), signaling_url.clone(), project_b.display().to_string());
        tokio::spawn(async move {
            let state = sender_h.state::<AppState>();
            commands::share_snapshot(sender_h.clone(), state, path, room, url).await
        })
    };
    let recv_a = {
        let (h, room, url) = (receiver_a.clone(), room_a.clone(), signaling_url.clone());
        tokio::spawn(async move {
            let state = h.state::<AppState>();
            commands::receive_snapshot(h.clone(), state, room, url).await
        })
    };
    let recv_b = {
        let (h, room, url) = (receiver_b.clone(), room_b.clone(), signaling_url.clone());
        tokio::spawn(async move {
            let state = h.state::<AppState>();
            commands::receive_snapshot(h.clone(), state, room, url).await
        })
    };

    let (send_a_result, send_b_result, recv_a_result, recv_b_result) = tokio::join!(send_a, send_b, recv_a, recv_b);

    let snapshot_id_a = send_a_result.expect("send_a task panicked").expect("share_snapshot(project A) should succeed while a second send is concurrently in flight");
    let snapshot_id_b = send_b_result.expect("send_b task panicked").expect("share_snapshot(project B) should succeed while a first send is concurrently in flight");
    let info_a = recv_a_result.expect("recv_a task panicked").expect("receive_snapshot for project A should succeed");
    let info_b = recv_b_result.expect("recv_b task panicked").expect("receive_snapshot for project B should succeed");

    // ---- each receiver got the RIGHT project, not a mixed-up/overwritten one ----
    assert_eq!(info_a.snapshot_id, snapshot_id_a);
    assert_eq!(info_b.snapshot_id, snapshot_id_b);
    assert_eq!(info_a.manifest.project_name, "sample-project", "receiver A must get project A's manifest, not project B's");
    assert_eq!(info_b.manifest.project_name, "sample-project-node", "receiver B must get project B's manifest, not project A's");
    assert_ne!(snapshot_id_a, snapshot_id_b);

    // ---- the sender's own roster shows BOTH sessions, both correctly attributed ----
    let roster = commands::list_connected_receivers(sender.state::<AppState>()).expect("list_connected_receivers should succeed");
    assert_eq!(roster.len(), 2, "both concurrent sessions should be tracked at once, got: {roster:?}");
    let roster_ids: Vec<&str> = roster.iter().map(|r| r.peer_id.as_str()).collect();
    assert!(roster_ids.contains(&room_a.as_str()));
    assert!(roster_ids.contains(&room_b.as_str()));

    // ---- each roster entry still points at its OWN project path, not the other one's ----
    // (push_update/respond_to_pull_request re-bundle from this - if the two
    // concurrent sends had clobbered each other's AppState entry, this is
    // exactly where it would show up.)
    {
        let sender_state = sender.state::<AppState>();
        let receivers = sender_state.connected_receivers.lock().unwrap();
        let entry_a = receivers.get(&room_a).expect("room_a should have a roster entry");
        let entry_b = receivers.get(&room_b).expect("room_b should have a roster entry");
        assert_eq!(Path::new(&entry_a.project_path), project_a.as_path());
        assert_eq!(Path::new(&entry_b.project_path), project_b.as_path());
    }

    // ---- neither side auto-ran anything ----
    assert!(sender.state::<AppState>().sessions.lock().unwrap().is_empty());
    assert!(receiver_a.state::<AppState>().sessions.lock().unwrap().is_empty());
    assert!(receiver_b.state::<AppState>().sessions.lock().unwrap().is_empty());
}
