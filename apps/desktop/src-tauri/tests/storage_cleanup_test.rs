//! The Storage view against real Podman and a real sender -> relay ->
//! receiver flow on one box:
//! - a closed session's database volume and unpacked folder show up as
//!   unused, and "Clean up unused" removes them and reports the bytes freed;
//! - a listed session's volume/folder is never touched by that, even when
//!   its id is passed in;
//! - "Free up disk" on a running session stops it, then removes its volume
//!   and folder;
//! - the sender's project folders - which sit right inside the work folder
//!   here - and someone's own look-alike folder survive all of it.
//!
//! The "database" is python:3.12-slim tagged with a name containing
//! `postgres` (what makes LocalSync treat a service as a database and pin its
//! volume); it writes 3 MB into that volume and serves HTTP. Skips where
//! podman/podman-compose are absent.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use localsync_desktop::commands::{self, FolderPlanDto, IncomingSnapshotInfo};
use localsync_desktop::session_commands::{self, SendRequest};
use localsync_desktop::state::AppState;
use localsync_desktop::{receiver_session_commands, session_list_commands, storage_commands};
use tauri::Manager;

type Handle = tauri::AppHandle<tauri::test::MockRuntime>;

const BASE_IMAGE: &str = "docker.io/library/python:3.12-slim";
const FAKE_DB_IMAGE: &str = "localhost/ls-storage-test-postgres:latest";

