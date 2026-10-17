//! A push diffs against what the device last received only when that is
//! still of use; otherwise the full snapshot goes, and the sender is told
//! why (same box, over real transfers):
//!
//! - the receiver confirms it holds the marker's commits (BaseCheck) -> diff;
//! - it holds a different version, or no longer has the project -> full,
//!   "the receiver had a different version";
//! - it never answers (an older LocalSync) -> full after the timeout;
//! - the marker's commit was pruned from the sender's repo -> full, without
//!   asking;
//! - new (untracked) files show in "changes since last send", can be ticked
//!   out, and ship when ticked in; a secret-looking one starts out unticked.
//!
//! The receivers here are stand-ins that answer BaseCheck the way a test
//! needs (`ls_net::receive_payload_answering`), so this covers the sender's
//! decisions independently of how a real receiver looks up what it holds.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use localsync_desktop::commands::FolderPlanDto;
use localsync_desktop::project_session::{FULL_BASE_GONE, FULL_DIFFERENT_VERSION, FULL_NO_ANSWER};
use localsync_desktop::session_commands::{self, SendRequest, SendResult};
use localsync_desktop::state::AppState;
use tauri::Manager;

fn sh(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn make_project(base: &Path) -> PathBuf {
    let root = base.join("base-check-project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("README.md"), "v1\n").unwrap();
    std::fs::write(root.join("docker-compose.yml"), "services: {}\n").unwrap();
    sh(&root, &["init", "-q"]);
    sh(&root, &["add", "-A"]);
    sh(&root, &["commit", "-q", "-m", "one"]);
    root
}

fn commit_change(root: &Path, text: &str) {
    std::fs::write(root.join("README.md"), text).unwrap();
    sh(root, &["commit", "-q", "-am", text.trim()]);
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

fn unpack(payload: &[u8]) -> HashMap<String, Vec<u8>> {
    let mut archive = tar::Archive::new(zstd::stream::read::Decoder::new(payload).unwrap());
    let mut out = HashMap::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().replace('\\', "/");
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).unwrap();
        out.insert(path, buf);
    }
    out
}

/// How the stand-in receiver behaves.
#[derive(Clone)]
enum Receiver {
    /// Answers BaseCheck with these commits (`None`: no such project).
    Holding(Option<Vec<String>>),
    /// An older build: never reads the control channel before the payload.
    Old,
}

struct Pushed {
    sent: SendResult,
    snapshot: ls_snapshot::Snapshot,
    /// Whether a BaseCheck was asked.
    asked: bool,
    took: Duration,
}

async fn send(sender: &tauri::AppHandle<tauri::test::MockRuntime>, url: &str, room: &str, request: SendRequest, receiver: Receiver) -> Pushed {
    let request = SendRequest { room_code: room.to_string(), signaling_url: url.to_string(), ..request };
    let (r_room, r_url) = (room.to_string(), url.to_string());
    let task = tokio::spawn(async move {
        let conn = ls_net::connect_as_receiver(&r_url, &r_room).await.expect("receiver connects");
        let mut asked = false;
        let bytes = match receiver {
            Receiver::Old => ls_net::receive_payload(&conn, |_, _| {}).await,
            Receiver::Holding(held) => {
                ls_net::receive_payload_answering(&conn, |_, _| {}, |msg| match msg {
                    ls_net::ControlMessage::BaseCheck { .. } => {
                        asked = true;
                        Some(ls_net::ControlMessage::BaseCheckResponse { commits: held.clone() })
                    }
                    _ => None,
                })
                .await
            }
        }
        .expect("receive");
        (serde_json::from_slice::<ls_snapshot::Snapshot>(&bytes).unwrap(), asked)
    });
    let started = Instant::now();
    let sent = session_commands::send_project_session(sender.clone(), sender.state::<AppState>(), request)
        .await
        .expect("send_project_session should succeed");
    let took = started.elapsed();
    let (snapshot, asked) = task.await.unwrap();
    Pushed { sent, snapshot, asked, took }
}

fn request(session_id: &str, device_key: Option<&str>, since_last: bool) -> SendRequest {
    SendRequest {
        session_id: session_id.to_string(),
        room_code: String::new(),
        signaling_url: String::new(),
        require_accept: false,
        sender_name: String::new(),
        device_key: device_key.map(str::to_string),
        device_name: "Alice".to_string(),
        since_last,
    }
}

fn marker_commit(sent: &SendResult, key: &str) -> String {
    sent.view.devices.iter().find(|d| d.key == key).unwrap().marker.as_ref().unwrap().commits[0].commit.clone()
}

#[tokio::test]
async fn a_push_diffs_only_against_a_version_the_receiver_confirms_it_holds() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home.path());
    std::env::set_var("XDG_DATA_HOME", home.path().join("data"));
    let base = tempfile::tempdir().unwrap();
    let project = make_project(base.path());
    let folder = project.display().to_string();
    let (url, _server) = signaling_url(8181).await;

    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    let sender = app.handle().clone();
    let folders = vec![FolderPlanDto { path: folder.clone(), dump: None, compose: None, externals: vec![] }];
    let view = session_commands::create_project_session(sender.state::<AppState>(), folders, None).await.unwrap();
    let sid = view.id.clone();

    // ---- nothing sent yet: Push update has no target ----
    let status = session_commands::project_push_status(sender.state::<AppState>(), sid.clone()).await.unwrap();
    assert!(status.last_device_key.is_none() && !status.changed);

    // ---- a first send asks nothing, and names the session in the manifest ----
    let first = send(&sender, &url, "bc1", request(&sid, None, false), Receiver::Holding(None)).await;
    assert!(!first.asked, "a plain send has no base to check");
    assert_eq!(first.sent.full_reason, None);
    assert_eq!(first.snapshot.manifest.project_id.as_deref(), Some(sid.as_str()));
    let key = first.sent.device_key.clone();
    let commit1 = marker_commit(&first.sent, &key);

    // ---- the receiver holds exactly what was sent: the push is a diff ----
    commit_change(&project, "v2\n");
    let pushed = send(&sender, &url, "bc2", request(&sid, Some(&key), true), Receiver::Holding(Some(vec![commit1.clone()]))).await;
    assert!(pushed.asked);
    assert_eq!(pushed.sent.full_reason, None);
    assert_eq!(pushed.snapshot.manifest.git_parent_commit.as_deref(), Some(commit1.as_str()));
    let commit2 = marker_commit(&pushed.sent, &key);
    assert_ne!(commit2, commit1);

    // ---- it holds something else, or nothing: the full snapshot goes ----
    commit_change(&project, "v3\n");
    let pushed = send(&sender, &url, "bc3", request(&sid, Some(&key), true), Receiver::Holding(Some(vec!["0123abcd".into()]))).await;
    assert!(pushed.asked);
    assert_eq!(pushed.sent.full_reason.as_deref(), Some(FULL_DIFFERENT_VERSION));
    assert!(pushed.snapshot.manifest.git_parent_commit.is_none(), "sent in full");
    let commit3 = marker_commit(&pushed.sent, &key);
    assert_eq!(pushed.snapshot.manifest.git_commit, commit3, "the marker still moves to what was sent");

    commit_change(&project, "v4\n");
    let pushed = send(&sender, &url, "bc4", request(&sid, Some(&key), true), Receiver::Holding(None)).await;
    assert_eq!(pushed.sent.full_reason.as_deref(), Some(FULL_DIFFERENT_VERSION), "the receiver's session is gone");
    assert!(pushed.snapshot.manifest.git_parent_commit.is_none());

    // ---- an older receiver never answers: full, after the timeout ----
    commit_change(&project, "v5\n");
    let pushed = send(&sender, &url, "bc5", request(&sid, Some(&key), true), Receiver::Old).await;
    assert_eq!(pushed.sent.full_reason.as_deref(), Some(FULL_NO_ANSWER));
    assert!(pushed.snapshot.manifest.git_parent_commit.is_none());
    assert!(pushed.took >= Duration::from_secs(5), "waited for the BaseCheck reply: {:?}", pushed.took);
    let commit5 = marker_commit(&pushed.sent, &key);

    // ---- new files: listed, tickable, secrets start unticked ----
    std::fs::write(project.join("README.md"), "v6, not committed\n").unwrap();
    std::fs::write(project.join("notes.md"), "new\n").unwrap();
    std::fs::write(project.join(".env"), "TOKEN=x\n").unwrap();
    let changes = session_commands::project_changes(sender.state::<AppState>(), sid.clone()).await.unwrap();
    assert!(changes.status.changed);
    assert_eq!(changes.status.last_device_key.as_deref(), Some(key.as_str()));
    let lines: Vec<(String, String, bool, bool)> =
        changes.folders[0].files.iter().map(|f| (f.path.clone(), f.change_type.clone(), f.new_file, f.included)).collect();
    assert_eq!(
        lines,
        vec![
            (".env".to_string(), "added".to_string(), true, false),
            ("README.md".to_string(), "modified".to_string(), false, true),
            ("notes.md".to_string(), "added".to_string(), true, true),
        ]
    );
    let status = session_commands::project_push_status(sender.state::<AppState>(), sid.clone()).await.unwrap();
    assert_eq!(status, changes.status, "the panel and Push update give the same answer");

    session_commands::set_project_change_selection(sender.state::<AppState>(), sid.clone(), folder.clone(), "notes.md".into(), false).unwrap();
    let changes = session_commands::project_changes(sender.state::<AppState>(), sid.clone()).await.unwrap();
    assert!(changes.folders[0].files.iter().any(|f| f.path == "notes.md" && !f.included), "unticked, still listed");
    session_commands::set_project_change_selection(sender.state::<AppState>(), sid.clone(), folder.clone(), "notes.md".into(), true).unwrap();
    assert!(session_commands::set_project_change_selection(sender.state::<AppState>(), sid.clone(), "/elsewhere".into(), "x".into(), true).is_err());
    let saved = localsync_desktop::session_history::find_project(&sid).unwrap().unwrap();
    assert!(saved.included_paths.iter().any(|p| p.ends_with("/notes.md")), "the choice is kept with the session");

    // The push diffs against commit5 (the receiver holds it) and carries the
    // edit and the ticked new file, not the secret one.
    let pushed = send(&sender, &url, "bc6", request(&sid, Some(&key), true), Receiver::Holding(Some(vec![commit5.clone()]))).await;
    assert_eq!(pushed.sent.full_reason, None);
    assert_eq!(pushed.snapshot.manifest.git_parent_commit.as_deref(), Some(commit5.as_str()));
    let files = unpack(&pushed.snapshot.payload);
    assert!(files.contains_key("base-check-project/source/notes.md"), "{:?}", files.keys().collect::<Vec<_>>());
    assert!(!files.contains_key("base-check-project/source/.env"));
    let unsent_edit = marker_commit(&pushed.sent, &key);
    assert_eq!(sh_output(&project, &["status", "--porcelain"]), " M README.md\n?? .env\n?? notes.md", "the project itself is untouched");
    let after = session_commands::project_changes(sender.state::<AppState>(), sid.clone()).await.unwrap();
    assert!(!after.status.changed, "everything ticked was sent");
    assert_eq!(after.folders[0].files.len(), 1, "only the unticked secret is left: {:?}", after.folders[0].files);

    // ---- the marker's commit gets pruned: full, without asking ----
    std::fs::remove_file(project.join("notes.md")).unwrap();
    commit_change(&project, "v7\n");
    sh(&project, &["reflog", "expire", "--expire=now", "--all"]);
    sh(&project, &["gc", "--prune=now", "-q"]);
    assert!(!ls_snapshot::commit_exists(&project, &unsent_edit), "gc pruned the unsent-edit commit");
    let changes = session_commands::project_changes(sender.state::<AppState>(), sid.clone()).await.unwrap();
    assert!(changes.status.changed && changes.folders[0].base_gone, "the panel can't list against a pruned commit");
    let pushed = send(&sender, &url, "bc7", request(&sid, Some(&key), true), Receiver::Holding(Some(vec![unsent_edit.clone()]))).await;
    assert!(!pushed.asked, "nothing to diff against, so nothing to check");
    assert_eq!(pushed.sent.full_reason.as_deref(), Some(FULL_BASE_GONE));
    assert!(pushed.snapshot.manifest.git_parent_commit.is_none());
}

fn sh_output(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim_end().to_string()
}
