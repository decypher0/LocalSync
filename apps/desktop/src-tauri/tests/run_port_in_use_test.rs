//! A receiver's Run whose service can't start - here because its host port
//! is already taken - fails with the service's name, instead of reporting
//! the project running with nothing behind it (podman-compose itself exits 0).
//!
//! Real sender -> relay -> receiver flow (same box) and real Podman; skips
//! where podman/podman-compose are absent.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use localsync_desktop::commands::{self, FolderPlanDto, IncomingSnapshotInfo};
use localsync_desktop::session_commands::{self, SendRequest};
use localsync_desktop::state::AppState;
use localsync_desktop::receiver_session_commands;
use tauri::Manager;

type Handle = tauri::AppHandle<tauri::test::MockRuntime>;

fn podman_stack_available() -> bool {
    let ok = |bin: &str| Command::new(bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    ok("podman") && ok("podman-compose")
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn make_project(base: &Path, name: &str, port: u16) -> PathBuf {
    let root = base.join(name);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("README.md"), "v1\n").unwrap();
    std::fs::write(
        root.join("docker-compose.yml"),
        format!(
            "services:\n  web:\n    image: docker.io/library/python:3.12-slim\n    command: [\"python\", \"-m\", \"http.server\", \"80\"]\n    ports:\n      - \"{port}:80\"\n"
        ),
    )
    .unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "one"]);
    root
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

/// Sends `sid` and receives it (a fresh receive, or an armed update).
async fn push(sender: &Handle, receiver: &Handle, url: &str, room: &str, sid: &str) -> IncomingSnapshotInfo {
    let request = SendRequest {
        session_id: sid.to_string(),
        room_code: room.to_string(),
        signaling_url: url.to_string(),
        require_accept: false,
        sender_name: "Alice".into(),
        device_key: None,
        device_name: "Bob".into(),
        since_last: false,
    };
    let r = receiver.clone();
    let (r_room, r_url) = (room.to_string(), url.to_string());
    let recv = tokio::spawn(async move { commands::receive_snapshot(r.clone(), r.state::<AppState>(), r_room, r_url).await });
    session_commands::send_project_session(sender.clone(), sender.state::<AppState>(), request).await.expect("send");
    recv.await.unwrap().expect("receive")
}


#[tokio::test]
async fn a_run_whose_port_is_taken_fails_instead_of_showing_running() {
    if !podman_stack_available() {
        eprintln!("podman/podman-compose not on PATH - skipping");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home.path());
    std::env::set_var("XDG_DATA_HOME", home.path().join("data"));
    let base = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let (url, _server) = signaling_url(8176).await;
    let (sender, receiver) = (app(), app());
    let project = make_project(base.path(), "port-taken", 18143);
    let sid = session_commands::create_project_session(
        sender.state::<AppState>(),
        vec![FolderPlanDto { path: project.display().to_string(), dump: None, compose: None }],
        None,
    )
    .await
    .unwrap()
    .id;
    let info = push(&sender, &receiver, &url, "portroom1", &sid).await;
    let (title, commit) = (info.manifest.project_name.clone(), info.manifest.git_commit.clone());

    // Something else on this computer already listens on the service's port.
    let _taken = std::net::TcpListener::bind("0.0.0.0:18143").expect("hold the port");
    let err = receiver_session_commands::run_received_session(
        receiver.clone(),
        receiver.state::<AppState>(),
        info.received_session_id.clone(),
        work.path().display().to_string(),
    )
    .await;
    let err = err.err().expect("a service that couldn't start must fail the Run");
    assert!(err.contains("`web` could not be started") && err.contains("port"), "{err}");
    assert!(!ls_containers::project_running(&title, &commit).unwrap(), "nothing left running");
}
