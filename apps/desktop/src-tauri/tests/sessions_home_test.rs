//! Home's session list: every session - sent and received - is persisted from
//! the moment it exists, carries the name the person gave it, and is still
//! listed after an app restart (a fresh app sharing only the data dir);
//! closing one removes it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use localsync_desktop::commands::{self, FolderPlanDto};
use localsync_desktop::session_commands::{self, SendRequest};
use localsync_desktop::session_list_commands;
use localsync_desktop::state::AppState;
use tauri::Manager;

type Handle = tauri::AppHandle<tauri::test::MockRuntime>;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

async fn signaling_url(port: u16) -> (String, Option<tokio::process::Child>) {
    if let Ok(url) = std::env::var("LS_NET_TEST_SIGNALING_URL") {
        return (url, None);
    }
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../apps/signaling-server/index.js");
    let child = tokio::process::Command::new("node")
        .arg(script)
        .env("PORT", port.to_string())
        .kill_on_drop(true)
        .spawn()
        .expect("failed to spawn `node` for the signaling server");
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (format!("ws://127.0.0.1:{port}"), Some(child))
}

fn app() -> Handle {
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    app.handle().clone()
}

#[tokio::test]
async fn every_named_session_is_listed_after_a_restart_and_closing_removes_it() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home.path());
    std::env::set_var("XDG_DATA_HOME", home.path().join("data"));
    // On Windows, dirs::data_dir() is %APPDATA%.
    std::env::set_var("APPDATA", home.path().join("data"));

    let base = tempfile::tempdir().unwrap();
    let project: PathBuf = base.path().join("demo-backend");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("docker-compose.yml"), "services:\n  web:\n    image: docker.io/library/busybox\n").unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "one"]);
    let (url, _server) = signaling_url(8175).await;

    // ---- a sent session, named in the wizard, never explicitly saved ----
    let (sender, receiver) = (app(), app());
    let folders = vec![FolderPlanDto { path: project.display().to_string(), dump: None, compose: None }];
    let send = session_commands::create_project_session(sender.state::<AppState>(), folders, Some("Client Demo Backend".into()))
        .await
        .unwrap();

    // ---- a received session, then named ----
    let r = receiver.clone();
    let r_url = url.clone();
    let recv = tokio::spawn(async move { commands::receive_snapshot(r.clone(), r.state::<AppState>(), "homeroom".into(), r_url).await });
    let request = SendRequest {
        session_id: send.id.clone(),
        room_code: "homeroom".into(),
        signaling_url: url.clone(),
        require_accept: false,
        sender_name: "Alice".into(),
        device_key: None,
        device_name: "Bob".into(),
        since_last: false,
    };
    session_commands::send_project_session(sender.clone(), sender.state::<AppState>(), request).await.expect("send");
    let info = recv.await.unwrap().expect("receive");
    session_list_commands::rename_session(receiver.state::<AppState>(), info.received_session_id.clone(), "ArtigemRS Admin Panel".into()).unwrap();
    assert!(session_list_commands::rename_session(receiver.state::<AppState>(), info.received_session_id.clone(), "  ".into()).is_err());

    // ---- "restart": a fresh app with nothing in memory ----
    let fresh = app();
    let items = session_list_commands::list_sessions(fresh.state::<AppState>()).unwrap();
    let sent = items.iter().find(|i| i.id == send.id).expect("the sent session survives a restart without Save");
    assert_eq!((sent.kind.as_str(), sent.name.as_str()), ("send", "Client Demo Backend"));
    assert_ne!(sent.updated_at, sent.created_at, "updated by the send");
    let got = items.iter().find(|i| i.id == info.received_session_id).expect("the received session survives a restart");
    assert_eq!((got.kind.as_str(), got.name.as_str(), got.running), ("receive", "ArtigemRS Admin Panel", false));

    // The display name never replaces the project name its containers use.
    let reopened = localsync_desktop::receiver_session_commands::open_saved_received_session(fresh.state::<AppState>(), info.received_session_id.clone()).unwrap();
    assert_eq!(reopened.name, "ArtigemRS Admin Panel");
    assert_eq!(reopened.title, "demo-backend");

    // ---- Close removes each from the list, for good ----
    session_list_commands::close_session(fresh.state::<AppState>(), send.id.clone()).await.unwrap();
    session_list_commands::close_session(fresh.state::<AppState>(), info.received_session_id.clone()).await.unwrap();
    let after = session_list_commands::list_sessions(app().state::<AppState>()).unwrap();
    assert!(after.iter().all(|i| i.id != send.id && i.id != info.received_session_id), "{after:?}");
}
