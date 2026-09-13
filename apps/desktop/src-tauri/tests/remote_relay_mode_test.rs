//! Proves round-10's configurable relay mode at the real Tauri command
//! layer: `commands::start_send_session` takes `mode`/`relay_url` (a real
//! sender-side choice, unchanged), extending (not replacing) the shape
//! `send_flow_test.rs` already exercises for `share_snapshot`/
//! `receive_snapshot` themselves (untouched, still called with a plain
//! `room_code`/`signaling_url` pair either way).
//!
//! `commands::decode_room_code` no longer takes a `mode` at all as of round
//! 25 - see its own doc comment for why a receiver-supplied mode was both
//! redundant and a real bug (a receiver's own, unrelated Settings toggle
//! could block a perfectly valid code). Every call to it below reflects
//! that: the code alone decides which branch runs.
//!
//! "remote" mode's whole point is a relay that's already running somewhere
//! else, separate from the app - proven here with a real
//! `ls_net::host_ephemeral_relay()` instance standing in for that already-
//! running relay. That's a legitimate proof, not a shortcut: the embedded
//! relay implements the exact same wire protocol `apps/signaling-server`
//! does (see `discovery.rs`'s module doc comment), so this is proving real
//! protocol compatibility, not skipping it. No Node process needed.
//!
//! "local" mode's full transfer path is already proven end-to-end by
//! `crates/ls-net/tests/embedded_relay_test.rs` and by `send_flow_test.rs`'s
//! pre-round-10 behavior; `local_mode_still_produces_the_old_encoded_code`
//! below only needs to confirm the mode branch in `commands.rs` wasn't
//! regressed, not re-prove the whole transfer.

use std::path::{Path, PathBuf};
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

/// Same requirement `pipeline_test.rs`/`send_flow_test.rs` document:
/// `create_snapshot` needs `project_root` to be a repo root of its own.
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
            "commit", "-q", "-m", "remote relay mode test snapshot",
        ],
    );
    (dir, project_dir)
}

/// Real payload transfer through nothing but "remote" mode's returned
/// values: `start_send_session("remote", Some(relay_url))` on the sender
/// side, `decode_room_code("remote", code, Some(relay_url))` on the
/// receiver side, both against a real `host_ephemeral_relay()` instance
/// standing in for an already-running, separately-hosted relay - then the
/// existing, unmodified `share_snapshot`/`receive_snapshot`. Confirms no
/// LAN-IP-encoded code appears anywhere in this path: the room code IS the
/// bare room id.
#[tokio::test]
async fn remote_mode_transfers_a_real_payload_via_an_already_running_relay() {
    // Stand-in for "an already-running, separately-hosted relay" - real
    // relay, real protocol, just started by this test instead of by a human
    // running `node apps/signaling-server/index.js` ahead of time.
    let (port, _relay_task) = ls_net::host_ephemeral_relay()
        .await
        .expect("failed to start stand-in relay");
    let relay_url = format!("ws://127.0.0.1:{port}");

    let sample_project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../sample-project");
    assert!(sample_project.is_dir(), "expected {} to exist", sample_project.display());

    let sender_home = tempfile::tempdir().unwrap();
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::set_var(home_var, sender_home.path());

    let (_tempdir, project_dir) = make_git_project_from(&sample_project, "sample-project");

    // ---- sender side: start_send_session("remote", ...) ----
    let send_info = commands::start_send_session("remote".to_string(), Some(relay_url.clone()))
        .await
        .expect("start_send_session should succeed in remote mode");
    assert_eq!(
        send_info.room_code, send_info.room_id,
        "remote mode's room_code must be the bare room_id, not an IP-encoded string"
    );
    assert_eq!(send_info.room_code.len(), 4, "generate_room_id() ids are 4 characters");
    assert_eq!(send_info.signaling_url, relay_url, "remote mode must reuse the configured relay_url verbatim");

    // ---- receiver side: decode_room_code(<pasted code>, ...) ----
    // Round 25: no `mode` argument anymore - a bare 4-character room id is
    // unambiguously remote-shaped on its own (see decode_room_code's own
    // doc comment), so this is real self-describing-code behavior, not a
    // trimmed-down call.
    let decoded = commands::decode_room_code(send_info.room_code.clone(), Some(relay_url.clone()))
        .expect("decode_room_code should succeed for a remote-shaped code");
    assert_eq!(decoded.room_id, send_info.room_id, "the pasted code IS the room_id in remote mode");
    assert_eq!(decoded.signaling_url, relay_url);

    // ---- real transfer through the existing, unmodified commands ----
    let sender_app = tauri::test::mock_app();
    sender_app.manage(AppState::default());
    let sender_handle = sender_app.handle().clone();
    let receiver_app = tauri::test::mock_app();
    receiver_app.manage(AppState::default());
    let receiver_handle = receiver_app.handle().clone();

    let project_path = project_dir.display().to_string();
    let sender_room = send_info.room_id.clone();
    let sender_url = send_info.signaling_url.clone();
    let sender_task = tokio::spawn(async move {
        let state = sender_handle.state::<AppState>();
        commands::share_snapshot(sender_handle.clone(), state, project_path, sender_room, sender_url).await
    });

    let receiver_room = decoded.room_id.clone();
    let receiver_url = decoded.signaling_url.clone();
    let receiver_task = tokio::spawn(async move {
        let state = receiver_handle.state::<AppState>();
        commands::receive_snapshot(receiver_handle.clone(), state, receiver_room, receiver_url).await
    });

    let sender_result = sender_task.await.expect("sender task panicked");
    let receiver_result = receiver_task.await.expect("receiver task panicked");

    let snapshot_id = sender_result.expect("share_snapshot should succeed over the remote-mode relay");
    let info = receiver_result.expect("receive_snapshot should succeed over the remote-mode relay");

    assert_eq!(info.snapshot_id, snapshot_id);
    assert_eq!(info.manifest.project_name, "sample-project");
    assert!(!info.diff.entries.is_empty(), "an initial snapshot should report every file as added");

    let held = receiver_app
        .state::<AppState>()
        .verified
        .lock()
        .unwrap()
        .contains_key(&info.snapshot_id);
    assert!(held, "receive_snapshot should stash the verified snapshot in AppState");
}

