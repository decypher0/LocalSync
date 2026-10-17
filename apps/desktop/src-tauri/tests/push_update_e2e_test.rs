//! Push update end to end on one machine, with real Podman: a project whose
//! config points at a file outside it (the multitenancy case) and a MySQL
//! dump is sent and run; the sender then edits a tracked file, adds a new
//! file and edits the external file. Changes lists all three, the
//! compatibility check test-runs the update, and the update lands on the
//! receiver's same session: Apply & restart runs the new version - the new
//! external file's content included - on the database the receiver already
//! had. Then a broken build: the check blocks it, Send anyway still delivers
//! it, it fails to come up, and Roll back brings the previous version back.
//!
//! The dump keep/replace question is covered by receiver_update_apply_test.
//! Network-free like that test: the sender session's real artifact (what a
//! P2P send carries) goes to the receiver's real `finalize_received_snapshot`.
//! Skips where podman/podman-compose are absent. One `#[tokio::test]`: it
//! sets the process-wide `HOME` and `LOCALSYNC_HEALTH_TIMEOUT_SECS`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use localsync_desktop::commands::{self, DumpPlanDto, FolderPlanDto, IncomingSnapshotInfo};
use localsync_desktop::externals::{self, ExternalStatus};
use localsync_desktop::receiver_session_commands::{self, RunReceivedResult};
use localsync_desktop::state::{AppState, CloudDelivery};
use localsync_desktop::{compat, session_commands};
use ls_composegen::{BuildTool, ComposeSpec, DatabaseSpec, DbEngine, DbEnvPreset, JavaPackaging, Runtime};
use tauri::Manager;

type Handle = tauri::AppHandle<tauri::test::MockRuntime>;

const PORT: u16 = 18181;

/// Serves index.html and the tenants file the properties point at (read at
/// start-up, so a wrong path fails the start).
const APP_PY: &str = r##"import http.server
props = {}
for line in open("application.properties"):
    line = line.strip()
    if line and not line.startswith("#") and "=" in line:
        k, v = line.split("=", 1)
        props[k.strip()] = v.strip()
body = (open("index.html").read() + "|" + open(props["tenants.file"]).read()).encode()

class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

http.server.HTTPServer(("0.0.0.0", 18181), H).serve_forever()
"##;

