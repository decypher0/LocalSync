//! Reopening a saved received session after an app restart: whether it's
//! running must come from Podman (the in-memory record of the Run is gone
//! after a restart), a running one must come back with its real ports, and
//! Stop must tear its containers down by the session's id alone.
//!
//! "Restart" = a second app with completely fresh in-memory state, sharing
//! only what's on disk (the saved session, via the same data dir) and the
//! real Podman containers. Needs real Podman; skips without it.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use localsync_desktop::commands::{self, FolderPlanDto};
use localsync_desktop::receiver_session_commands;
use localsync_desktop::session_commands::{self, SendRequest};
use localsync_desktop::state::AppState;
use tauri::{Listener, Manager};

type Handle = tauri::AppHandle<tauri::test::MockRuntime>;

const WEB_PORT: u16 = 8094;

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

fn containers_of(project: &str) -> Vec<String> {
    let out = Command::new("podman")
        .args(["ps", "-a", "--filter", &format!("label=io.podman.compose.project={project}"), "--format", "{{.Names}}"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.trim().is_empty()).map(String::from).collect()
}

fn http_ok(port: u16) -> bool {
    let Ok(mut s) = TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse().unwrap(), Duration::from_secs(1)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    if s.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n").is_err() {
        return false;
    }
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    buf.lines().next().is_some_and(|l| l.contains("200"))
}

#[tokio::test]
async fn a_saved_session_reopened_after_a_restart_reports_running_with_its_ports_and_stops() {
    if !podman_stack_available() {
        eprintln!("podman/podman-compose not on PATH - skipping");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home.path());
    std::env::set_var("XDG_DATA_HOME", home.path().join("data"));

    let base = tempfile::tempdir().unwrap();
    let project: PathBuf = base.path().join("resume-project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(
        project.join("docker-compose.yml"),
        // Python's built-in server: unlike nginx:alpine, it keeps running on
        // the sandbox's read-only root filesystem (nginx exits writing its cache).
        format!(
            "services:\n  web:\n    image: docker.io/library/python:3.12-slim\n    command: [\"python\", \"-m\", \"http.server\", \"80\"]\n    ports:\n      - \"{WEB_PORT}:80\"\n"
        ),
    )
    .unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "one"]);
    let (url, _server) = signaling_url(8174).await;

    // ---- first app process: receive, Run, save ----
    let (sender, receiver) = (app(), app());
    let folders = vec![FolderPlanDto { path: project.display().to_string(), dump: None, compose: None }];
    let sid = session_commands::create_project_session(sender.state::<AppState>(), folders, None).await.unwrap().id;
    let r = receiver.clone();
    let r_url = url.clone();
    let recv = tokio::spawn(async move { commands::receive_snapshot(r.clone(), r.state::<AppState>(), "resumeroom".into(), r_url).await });
    let request = SendRequest {
        session_id: sid,
        room_code: "resumeroom".into(),
        signaling_url: url.clone(),
        require_accept: false,
        sender_name: "Alice".into(),
        device_key: None,
        device_name: "Bob".into(),
        since_last: false,
    };
    session_commands::send_project_session(sender.clone(), sender.state::<AppState>(), request).await.expect("send");
    let info = recv.await.unwrap().expect("receive");
    let id = info.received_session_id.clone();
    let work = tempfile::tempdir().unwrap();
    // The receiver's Logs panel listens for `run-progress` tagged with the
    // session's snapshot id; the receiver Run used to emit none at all.
    let progress = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = progress.clone();
    let snapshot_id = info.snapshot_id.clone();
    receiver.listen("run-progress", move |e| {
        let v: serde_json::Value = serde_json::from_str(e.payload()).unwrap();
        if v["session_id"] == snapshot_id.as_str() {
            sink.lock().unwrap().push(v["line"].as_str().unwrap_or_default().to_string());
        }
    });
    let ran = receiver_session_commands::run_received_session(receiver.clone(), receiver.state::<AppState>(), id.clone(), work.path().display().to_string())
        .await
        .expect("run");
    let lines = progress.lock().unwrap().clone();
    assert!(lines.iter().any(|l| l.contains("provisioning check")), "the readiness check streams live: {lines:?}");
    assert!(lines.iter().any(|l| l.contains("containers started")), "and the container start: {lines:?}");
    let project_label = ran.running.session_id.clone();
    receiver_session_commands::save_received_session(receiver.state::<AppState>(), id.clone()).unwrap();
    drop(receiver); // the first process is gone; its containers keep running

    // ---- "restart": a fresh app, nothing in memory, same disk + containers ----
    let restarted = app();
    assert!(restarted.state::<AppState>().sessions.lock().unwrap().is_empty(), "nothing from the Run survives in memory");
    let started = Instant::now();
    let view = receiver_session_commands::open_saved_received_session(restarted.state::<AppState>(), id.clone()).expect("reopen");
    let reopen_ms = started.elapsed().as_millis();
    eprintln!("REOPEN (incl. podman running check) took {reopen_ms} ms");
    assert!(view.running, "its containers are still running, so it must say so after a restart");
    assert_eq!(view.service_ports, Some(vec![("web".to_string(), format!("{WEB_PORT}:80"))]), "real ports from the compose file it ran");
    assert_eq!(view.db_cache_hit, None, "cache status isn't known after a restart - reported as unknown, not guessed");
    assert!(http_ok(WEB_PORT), "and it really is serving");

    // ---- Stop by the session id alone ----
    receiver_session_commands::stop_received_session(restarted.state::<AppState>(), id.clone()).await.expect("stop");
    assert!(containers_of(&project_label).is_empty(), "Stop must remove the session's containers: {:?}", containers_of(&project_label));
    assert!(!http_ok(WEB_PORT));

    // ---- reopened while not running: a plain resume, no ports ----
    let again = app();
    let idle = receiver_session_commands::open_saved_received_session(again.state::<AppState>(), id.clone()).expect("reopen idle");
    assert!(!idle.running);
    assert_eq!(idle.service_ports, None);
    assert_eq!(idle.db_cache_hit, None);
}
