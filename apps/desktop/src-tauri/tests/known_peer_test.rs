//! The critical proof for round 10 goal 2 (known-peer pairing): recognizing
//! a peer must never shortcut the diff-review-then-Run consent gate.
//!
//! Drives the real command-layer logic — `commands::finalize_received_snapshot`,
//! the exact function `commands::receive_snapshot` calls with the bytes once
//! they're off the wire (verify -> diff -> known-peers lookup -> hold in
//! `AppState`) — against a real signed `Snapshot` built via
//! `ls_snapshot::create_snapshot`, with a real `State<AppState>` from
//! `tauri::test::mock_app()`, same pattern `run_retry_test.rs`/
//! `run_progress_test.rs` use.
//!
//! Doesn't go through `ls_net`'s live WebRTC handshake (that hop is already
//! covered end-to-end by `tests/send_flow_test.rs`, and needs a real
//! network/NAT path that isn't available in every sandbox); this test proves
//! the actual consent-gate-relevant code, not a reimplementation of it —
//! `receive_snapshot` itself is a thin wrapper: connect, receive bytes,
//! deserialize, then call this exact function.
//!
//! Both the recognized and unrecognized cases live in *one* `#[test]` (not
//! two): each case mutates the process-global `HOME`/`XDG_DATA_HOME` env
//! vars (identity + known-peers store location), and Rust runs `#[test]`
//! functions in parallel by default within one binary — same reasoning
//! `pipeline_test.rs`/`send_flow_test.rs` document for why they mutate `HOME`
//! only from a single test.

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

fn make_minimal_git_project(dir: &Path) {
    std::fs::write(dir.join("README.md"), "known-peer test project\n").unwrap();
    sh(dir, "git", &["init", "-q"]);
    sh(dir, "git", &["add", "-A"]);
    sh(
        dir,
        "git",
        &[
            "-c", "user.name=Test", "-c", "user.email=test@example.com",
            "commit", "-q", "-m", "known-peer test snapshot",
        ],
    );
}

/// Builds a real signed snapshot from a fresh sender identity under an
/// isolated `HOME`/`USERPROFILE`.
fn build_signed_snapshot(home_var: &str) -> ls_snapshot::Snapshot {
    let sender_home = tempfile::tempdir().unwrap();
    std::env::set_var(home_var, sender_home.path());

    let project_dir = tempfile::tempdir().unwrap();
    make_minimal_git_project(project_dir.path());
    ls_snapshot::create_snapshot(project_dir.path(), None)
        .expect("create_snapshot should succeed against a minimal git-initialized project")
}

#[test]
fn recognized_peer_never_bypasses_the_run_gate() {
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let data_dir_var = if cfg!(windows) { "APPDATA" } else { "XDG_DATA_HOME" };

    // ============ case 1: a pre-known sender ============
    let snapshot = build_signed_snapshot(home_var);
    let sender_pubkey = snapshot.manifest.sender_pubkey;

    // Isolated known-peers data dir, pre-populated with the sender's real
    // pubkey under a chosen name *before* the receive happens.
    let receiver_data_dir = tempfile::tempdir().unwrap();
    std::env::set_var(data_dir_var, receiver_data_dir.path());
    let mut known_peers = ls_security::KnownPeers::load_default()
        .expect("KnownPeers::load_default should succeed against the isolated data dir");
    known_peers
        .remember(sender_pubkey, "Trusted Laptop".to_string())
        .expect("remember should persist the pre-known peer");

    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    let handle = app.handle().clone();
    let state = handle.state::<AppState>();

    let info = commands::finalize_received_snapshot(&state, snapshot)
        .expect("finalize_received_snapshot should succeed for a validly-signed snapshot");

    // --- (1) recognized as the pre-known peer, by name ---
    let recognized = info
        .recognized_peer
        .as_ref()
        .expect("a pre-known sender pubkey must come back as recognized_peer: Some(...)");
    assert_eq!(recognized.name, "Trusted Laptop");
    assert!(!recognized.first_seen.is_empty());
    assert_eq!(info.sender_pubkey_hex.len(), 64, "sender_pubkey_hex should be a 64-char hex string");

    // --- (2) the snapshot is still held, not auto-run ---
    let still_held = handle.state::<AppState>().verified.lock().unwrap().contains_key(&info.snapshot_id);
    assert!(
        still_held,
        "recognizing a peer must not consume/auto-run the held snapshot — it must still be \
         sitting in state.verified, exactly as an unrecognized sender's would be"
    );

    // --- (3) no session was implicitly created - run_snapshot was never
    // called, recognized peer or not ---
    let sessions_empty = handle.state::<AppState>().sessions.lock().unwrap().is_empty();
    assert!(
        sessions_empty,
        "recognizing a peer must never implicitly invoke run_snapshot — state.sessions must stay empty \
         until an explicit Run"
    );

    // ============ case 2: an unrecognized sender - same gate holds ============
    // "New sender" is not treated any differently by the gate, only by what
    // the UI banner says. A fresh sender identity + a fresh, empty
    // known-peers store (never `remember`ed this key).
    let snapshot2 = build_signed_snapshot(home_var);
    let receiver_data_dir2 = tempfile::tempdir().unwrap();
    std::env::set_var(data_dir_var, receiver_data_dir2.path());

    let app2 = tauri::test::mock_app();
    app2.manage(AppState::default());
    let handle2 = app2.handle().clone();
    let state2 = handle2.state::<AppState>();

    let info2 = commands::finalize_received_snapshot(&state2, snapshot2)
        .expect("finalize_received_snapshot should succeed for a validly-signed snapshot");

    assert!(info2.recognized_peer.is_none(), "an unremembered sender must come back as None, not recognized");
    assert!(handle2.state::<AppState>().verified.lock().unwrap().contains_key(&info2.snapshot_id));
    assert!(handle2.state::<AppState>().sessions.lock().unwrap().is_empty());
}