fn podman_stack_available() -> bool {
    let ok = |bin: &str| Command::new(bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    ok("podman") && ok("podman-compose")
}

fn podman(args: &[&str]) -> bool {
    Command::new("podman").args(args).status().map(|s| s.success()).unwrap_or(false)
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

fn make_project(base: &Path, name: &str, port: u16, nonce: &str) -> PathBuf {
    let root = base.join(name);
    std::fs::create_dir_all(root.join("db-seed")).unwrap();
    std::fs::write(root.join("README.md"), "v1\n").unwrap();
    // Unique seed -> a volume name no other run of this test shares.
    std::fs::write(root.join("db-seed/seed.txt"), format!("{name} {nonce}\n")).unwrap();
    std::fs::write(
        root.join("docker-compose.yml"),
        format!(
            "services:\n  web:\n    image: {BASE_IMAGE}\n    command: [\"python\", \"-m\", \"http.server\", \"80\"]\n    ports:\n      - \"{port}:80\"\n  \
             db:\n    image: {FAKE_DB_IMAGE}\n    command: [\"sh\", \"-c\", \"head -c 3000000 /dev/urandom > /data/blob && exec python -m http.server 5432\"]\n    \
             volumes:\n      - dbdata:/data\nvolumes:\n  dbdata: {{}}\n"
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

async fn send_and_receive(sender: &Handle, receiver: &Handle, url: &str, room: &str, project: &Path) -> IncomingSnapshotInfo {
    let sid = session_commands::create_project_session(
        sender.state::<AppState>(),
        vec![FolderPlanDto { path: project.display().to_string(), dump: None, compose: None }],
        None,
    )
    .await
    .unwrap()
    .id;
    let request = SendRequest {
        session_id: sid,
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

async fn run(receiver: &Handle, id: &str, work: &Path) {
    receiver_session_commands::run_received_session(receiver.clone(), receiver.state::<AppState>(), id.to_string(), work.display().to_string())
        .await
        .expect("Run");
}

fn unpack_dir(work: &Path, info: &IncomingSnapshotInfo) -> PathBuf {
    work.join(ls_containers::unpack_dir_name(&info.manifest.project_name, &info.manifest.git_commit))
}

fn volume_of(info: &IncomingSnapshotInfo) -> String {
    format!("localsync-db-{}", info.manifest.db_seed_hash)
}

#[tokio::test]
async fn clean_up_unused_and_free_session_disk_remove_only_what_they_should() {
    if !podman_stack_available() {
        eprintln!("podman/podman-compose not on PATH - skipping");
        return;
    }

    let home = tempfile::tempdir().unwrap();
    std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home.path());
    std::env::set_var("XDG_DATA_HOME", home.path().join("data"));
    // After the env change: rootless Podman's image store follows XDG_DATA_HOME.
    if !podman(&["image", "exists", BASE_IMAGE]) {
        assert!(podman(&["pull", "-q", BASE_IMAGE]), "pulling {BASE_IMAGE}");
    }
    assert!(podman(&["tag", BASE_IMAGE, FAKE_DB_IMAGE]), "tagging the fake database image");
    // The work folder is also where the sender's projects live.
    let work = tempfile::tempdir().unwrap();
    let nonce = format!("{:?}", std::time::SystemTime::now());
    let (url, _server) = signaling_url(8176).await;
    let (sender, receiver) = (app(), app());

    // Someone's own folder that merely looks like an unpack folder by name.
    let decoy = work.path().join(format!("decoy-{}", "a".repeat(40)));
    std::fs::create_dir_all(decoy.join("source")).unwrap();
    std::fs::write(decoy.join("notes.txt"), "mine").unwrap();

    // ---- A: received, run, then closed -> its volume and folder are leftovers ----
    let project_a = make_project(work.path(), "storage-closed", 18151, &nonce);
    let a = send_and_receive(&sender, &receiver, &url, "storageroom1", &project_a).await;
    run(&receiver, &a.received_session_id, work.path()).await;
    let (dir_a, vol_a) = (unpack_dir(work.path(), &a), volume_of(&a));
    assert!(dir_a.join(ls_containers::UNPACK_MARKER).is_file(), "the unpack marker is written");
    assert!(podman(&["volume", "exists", &vol_a]), "precondition: A's volume exists");
    session_list_commands::close_session(receiver.state::<AppState>(), a.received_session_id.clone()).await.expect("close A");
    assert!(dir_a.is_dir() && podman(&["volume", "exists", &vol_a]), "closing keeps the volume and folder");

    // ---- B: received and running -> listed, never "unused" ----
    let project_b = make_project(work.path(), "storage-listed", 18152, &nonce);
    let b = send_and_receive(&sender, &receiver, &url, "storageroom2", &project_b).await;
    run(&receiver, &b.received_session_id, work.path()).await;
    let (dir_b, vol_b) = (unpack_dir(work.path(), &b), volume_of(&b));

    let started = std::time::Instant::now();
    let report = storage_commands::storage_report(receiver.state::<AppState>()).await.expect("report");
    eprintln!(
        "storage_report took {:?} (scan_ms={}): total={} unused={} breakdown={:?}",
        started.elapsed(),
        report.scan_ms,
        report.total_bytes,
        report.unused_bytes,
        report.breakdown.iter().map(|p| (p.label.as_str(), p.bytes)).collect::<Vec<_>>()
    );
    assert_eq!(report.podman_error, None);
    let unused_ids: Vec<&str> = report.unused.iter().map(|u| u.id.as_str()).collect();
    let (vol_a_id, dir_a_id) = (format!("volume:{vol_a}"), format!("folder:{}", dir_a.display()));
    assert!(unused_ids.contains(&vol_a_id.as_str()), "A's volume is unused: {unused_ids:?}");
    assert!(unused_ids.contains(&dir_a_id.as_str()), "A's folder is unused: {unused_ids:?}");
    assert!(!unused_ids.iter().any(|id| id.contains(&vol_b) || id.contains(&*dir_b.display().to_string())), "B is listed: {unused_ids:?}");
    let never: Vec<String> = [&decoy, &project_a, &project_b].iter().map(|p| format!("folder:{}", p.display())).collect();
    assert!(!unused_ids.iter().any(|id| never.iter().any(|n| n == id)), "{unused_ids:?}");
    let a_bytes: u64 = report.unused.iter().filter(|u| u.id == vol_a_id || u.id == dir_a_id).map(|u| u.bytes).sum();
    let vol_a_bytes = report.unused.iter().find(|u| u.id == vol_a_id).unwrap().bytes;
    assert!(vol_a_bytes >= 2_000_000, "the volume's size comes from Podman: {vol_a_bytes}");
    let row_b = report.sessions.iter().find(|s| s.id == b.received_session_id).expect("B has a row");
    assert!(row_b.running, "B is running");
    assert_eq!(row_b.dirs.len(), 1);
    assert!(row_b.dirs_bytes > 0);
    assert_eq!(row_b.volumes.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), vec![vol_b.as_str()]);
    assert!(row_b.volumes_bytes >= 2_000_000, "{}", row_b.volumes_bytes);
    assert!(!report.sessions.iter().any(|s| s.id == a.received_session_id), "A was closed");

    // ---- Clean up unused: removes A's leftovers; B's ids, even if passed, are refused ----
    let vol_b_id = format!("volume:{vol_b}");
    let dir_b_id = format!("folder:{}", dir_b.display());
    let result = storage_commands::clean_up_unused_inner(
        &receiver.state::<AppState>(),
        vec![vol_a_id.clone(), dir_a_id.clone(), vol_b_id.clone(), dir_b_id.clone()],
    )
    .await
    .expect("clean up");
    eprintln!("clean_up_unused: {result:?}");
    assert_eq!(result.removed, vec![vol_a_id.clone(), dir_a_id.clone()]);
    assert_eq!(result.freed_bytes, a_bytes, "reports the bytes it freed");
    assert_eq!(result.kept.len(), 2, "{:?}", result.kept);
    assert!(!dir_a.exists(), "A's folder is gone");
    assert!(!podman(&["volume", "exists", &vol_a]), "A's volume is gone");
    assert!(dir_b.is_dir() && podman(&["volume", "exists", &vol_b]), "B's volume and folder are untouched");
    assert!(ls_containers::project_running(&b.manifest.project_name, &b.manifest.git_commit).unwrap(), "B still runs");

    // ---- Free up disk on running B: stops it, then removes its volume and folder ----
    let freed = storage_commands::free_session_disk_inner(&receiver.state::<AppState>(), &b.received_session_id).await.expect("free B");
    eprintln!("free_session_disk: {freed:?}");
    assert!(!ls_containers::project_running(&b.manifest.project_name, &b.manifest.git_commit).unwrap(), "B was stopped first");
    assert!(!dir_b.exists(), "B's folder is gone");
    assert!(!podman(&["volume", "exists", &vol_b]), "B's volume is gone");
    assert!(freed.freed_bytes >= row_b.dirs_bytes + 2_000_000, "{freed:?}");
    assert!(freed.kept.is_empty(), "{:?}", freed.kept);
    let b_after = receiver.state::<AppState>().received_sessions.lock().unwrap().get(&b.received_session_id).cloned().expect("B stays listed");
    assert_eq!(b_after.compose_dir, "", "the removed folder is forgotten");

    // ---- What must survive everything ----
    for project in [&project_a, &project_b] {
        assert!(project.join("README.md").is_file() && project.join("docker-compose.yml").is_file(), "{project:?} survived");
    }
    assert!(decoy.join("notes.txt").is_file(), "a look-alike folder that isn't LocalSync's survived");
    assert!(work.path().is_dir(), "the work folder itself survived");

    let _ = podman(&["rmi", FAKE_DB_IMAGE]);
}
