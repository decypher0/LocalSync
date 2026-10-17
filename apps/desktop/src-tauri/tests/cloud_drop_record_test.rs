//! A granted Cloud drop is recorded on its sender session once, and the UI
//! hears about it through `cloud-drop-delivered` wherever the approval was
//! clicked. The receiver's download confirmation must not count it again.
//! A Push update to that cloud device is a new Cloud drop of the session,
//! which moves the same device's marker.
//!
//! Everything except Google Drive is real: the session, its artifact, the
//! handoff over the local relay (WebRTC), and `finish_cloud_grant` - what
//! `respond_to_cloud_access_request` runs after the Drive grant.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use localsync_desktop::commands::{self, FolderPlanDto};
use localsync_desktop::state::{AppState, CloudDelivery};
use localsync_desktop::session_commands;
use tauri::{Listener, Manager};

type Handle = tauri::AppHandle<tauri::test::MockRuntime>;

const BOB: &str = "cloud:bob@example.com";

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git").args(["-c", "user.name=T", "-c", "user.email=t@example.com"]).args(args).current_dir(dir).status().unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn record(app: &Handle, event: &str) -> Arc<Mutex<Vec<serde_json::Value>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    app.listen_any(event, move |e| sink.lock().unwrap().push(serde_json::from_str(e.payload()).unwrap()));
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

fn has_room(events: &Mutex<Vec<serde_json::Value>>, room: &str) -> bool {
    events.lock().unwrap().iter().any(|v| v["room_id"] == room)
}

/// One Cloud drop of the session's current artifact, granted to bob, with
/// bob confirming the download and leaving. Returns the grant's view and
/// the room.
async fn drop_to_bob(sender: &Handle, sid: &str) -> (session_commands::ProjectSessionView, String) {
    let state = sender.state::<AppState>();
    let (_, artifact) = session_commands::cloud_drop_payload(&state, sid).await.unwrap();
    let delivery = CloudDelivery {
        session_id: sid.to_string(),
        snapshot_id: artifact.snapshot_id.clone(),
        commits: artifact.commits.clone(),
        bytes: artifact.bytes.len() as u64,
        upload_ms: 1000,
    };
    let handoff = commands::begin_cloud_drop_handoff(sender, "local".into(), None, "drive-file".into(), ls_clouddrop::retention::Retention::DeleteAfterDownload, delivery)
        .await
        .unwrap();
    let room = handoff.room_id.clone();
    let decoded = commands::decode_room_code(handoff.room_code, None).unwrap();
    let conn = ls_net::connect_as_receiver(&decoded.signaling_url, &decoded.room_id).await.unwrap();
    ls_net::send_control(&conn, &ls_net::ControlMessage::CloudAccessRequest { google_email: "bob@example.com".into() }).await.unwrap();
    until("the access request", || state.cloud_access_requests.lock().unwrap().contains_key(&room)).await;

    let view = commands::finish_cloud_grant(sender, &room, "bob@example.com").await.unwrap().expect("session open");
    assert!(matches!(ls_net::recv_control(&conn).await.unwrap(), ls_net::ControlMessage::CloudAccessResponse { accepted: true, .. }));
    ls_net::send_control(&conn, &ls_net::ControlMessage::CloudDownloadConfirmed).await.unwrap();
    conn.close().await;
    (view, room)
}

#[tokio::test]
async fn a_cloud_drop_is_recorded_once_announced_and_pushed_again_as_a_new_drop() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home.path());
    std::env::set_var("XDG_DATA_HOME", home.path().join("data"));

    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("cloud-record-app");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("index.html"), "<h1>v1</h1>").unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "one"]);

    let sender = tauri::test::mock_app();
    sender.manage(AppState::default());
    let sender = sender.handle().clone();
    let delivered = record(&sender, "cloud-drop-delivered");
    let ended = record(&sender, "cloud-drop-ended");
    let folders = vec![FolderPlanDto { path: project.display().to_string(), dump: None, compose: None, externals: vec![] }];
    let sid = session_commands::create_project_session(sender.state::<AppState>(), folders, None).await.unwrap().id;

    // ---- first drop: recorded on the grant, and announced ----
    let (view, room) = drop_to_bob(&sender, &sid).await;
    let bob = view.devices.iter().find(|d| d.key == BOB).expect("bob is a device");
    assert_eq!((bob.sends, bob.name.as_str()), (1, "bob@example.com"));
    let first_snapshot = bob.marker.as_ref().unwrap().snapshot_id.clone();
    {
        let events = delivered.lock().unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["room_id"], room.as_str());
        assert_eq!(events[0]["session_id"], sid.as_str());
        let devices = events[0]["view"]["devices"].as_array().unwrap();
        assert!(devices.iter().any(|d| d["key"] == BOB && d["sends"] == 1), "{devices:?}");
    }

    // ---- bob's download confirmation: no second history entry, no event ----
    // `cloud-drop-ended` comes after the listener has handled everything bob sent.
    until("the first drop's control channel to end", || has_room(&ended, &room)).await;
    let session = localsync_desktop::session_history::find_project(&sid).unwrap().expect("saved");
    assert_eq!(session.device(BOB).unwrap().history.len(), 1, "the confirmation didn't count it again");
    assert_eq!(delivered.lock().unwrap().len(), 1);
    assert!(!session_commands::project_push_status(sender.state::<AppState>(), sid.clone()).await.unwrap().changed);

    // ---- Push update to bob: a new Cloud drop of the changed project ----
    std::fs::write(project.join("index.html"), "<h1>v2</h1>").unwrap();
    git(&project, &["commit", "-q", "-am", "two"]);
    let status = session_commands::project_push_status(sender.state::<AppState>(), sid.clone()).await.unwrap();
    assert_eq!(status.last_device_key.as_deref(), Some(BOB));
    assert!(status.changed, "bob is behind");

    let (view, room2) = drop_to_bob(&sender, &sid).await;
    let bob = view.devices.iter().find(|d| d.key == BOB).unwrap();
    assert_eq!(bob.sends, 2, "the same device, one more delivery");
    assert_ne!(bob.marker.as_ref().unwrap().snapshot_id, first_snapshot, "bob's marker moved to the new version");
    assert!(bob.up_to_date);
    until("the second drop's control channel to end", || has_room(&ended, &room2)).await;
    assert_eq!(delivered.lock().unwrap().len(), 2);
    let session = localsync_desktop::session_history::find_project(&sid).unwrap().unwrap();
    assert_eq!(session.device(BOB).unwrap().history.len(), 2);
    assert!(!session_commands::project_push_status(sender.state::<AppState>(), sid.clone()).await.unwrap().changed);
}