fn podman_stack_available() -> bool {
    let ok = |bin: &str| Command::new(bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    ok("podman") && ok("podman-compose")
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git").args(["-c", "user.name=T", "-c", "user.email=t@example.com"]).args(args).current_dir(dir).status().unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

fn app() -> Handle {
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    app.handle().clone()
}

fn http_body() -> Option<String> {
    let mut s = TcpStream::connect_timeout(&format!("127.0.0.1:{PORT}").parse().unwrap(), Duration::from_secs(1)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    s.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n").ok()?;
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    let (head, body) = buf.split_once("\r\n\r\n")?;
    head.contains(" 200 ").then(|| body.to_string())
}

async fn wait_for_body(expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut last = None;
    while Instant::now() < deadline {
        last = http_body();
        if last.as_deref() == Some(expected) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("port {PORT} never served {expected:?} (last: {last:?})");
}

fn mysql(project: &str, sql: &str) -> String {
    let out = Command::new("podman")
        .args(["exec", &format!("{project}_mysql_1"), "mysql", "-uroot", "-plocalsync_root_pw", "-N", "-e", sql, "appdb"])
        .output()
        .unwrap();
    assert!(out.status.success(), "mysql: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).split_whitespace().collect::<Vec<_>>().join(",")
}

/// The sender session's current artifact, received - and recorded on the
/// sender as delivered to `cloud:r@example.com`, so Push update targets it.
async fn send(sender: &Handle, receiver: &Handle, sid: &str) -> IncomingSnapshotInfo {
    let (_, artifact) = session_commands::cloud_drop_payload(&sender.state::<AppState>(), sid).await.expect("the sender's artifact");
    let delivery = CloudDelivery {
        session_id: sid.to_string(),
        snapshot_id: artifact.snapshot_id.clone(),
        commits: artifact.commits.clone(),
        bytes: artifact.bytes.len() as u64,
        upload_ms: 1000,
    };
    session_commands::record_cloud_delivery(&sender.state::<AppState>(), &delivery, "r@example.com").expect("recorded");
    let snapshot: ls_snapshot::Snapshot = serde_json::from_slice(&artifact.bytes).unwrap();
    commands::finalize_received_snapshot(&receiver.state::<AppState>(), snapshot).expect("verified and filed")
}

async fn apply(receiver: &Handle, id: &str) -> Result<RunReceivedResult, String> {
    receiver_session_commands::apply_received_update(receiver.clone(), receiver.state::<AppState>(), id.to_string(), None).await
}

async fn compat_report(sender: &Handle, sid: &str) -> compat::CompatReport {
    let session = sender.state::<AppState>().project_sessions.lock().unwrap().get(sid).cloned().unwrap();
    compat::check_session(sender.clone(), &session, Duration::from_secs(180)).await
}

#[tokio::test]
async fn push_update_carries_edits_new_files_and_external_files_and_a_broken_build_is_blocked_then_rolled_back() {
    if !podman_stack_available() {
        eprintln!("podman/podman-compose not on PATH - skipping");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home.path());
    std::env::set_var("XDG_DATA_HOME", home.path().join("data"));
    std::env::set_var("LOCALSYNC_HEALTH_TIMEOUT_SECS", "15");
    let base = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let (sender, receiver) = (app(), app());

    // The tenant list lives outside the project, at a path with a space in it
    // (under /var: /tmp isn't one of the roots the detector treats as a host path).
    let outside_root = tempfile::Builder::new().prefix("ls-e2e-").tempdir_in("/var/tmp").unwrap();
    let outside = outside_root.path().join("Some Company").join("configuration");
    std::fs::create_dir_all(&outside).unwrap();
    let tenants = outside.join("company_configuration.xml");
    std::fs::write(&tenants, "<companies>EVLINS</companies>").unwrap();

    let project = base.path().join("tenant-app");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("app.py"), APP_PY).unwrap();
    std::fs::write(project.join("requirements.txt"), "").unwrap();
    std::fs::write(project.join("index.html"), "v1").unwrap();
    std::fs::write(project.join("application.properties"), format!("#tenants.file=/home/someone-else/tenants.xml\ntenants.file={}\n", tenants.display())).unwrap();
    git(&project, &["init", "-q"]);
    commit_all(&project, "one");
    let dump_path = base.path().join("appdb.sql");
    std::fs::write(&dump_path, "CREATE TABLE `items` (`id` int NOT NULL, PRIMARY KEY (`id`));\nINSERT INTO `items` VALUES (1),(2);\n").unwrap();

    // Setup: the detector finds the outside file; it is ticked to travel.
    let found = externals::detect_external_refs(project.display().to_string()).await.unwrap();
    assert_eq!(found.len(), 1, "only the active line: {found:?}");
    assert_eq!(found[0].status, ExternalStatus::OutsideExists);
    let spec = ComposeSpec {
        runtime: Runtime::Python,
        runtime_version: "3.12".into(),
        build_tool: BuildTool::Pip,
        run_command: "python app.py".into(),
        port: PORT,
        artifact_path: None,
        java_packaging: JavaPackaging::Jar,
        tomcat_version: None,
        database: Some(DatabaseSpec { engine: DbEngine::Mysql, version: "8.0".into(), database: "appdb".into() }),
        db_env_preset: DbEnvPreset::Standard,
        extras: vec![],
        env: vec![],
    };
    let folders = vec![FolderPlanDto {
        path: project.display().to_string(),
        dump: Some(DumpPlanDto { schema: "appdb".into(), file_path: dump_path.display().to_string(), engine: "mysql".into() }),
        compose: Some(spec),
        externals: found,
    }];
    let sid = session_commands::create_project_session(sender.state::<AppState>(), folders, None).await.unwrap().id;
    let originals = (std::fs::read(project.join("application.properties")).unwrap(), std::fs::read(&tenants).unwrap());

    // ================= v1: sent and run =================
    let v1 = send(&sender, &receiver, &sid).await;
    let id = v1.received_session_id.clone();
    let volume = format!("localsync-db-{}", v1.manifest.db_seed_hash);
    let _ = Command::new("podman").args(["volume", "rm", "-f", &volume]).output();
    let ran = receiver_session_commands::run_received_session(receiver.clone(), receiver.state::<AppState>(), id.clone(), work.path().display().to_string())
        .await
        .expect("v1 runs");
    wait_for_body("v1|<companies>EVLINS</companies>").await;
    mysql(&ran.running.session_id, "INSERT INTO items VALUES (99)"); // the receiver's own data

    let unchanged = session_commands::project_changes(sender.state::<AppState>(), sid.clone()).await.unwrap();
    assert!(!unchanged.status.changed, "nothing to push right after the send");
    assert_eq!(unchanged.status.last_device_key.as_deref(), Some("cloud:r@example.com"));

    // ================= the sender edits, adds, and edits the external file =================
    std::fs::write(project.join("index.html"), "v2").unwrap();
    std::fs::write(project.join("notes.txt"), "new file").unwrap();
    std::fs::write(&tenants, "<companies>EVLINS,HAMINS</companies>").unwrap();

    let changes = session_commands::project_changes(sender.state::<AppState>(), sid.clone()).await.unwrap();
    assert!(changes.status.changed, "Push update is enabled");
    let f = &changes.folders[0];
    let lines: Vec<(&str, &str, bool)> = f.files.iter().map(|x| (x.path.as_str(), x.change_type.as_str(), x.new_file)).collect();
    assert!(lines.contains(&("index.html", "modified", false)), "{lines:?}");
    assert!(lines.contains(&("notes.txt", "added", true)), "{lines:?}");
    let ext: Vec<(&str, &str)> = f.externals.iter().map(|x| (x.name.as_str(), x.change_type.as_str())).collect();
    assert_eq!(ext, [("company_configuration.xml", "modified")]);

    // The compatibility check test-runs it: the external file changed.
    let report = compat_report(&sender, &sid).await;
    assert!(report.ok, "compatible: {:?}", report.problems);
    let test = report.test.expect("a test run, because a file from outside the project changed");
    assert!(test.ok && !test.cached, "{test:?}");

    // ================= v2: Push update, lands on the same session, applied =================
    let v2 = send(&sender, &receiver, &sid).await;
    assert_eq!(v2.received_session_id, id, "the update lands on the receiver's existing session");
    let applied = apply(&receiver, &id).await.expect("Apply & restart");
    wait_for_body("v2|<companies>EVLINS,HAMINS</companies>").await;
    assert_eq!(mysql(&applied.running.session_id, "SELECT id FROM items ORDER BY id"), "1,2,99", "the receiver's database was kept");
    let after = session_commands::project_changes(sender.state::<AppState>(), sid.clone()).await.unwrap();
    assert!(!after.status.changed, "up to date after the push: {:?}", after.folders);

    // ================= a broken build: blocked, sent anyway, rolled back =================
    std::fs::write(project.join("app.py"), format!("{APP_PY}\ndef broken(:\n")).unwrap();
    std::fs::write(project.join("requirements.txt"), "\n").unwrap();
    commit_all(&project, "broken");
    let blocked = compat_report(&sender, &sid).await;
    assert!(!blocked.ok, "the broken build is not compatible: {blocked:?}");
    let failed = blocked.test.expect("tested");
    assert!(!failed.ok && failed.output_tail.contains("SyntaxError"), "{failed:?}");

    // "Send anyway": the same send, just past the check.
    let v3 = send(&sender, &receiver, &sid).await;
    assert_eq!(v3.received_session_id, id);
    let err = apply(&receiver, &id).await.err().expect("a version that can't start must not be applied");
    eprintln!("--- apply error ---\n{err}");
    let back = receiver_session_commands::rollback_received_update(receiver.state::<AppState>(), id.clone()).await.expect("roll back");
    assert!(back.running, "the previous version is running again");
    assert_eq!(back.git_commit, v2.manifest.git_commit);
    wait_for_body("v2|<companies>EVLINS,HAMINS</companies>").await;

    receiver_session_commands::stop_received_session(receiver.state::<AppState>(), id.clone()).await.expect("stop");
    let _ = Command::new("podman").args(["volume", "rm", "-f", &volume]).output();
    assert_eq!(
        (std::fs::read(project.join("application.properties")).unwrap(), std::fs::read(&tenants).unwrap().len()),
        (originals.0, "<companies>EVLINS,HAMINS</companies>".len()),
        "the sender's config was never rewritten; the external file holds only the sender's own edit"
    );
    let _ = originals.1;
}
