//! Round 23: proves the Cloud-drop control-channel protocol end to end over
//! a real `ls_net::host_ephemeral_relay()` connection (same legitimate
//! same-box stand-in `multi_receiver_session_test.rs` already uses - real
//! WebSocket signaling + a real WebRTC data channel, just not a separately
//! hosted Node process).
//!
//! What this file deliberately does NOT attempt: any real call into
//! `ls_clouddrop::drive`/`oauth` - those need a real Google OAuth Client ID
//! and a live Google account, which don't exist in this sandbox (see
//! `crates/ls-clouddrop`'s own wiremock-backed tests for that layer's
//! coverage). What's provable without them, and exercised here for real:
//! the reject path never reaches Drive at all (structurally - it returns
//! before `cloud_drop_config()` is ever called) and the wire protocol
//! (`CloudAccessRequest` -> `CloudAccessResponse`) round-trips correctly
//! between two independent real connections.

use std::sync::Arc;

use localsync_desktop::{commands, state::AppState};
use tauri::Manager;

#[tokio::test]
async fn declined_access_request_sends_no_grant_and_no_file_id() {
    let (relay_port, _relay_task) = ls_net::host_ephemeral_relay().await.expect("failed to start embedded relay");
    let signaling_url = format!("ws://127.0.0.1:{relay_port}");
    let room_id = ls_net::generate_room_id();

    // Two independent real connections to the same room - exactly what
    // `start_cloud_drop_session` / `request_cloud_drop_access` each do,
    // minus the Drive upload/download either side of them.
    let (sender_conn, receiver_conn) = tokio::join!(
        ls_net::connect_as_sender(&signaling_url, &room_id),
        ls_net::connect_as_receiver(&signaling_url, &room_id),
    );
    let sender_conn = Arc::new(sender_conn.expect("sender connect failed"));
    let receiver_conn = Arc::new(receiver_conn.expect("receiver connect failed"));

    let sender_app = tauri::test::mock_app();
    sender_app.manage(AppState::default());
    let sender = sender_app.handle().clone();
    sender.state::<AppState>().cloud_drop_uploads.lock().unwrap().insert(
        room_id.clone(),
        localsync_desktop::state::CloudDropUpload {
            conn: sender_conn.clone(),
            file_id: "fake-drive-file-id".to_string(),
            retention: ls_clouddrop::retention::Retention::DeleteAfterDownload,
        },
    );

    // Receiver announces its (fake) linked account - a real
    // `CloudAccessRequest` on the wire, same as `request_cloud_drop_access`
    // sends.
    ls_net::send_control(
        &receiver_conn,
        &ls_net::ControlMessage::CloudAccessRequest { google_email: "receiver@example.com".to_string() },
    )
    .await
    .expect("send_control should succeed");

    // Stand in for `listen_for_cloud_access_requests`'s one real effect
    // (stashing the email for `respond_to_cloud_access_request` to use) -
    // not spawning the background task itself, since this test is about the
    // command's own behavior, not the listener loop.
    match ls_net::recv_control(&sender_conn).await.expect("recv_control should succeed") {
        ls_net::ControlMessage::CloudAccessRequest { google_email } => {
            sender.state::<AppState>().cloud_access_requests.lock().unwrap().insert(room_id.clone(), google_email);
        }
        other => panic!("expected a CloudAccessRequest, got {other:?}"),
    }

    // The actual command under test. If this reached `cloud_drop_config()`
    // (which it must not, on the reject path) it would fail outright - no
    // `GOOGLE_OAUTH_CLIENT_ID` is set in this test process - so this
    // succeeding at all is itself proof the reject branch never touches
    // Cloud drop's Google-facing config, let alone grants a real permission.
    commands::respond_to_cloud_access_request(sender.state::<AppState>(), room_id.clone(), false)
        .await
        .expect("declining a cloud-access request should succeed without any Google credentials");

    match ls_net::recv_control(&receiver_conn).await.expect("recv_control should succeed") {
        ls_net::ControlMessage::CloudAccessResponse { accepted, drive_file_id } => {
            assert!(!accepted, "a declined request must report accepted: false");
            assert!(drive_file_id.is_none(), "a declined request must never hand back a file id to download");
        }
        other => panic!("expected a CloudAccessResponse, got {other:?}"),
    }

    // And the pending-request bookkeeping is cleaned up, not left to leak
    // across a second request from the same peer.
    assert!(
        !sender.state::<AppState>().cloud_access_requests.lock().unwrap().contains_key(&room_id),
        "a responded-to request must be removed from the pending map"
    );
}

#[tokio::test]
async fn responding_to_an_unknown_peer_is_an_error_not_a_panic() {
    let sender_app = tauri::test::mock_app();
    sender_app.manage(AppState::default());
    let err = commands::respond_to_cloud_access_request(sender_app.state::<AppState>(), "no-such-peer".to_string(), true)
        .await
        .expect_err("responding to a peer with no pending request should fail cleanly");
    assert!(err.contains("no pending"), "expected a clear error message, got: {err}");
}
