//! Cloud drop's send side, after the Drive upload (`begin_cloud_drop_handoff`
//! - Drive and OAuth can't run here, the handoff can): the code must come back
//! straight away, with nobody connected. It used to wait for the receiver
//! first - who could only connect with the code it never returned - until the
//! 5-minute timeout. Real local relay + WebRTC between two ends on one box.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use localsync_desktop::commands;
use localsync_desktop::state::AppState;
use tauri::{Listener, Manager};

type Handle = tauri::AppHandle<tauri::test::MockRuntime>;

fn app() -> Handle {
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    app.handle().clone()
}

/// Collects the `room_id`s carried by `event`.
fn record(app: &Handle, event: &str) -> Arc<Mutex<Vec<String>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    app.listen_any(event, move |e| {
        let v: serde_json::Value = serde_json::from_str(e.payload()).unwrap_or_default();
        let id = v.get("room_id").or_else(|| v.get("peer_id")).and_then(|x| x.as_str()).unwrap_or_default().to_string();
        sink.lock().unwrap().push(id);
    });
    seen
}

async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..300 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for: {what}");
}

#[tokio::test]
async fn the_code_comes_back_at_once_and_a_receiver_entering_it_is_reported() {
    let sender = app();
    let joined = record(&sender, "cloud-drop-receiver-joined");
    let requests = record(&sender, "cloud-access-request");

    // ---- returns without anyone connecting ----
    let info = tokio::time::timeout(
        Duration::from_secs(10),
        commands::begin_cloud_drop_handoff(&sender, "local".into(), None, "drive-file-1".into(), ls_clouddrop::retention::Retention::DeleteAfterDownload),
    )
    .await
    .expect("the code must come back without waiting for a receiver")
    .expect("handoff");
    assert!(!info.room_code.is_empty());
    assert_eq!(info.file_id, "drive-file-1");
    assert!(sender.state::<AppState>().cloud_drop_waits.lock().unwrap().contains_key(&info.room_id), "a background task waits for the receiver");
    assert!(joined.lock().unwrap().is_empty(), "nobody has joined yet");

    // ---- the receiver enters the code ----
    let decoded = commands::decode_room_code(info.room_code.clone(), None).expect("decode the code the sender shows");
    let conn = ls_net::connect_as_receiver(&decoded.signaling_url, &decoded.room_id).await.expect("receiver joins");
    until("cloud-drop-receiver-joined for this room", || joined.lock().unwrap().contains(&info.room_id)).await;
    assert!(sender.state::<AppState>().cloud_drop_uploads.lock().unwrap().contains_key(&info.room_id), "the upload is filed for the approval");

    // ---- their access request still reaches the sender (unchanged flow) ----
    ls_net::send_control(&conn, &ls_net::ControlMessage::CloudAccessRequest { google_email: "bob@example.com".into() }).await.unwrap();
    until("cloud-access-request from this room", || requests.lock().unwrap().contains(&info.room_id)).await;
    assert_eq!(
        sender.state::<AppState>().cloud_access_requests.lock().unwrap().get(&info.room_id).map(String::as_str),
        Some("bob@example.com")
    );

    // ---- Close: the wait/listen task stops and the connection closes ----
    commands::cancel_cloud_drop(sender.state::<AppState>(), info.room_id.clone()).unwrap();
    assert!(!sender.state::<AppState>().cloud_drop_uploads.lock().unwrap().contains_key(&info.room_id));
    assert!(!sender.state::<AppState>().cloud_drop_waits.lock().unwrap().contains_key(&info.room_id));
    let closed = tokio::time::timeout(Duration::from_secs(30), ls_net::recv_control(&conn)).await;
    assert!(matches!(closed, Ok(Err(_))), "the receiver sees the sender's control channel close");
}

#[tokio::test]
async fn cancelling_before_anyone_joins_stops_the_wait() {
    let sender = app();
    let info = commands::begin_cloud_drop_handoff(&sender, "local".into(), None, "drive-file-2".into(), ls_clouddrop::retention::Retention::DeleteAfterDownload)
        .await
        .expect("handoff");
    commands::cancel_cloud_drop(sender.state::<AppState>(), info.room_id.clone()).unwrap();
    assert!(sender.state::<AppState>().cloud_drop_waits.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_receiver_who_leaves_is_reported_so_the_card_does_not_wait_forever() {
    let sender = app();
    let joined = record(&sender, "cloud-drop-receiver-joined");
    let ended = record(&sender, "cloud-drop-ended");
    let info = commands::begin_cloud_drop_handoff(&sender, "local".into(), None, "drive-file-3".into(), ls_clouddrop::retention::Retention::DeleteAfterDownload)
        .await
        .expect("handoff");
    let decoded = commands::decode_room_code(info.room_code.clone(), None).unwrap();
    let conn = ls_net::connect_as_receiver(&decoded.signaling_url, &decoded.room_id).await.expect("receiver joins");
    until("joined", || joined.lock().unwrap().contains(&info.room_id)).await;
    // What the real receiver does when it's done (request_cloud_drop_access).
    conn.close().await;
    until("cloud-drop-ended for this room", || ended.lock().unwrap().contains(&info.room_id)).await;
    assert!(!sender.state::<AppState>().cloud_drop_uploads.lock().unwrap().contains_key(&info.room_id), "its dead connection is forgotten");
}

#[test]
fn nobody_entering_the_code_gets_a_cloud_drop_message_not_the_p2p_one() {
    let timeout = anyhow::anyhow!("timed out connecting to peer after 300s");
    let msg = commands::cloud_drop_wait_error(&timeout, "ws://192.168.1.9:4000");
    assert_eq!(msg, "No one entered this code within 5 minutes. The file is still in your Drive — send again to get a new code.");
    assert!(!msg.contains("other device didn't connect"));
    // A relay that can't be reached keeps its own (shared) explanation.
    let unreachable = anyhow::anyhow!("failed to connect to signaling server at ws://x:1");
    assert!(commands::cloud_drop_wait_error(&unreachable, "ws://x:1").starts_with("Couldn't reach the relay"));
}