/// Confirms the `mode == "local"` branch of `start_send_session` wasn't
/// regressed: still hosts its own relay and produces the old fixed-width,
/// LAN-IP-encoded room code. The full local-mode transfer path itself is
/// already proven by `crates/ls-net/tests/embedded_relay_test.rs`, so this
/// is a fast branch-didn't-regress check, not a re-proof of that path.
///
/// The `decode_room_code` call below is round 25's actual bug, proven
/// directly: a receiver connects using nothing but the pasted code and
/// `None` for `relay_url` - no relay configuration of any kind present,
/// let alone consulted. Before round 25, the frontend independently
/// gated this exact call behind a `mode` value read from Settings' own
/// relay-mode toggle (meant for the *sender* side); if that toggle
/// happened to be on "remote", a real local-mode code like this one would
/// never even have reached this command - the frontend blocked it first
/// with "Remote relay URL is required". This test proves the command
/// itself no longer has - or needs - any such gate.
#[tokio::test]
async fn local_mode_still_produces_the_old_encoded_code() {
    let info = commands::start_send_session("local".to_string(), None)
        .await
        .expect("start_send_session should succeed in local mode");

    assert_eq!(info.room_code.len(), 14, "local mode's room_code must stay the fixed-width encoded string");
    assert!(info.room_code.chars().all(|c| c.is_ascii_alphanumeric()));
    assert_ne!(info.room_code, info.room_id, "local mode's room_code encodes IP+port+id, unlike remote mode");

    // Round 25: a receiver never picks a mode, never configures a relay
    // URL for this case - `None` here, not just an empty string, is the
    // whole point.
    let decoded = commands::decode_room_code(info.room_code.clone(), None)
        .expect("a local-mode code must decode with zero relay configuration present");
    assert_eq!(decoded.room_id, info.room_id);
    assert_eq!(decoded.signaling_url, info.signaling_url);
}

/// Remote mode without a relay_url is a real, user-facing error - not a
/// silent fallback to local behavior.
#[tokio::test]
async fn remote_mode_without_a_relay_url_is_a_real_error() {
    let result = commands::start_send_session("remote".to_string(), None).await;
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("remote mode must require a relay_url"),
    };
    assert!(err.contains("relay_url"), "error should mention the missing relay_url, got: {err}");

    // "abcd" is a real, valid-shaped remote-mode code (generate_room_id()'s
    // own 4-character output) - decode_room_code must recognize it as
    // remote-shaped (not silently misread as an incomplete local code) and
    // fail specifically because no relay_url was given, not because the
    // code itself looked wrong.
    let result = commands::decode_room_code("abcd".to_string(), None);
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("a remote-shaped code with no relay_url must fail"),
    };
    assert!(
        err.contains("relay") || err.contains("URL"),
        "error should mention needing a relay URL, got: {err}"
    );
}

/// The actual self-describing-code claim round 25 makes: decode_room_code
/// takes no `mode` argument at all anymore, and correctly tells a real
/// local-shaped code apart from a real remote-shaped one purely from the
/// code's own length/content - proven with one of each, back to back, in
/// the same test, with no shared state or ordering dependency between them.
#[tokio::test]
async fn decode_room_code_tells_local_and_remote_codes_apart_with_no_mode_argument() {
    let local_info = commands::start_send_session("local".to_string(), None)
        .await
        .expect("start_send_session should succeed in local mode");
    let local_decoded =
        commands::decode_room_code(local_info.room_code.clone(), None).expect("a local-shaped code must decode as local");
    assert_eq!(local_decoded.signaling_url, local_info.signaling_url);

    let (port, _relay_task) = ls_net::host_ephemeral_relay().await.expect("failed to start stand-in relay");
    let relay_url = format!("ws://127.0.0.1:{port}");
    let remote_info = commands::start_send_session("remote".to_string(), Some(relay_url.clone()))
        .await
        .expect("start_send_session should succeed in remote mode");
    let remote_decoded = commands::decode_room_code(remote_info.room_code.clone(), Some(relay_url.clone()))
        .expect("a remote-shaped code must decode as remote when a relay_url is available");
    assert_eq!(remote_decoded.signaling_url, relay_url);
    assert_eq!(remote_decoded.room_id, remote_info.room_id);
}
