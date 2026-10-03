//! A MongoDB dump sent with a wizard project is restored on the receiver's
//! first Run - the receiver's database is not left empty.
//!
//! The dump is a real `mongodump` archive (made by the mongo image itself,
//! laid out the way `ls_dbsource` packs one: `dump/<source db>/...`, tar+gz),
//! sent sender -> relay -> receiver on one box, and run with real Podman.
//! Skips where podman/podman-compose are absent.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use ls_composegen::{BuildTool, ComposeSpec, DatabaseSpec, DbEngine, DbEnvPreset, JavaPackaging, Runtime};
use localsync_desktop::commands::{self, DumpPlanDto, FolderPlanDto};
use localsync_desktop::receiver_session_commands;
use localsync_desktop::session_commands::{self, SendRequest};
use localsync_desktop::state::AppState;
use tauri::Manager;

type Handle = tauri::AppHandle<tauri::test::MockRuntime>;

const PORT: u16 = 18132;
const SRC: &str = "lsmongo-dump-src";

fn podman_stack_available() -> bool {
    let ok = |bin: &str| Command::new(bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    ok("podman") && ok("podman-compose")
}

fn run_ok(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{cmd:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn podman(args: &[&str]) -> String {
    run_ok(Command::new("podman").args(args))
}

fn git(dir: &Path, args: &[&str]) {
    run_ok(Command::new("git").args(["-c", "user.name=T", "-c", "user.email=t@example.com"]).args(args).current_dir(dir));
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

/// A real mongodump of database `srcdb` (3 documents in `items`), packed as
/// `dump/srcdb/...` in a .tar.gz at `dest`.
fn make_dump(dest: &Path) {
    let _ = Command::new("podman").args(["rm", "-f", SRC]).output();
    podman(&["run", "-d", "--name", SRC, "docker.io/library/mongo:7.0"]);
    let mut up = false;
    for _ in 0..60 {
        let ok = Command::new("podman")
            .args(["exec", SRC, "mongosh", "--quiet", "--eval", "db.adminCommand('ping').ok"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            up = true;
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    assert!(up, "source mongo never came up");
    podman(&["exec", SRC, "mongosh", "--quiet", "srcdb", "--eval", "db.items.insertMany([{n:1},{n:2},{n:3}])"]);
    podman(&["exec", SRC, "mongodump", "--quiet", "--db", "srcdb", "--out", "/tmp/dump"]);
    podman(&["exec", SRC, "tar", "-czf", "/tmp/srcdb.tar.gz", "-C", "/tmp", "dump"]);
    podman(&["cp", &format!("{SRC}:/tmp/srcdb.tar.gz"), &dest.display().to_string()]);
    let _ = Command::new("podman").args(["rm", "-f", SRC]).output();
}

#[tokio::test]
async fn a_mongodb_dump_is_restored_on_the_receivers_first_run() {
    if !podman_stack_available() {
        eprintln!("podman/podman-compose not on PATH - skipping");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home.path());
    std::env::set_var("XDG_DATA_HOME", home.path().join("data"));
    let base = tempfile::tempdir().unwrap();
    let project: PathBuf = base.path().join("receiver-mongo-restore");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("index.html"), "<h1>app</h1>").unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "one"]);
    let dump_path = base.path().join("srcdb.tar.gz");
    make_dump(&dump_path);

    let spec = ComposeSpec {
        runtime: Runtime::Python,
        runtime_version: "3.12".into(),
        build_tool: BuildTool::Pip,
        run_command: format!("python -m http.server {PORT}"),
        port: PORT,
        artifact_path: None,
        java_packaging: JavaPackaging::Jar,
        tomcat_version: None,
        // A different database name than the dump's: the restore renames it.
        database: Some(DatabaseSpec { engine: DbEngine::Mongodb, version: "7.0".into(), database: "appdb".into() }),
        db_env_preset: DbEnvPreset::Standard,
        extras: vec![],
        env: vec![],
    };
    let folders = vec![FolderPlanDto {
        path: project.display().to_string(),
        dump: Some(DumpPlanDto { schema: "srcdb".into(), file_path: dump_path.display().to_string(), engine: "mongodb".into() }),
        compose: Some(spec),
    }];

    let (url, _server) = signaling_url(8174).await;
    let (sender, receiver) = (app(), app());
    let sid = session_commands::create_project_session(sender.state::<AppState>(), folders, None).await.unwrap().id;
    let request = SendRequest {
        session_id: sid,
        room_code: "mongoroom".into(),
        signaling_url: url.clone(),
        require_accept: false,
        sender_name: "Alice".into(),
        device_key: None,
        device_name: "Bob".into(),
        since_last: false,
    };
    let r = receiver.clone();
    let u = url.clone();
    let recv = tokio::spawn(async move { commands::receive_snapshot(r.clone(), r.state::<AppState>(), "mongoroom".into(), u).await });
    session_commands::send_project_session(sender.clone(), sender.state::<AppState>(), request).await.expect("send");
    let info = recv.await.unwrap().expect("receive");
    let volume = format!("localsync-db-{}", info.manifest.db_seed_hash);
    let _ = Command::new("podman").args(["volume", "rm", "-f", &volume]).output();

    let work = tempfile::tempdir().unwrap();
    let ran = receiver_session_commands::run_received_session(
        receiver.clone(),
        receiver.state::<AppState>(),
        info.received_session_id.clone(),
        work.path().display().to_string(),
    )
    .await
    .expect("Run with a MongoDB dump");
    let count = Command::new("podman")
        .args(["exec", &format!("{}_mongodb_1", ran.running.session_id), "mongosh", "--quiet"])
        .args(["-u", "localsync", "-p", "localsync_pw", "--authenticationDatabase", "admin", "appdb"])
        .args(["--eval", "db.items.countDocuments()"])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&count.stdout).trim().to_string();
    if out != "3" {
        let logs = Command::new("podman").args(["logs", &format!("{}_mongodb_1", ran.running.session_id)]).output().unwrap();
        let all = format!("{}{}", String::from_utf8_lossy(&logs.stdout), String::from_utf8_lossy(&logs.stderr));
        for line in all.lines().filter(|l| l.contains("LocalSync") || l.contains("initdb") || l.contains("restore") || l.contains("rror")) {
            eprintln!("mongo log: {}", &line[..line.len().min(300)]);
        }
        let mounts = Command::new("podman")
            .args(["inspect", "--format", "{{range .Mounts}}{{.Source}} -> {{.Destination}}\n{{end}}", &format!("{}_mongodb_1", ran.running.session_id)])
            .output()
            .unwrap();
        eprintln!("mounts:\n{}", String::from_utf8_lossy(&mounts.stdout));
        let found = Command::new("find").arg(work.path()).args(["-maxdepth", "4", "-name", "*mongo*"]).output().unwrap();
        eprintln!("files:\n{}", String::from_utf8_lossy(&found.stdout));
        let compose = Command::new("sh").arg("-c").arg(format!("cat $(find {} -maxdepth 3 -name docker-compose.yml | head -1)", work.path().display())).output().unwrap();
        eprintln!("compose:\n{}", String::from_utf8_lossy(&compose.stdout));
    }
    commands::stop_session(receiver.state::<AppState>(), ran.running.session_id.clone()).await.unwrap();
    let _ = Command::new("podman").args(["volume", "rm", "-f", &volume]).output();
    assert_eq!(out, "3", "the dump's 3 documents were restored into appdb (stderr: {})", String::from_utf8_lossy(&count.stderr));
}
