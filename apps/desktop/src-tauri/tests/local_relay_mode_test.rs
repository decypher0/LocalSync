//! Real-LAN-shaped coverage for "Local network" mode's actual send/receive
//! path, end to end through the same Tauri command surface the frontend
//! calls: `start_send_session("local", None)` (real `detect_lan_ip()`, real
//! embedded relay bound to `0.0.0.0`) -> `decode_room_code` (round 25's
//! self-describing decode, no mode argument) -> `share_snapshot`/
//! `receive_snapshot` connecting over the REAL detected LAN address, not
//! `127.0.0.1`.
//!
//! This exact combination was previously untested: `embedded_relay_test.rs`
//! and `local_mode_still_produces_the_old_encoded_code` (in
//! `remote_relay_mode_test.rs`) both stand in `127.0.0.1` for
//! `detect_lan_ip()`'s result rather than using it - their own doc comments
//! say so explicitly. Real user testing on an actual LAN found "Local
//! network" mode's room code expiring with the receiver never connecting;
//! this test reproduces that exact path same-box (two processes' worth of
//! state, one machine) to root-cause it for real rather than guessing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use localsync_desktop::{commands, commands::FolderPlanDto, state::AppState};
use tauri::Manager;

fn sh(dir: &Path, cmd: &str, args: &[&str]) {
    let status = Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {cmd} {args:?}: {e}"));
    assert!(status.success(), "{cmd} {args:?} failed in {}", dir.display());
}

fn make_git_project_from(source: &Path, name: &str) -> (tempfile::TempDir, PathBuf) {
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
            "commit", "-q", "-m", "local relay mode test snapshot",
        ],
    );
    (dir, project_dir)
}

/// Real payload transfer through nothing but "local" mode's actual,
/// real-LAN-IP-based returned values - no `127.0.0.1` substitution anywhere
/// in this test. Bounded well under `ls_net::CONNECT_TIMEOUT` (300s): if
/// this hangs/times out, that itself is the reproduction of the reported
/// bug, so the whole test is wrapped in a much shorter deadline than the
/// real app would wait, to fail fast with a clear message instead of
/// silently burning 5 minutes.
#[tokio::test]
async fn local_mode_transfers_a_real_payload_over_the_real_lan_address() {
    let send_info = commands::start_send_session("local".to_string(), None)
        .await
        .expect("start_send_session should succeed in local mode");
    assert_eq!(send_info.room_code.len(), 14);
    assert_ne!(
        send_info.signaling_url, "ws://127.0.0.1",
        "sanity: this test must exercise the real detected LAN address"
    );
    println!("local mode signaling_url = {}", send_info.signaling_url);

    let decoded = commands::decode_room_code(send_info.room_code.clone(), None)
        .expect("decode_room_code should succeed with zero relay configuration present");
    assert_eq!(decoded.room_id, send_info.room_id);
    assert_eq!(decoded.signaling_url, send_info.signaling_url);

    let sample_project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../sample-project");
    assert!(sample_project.is_dir(), "expected {} to exist", sample_project.display());

    let sender_home = tempfile::tempdir().unwrap();
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::set_var(home_var, sender_home.path());

    let (_tempdir, project_dir) = make_git_project_from(&sample_project, "sample-project");

    let sender_app = tauri::test::mock_app();
    sender_app.manage(AppState::default());
    let sender_handle = sender_app.handle().clone();
    let receiver_app = tauri::test::mock_app();
    receiver_app.manage(AppState::default());
    let receiver_handle = receiver_app.handle().clone();

    let project_path = project_dir.display().to_string();
    let sender_room = send_info.room_id.clone();
    let sender_url = send_info.signaling_url.clone();
    // Real Send flow (app.js's send-btn handler) always calls
    // `share_snapshot_wizard`, never the older `share_snapshot` directly -
    // that command is only still used by push_update/respond_to_pull_request
    // now. Using it here (rather than `share_snapshot`) matches what a real
    // click on Send actually invokes.
    let sender_task = tokio::spawn(async move {
        let state = sender_handle.state::<AppState>();
        let folders = vec![FolderPlanDto { path: project_path, dump: None }];
        commands::share_snapshot_wizard(sender_handle.clone(), state, folders, sender_room, sender_url, false, String::new()).await
    });

    let receiver_room = decoded.room_id.clone();
    let receiver_url = decoded.signaling_url.clone();
    let receiver_task = tokio::spawn(async move {
        let state = receiver_handle.state::<AppState>();
        commands::receive_snapshot(receiver_handle.clone(), state, receiver_room, receiver_url).await
    });

    // Real bug reports describe the FULL 300s CONNECT_TIMEOUT elapsing with
    // no connection - so a much shorter deadline here is a legitimate,
    // fast-failing proof of the same failure, not an unfair test.
    let deadline = Duration::from_secs(30);
    let sender_result = tokio::time::timeout(deadline, sender_task)
        .await
        .expect("sender task did not finish within 30s - this is the reported hang, reproduced")
        .expect("sender task panicked");
    let receiver_result = tokio::time::timeout(deadline, receiver_task)
        .await
        .expect("receiver task did not finish within 30s - this is the reported hang, reproduced")
        .expect("receiver task panicked");

    let snapshot_id = sender_result.expect("share_snapshot should succeed over the real local-mode LAN address");
    let info = receiver_result.expect("receive_snapshot should succeed over the real local-mode LAN address");

    assert_eq!(info.snapshot_id, snapshot_id);
    assert_eq!(info.manifest.project_name, "sample-project");
}
