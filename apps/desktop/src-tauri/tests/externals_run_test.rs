//! End-to-end: a project whose application.properties points at a file
//! outside the project, by an absolute Windows-style path with a space in it
//! (the real multitenancy case). The file is bundled under `external/<id>/`,
//! mounted into the container, LocalSync's copy of the config is rewritten to
//! the in-container path, and the app really serves the file's content - on
//! the sender's test run and on the receiver's Run of what is sent. The
//! person's files are byte-for-byte unchanged afterwards.
//!
//! Needs a real Podman + podman-compose (skips with a printed reason where
//! they're absent) and the python:3.12-slim base image. Run in WSL/Linux:
//! native Windows can't start this crate's test binaries.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use localsync_desktop::commands::{self, FolderPlanDto};
use localsync_desktop::externals::{self, ExternalStatus};
use localsync_desktop::{compose_wizard, state::AppState};
use ls_composegen::{BuildTool, ComposeSpec, DbEnvPreset, Runtime};
use tauri::Manager;

const PORT: u16 = 18131;
const WINDOWS_VALUE: &str = "C:/Users/Some One/LocalSync Fixture/configuration/tenants.xml";

const APP_PY: &str = r##"import http.server
props = {}
for line in open("application.properties"):
    line = line.strip()
    if line and not line.startswith("#") and "=" in line:
        k, v = line.split("=", 1)
        props[k.strip()] = v.strip()
path = props["tenants.file"]
print("reading tenants from", path, flush=True)
body = open(path, "rb").read()  # fails at start-up if the path is wrong

class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

http.server.HTTPServer(("0.0.0.0", 18131), H).serve_forever()
"##;

fn podman_available() -> bool {
    let ok = |bin: &str| Command::new(bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    ok("podman") && ok("podman-compose")
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// Every file under `dir` (but .git) with its bytes.
fn files_of(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            if e.file_name() == ".git" {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            } else {
                out.push((e.path(), std::fs::read(e.path()).unwrap()));
            }
        }
    }
    out.sort();
    out
}

fn http_body(port: u16) -> Option<String> {
    let mut s = TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse().unwrap(), Duration::from_secs(1)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    s.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n").ok()?;
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    let (head, body) = buf.split_once("\r\n\r\n")?;
    head.contains(" 200 ").then(|| body.to_string())
}

async fn wait_for_body(port: u16) -> String {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Some(b) = http_body(port) {
            return b;
        }
        assert!(Instant::now() < deadline, "port {port} never served the file");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn spec() -> ComposeSpec {
    ComposeSpec {
        runtime: Runtime::Python,
        runtime_version: "3.12".into(),
        build_tool: BuildTool::Pip,
        run_command: "python app.py".into(),
        port: PORT,
        artifact_path: None,
        database: None,
        db_env_preset: DbEnvPreset::Standard,
        extras: vec![],
        env: vec![],
        java_packaging: Default::default(),
        tomcat_version: None,
    }
}

#[tokio::test]
async fn an_outside_file_named_by_a_windows_path_is_sent_mounted_and_served() {
    if !podman_available() {
        eprintln!("podman/podman-compose not on PATH - skipping (this test boots real containers)");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("ext-fixture");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("app.py"), APP_PY).unwrap();
    std::fs::write(
        project.join("application.properties"),
        format!("# another developer's copy:\n#tenants.file=/home/someone-else/tenants.xml\ntenants.file={WINDOWS_VALUE}\n"),
    )
    .unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "fixture"]);
    let outside = base.path().join("outside dir").join("tenants.xml");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    let tenants = "<companies><company><schema>EVLINS</schema></company></companies>\n";
    std::fs::write(&outside, tenants).unwrap();
    let project_before = files_of(&project);
    let outside_before = std::fs::read(&outside).unwrap();

    // On Linux a C:/ path can't exist here: found, but missing.
    let found = externals::detect_external_refs(project.display().to_string()).await.unwrap();
    assert_eq!(found.len(), 1, "the commented line is ignored: {found:#?}");
    assert_eq!((found[0].key.as_str(), found[0].value.as_str()), ("tenants.file", WINDOWS_VALUE));
    assert_eq!(found[0].status, ExternalStatus::OutsideMissing);

    // "Add a file..." with the same key: where the file really is here.
    let ext = externals::describe_external(project.display().to_string(), "tenants.file".into(), outside.display().to_string()).await.unwrap();
    assert_eq!(ext.status, ExternalStatus::OutsideExists);
    assert_eq!(ext.value, WINDOWS_VALUE);
    assert!(ext.rewritable);

    let fingerprint = externals::externals_fingerprint(std::slice::from_ref(&ext)).unwrap();

    // 1. The sender's test run, through the real FolderPlanDto path.
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    let handle = app.handle().clone();
    let folder = FolderPlanDto { path: project.display().to_string(), dump: None, compose: Some(spec()), externals: vec![ext.clone()] };
    let report = compose_wizard::test_run_compose(handle.clone(), handle.state::<AppState>(), folder).await.expect("request ok");
    assert!(report.ok, "test run failed: {:?}\n{}", report.error, report.output_tail);
    assert!(report.output_tail.contains(&ext.container_path), "the app read the in-container path: {}", report.output_tail);

    // 2. What the receiver gets and runs.
    let folders = vec![ls_snapshot::FolderSpec { path: project.clone(), parent_commit: None }];
    let label = ls_snapshot::folder_labels(&folders).remove(0);
    let mut generated = ls_composegen::generate(&spec(), &ls_composegen::GenerateContext { folder_label: label, dump: None, host_port: None }).unwrap();
    externals::apply_to_generated(&project, std::slice::from_ref(&ext), &mut generated).unwrap();
    let files = ls_snapshot::GeneratedFiles {
        compose_yaml: generated.compose_yaml,
        files: generated.files.into_iter().map(|g| (g.path, g.contents.into_bytes())).collect(),
        externals: externals::bundle_entries(std::slice::from_ref(&ext)),
    };
    let snapshot = ls_snapshot::create_snapshot_multi_with(&folders, &[], &[Some(files)]).unwrap();
    assert_eq!(snapshot.manifest.externals.len(), 1);
    assert_eq!(snapshot.manifest.externals[0].original_value, WINDOWS_VALUE);
    assert!(snapshot.manifest.externals[0].bundle_path.starts_with("external/"));
    let verified = ls_security::verify(snapshot, &[]).unwrap();
    let id = format!("{}@{}", verified.snapshot().manifest.project_name, verified.snapshot().manifest.git_commit);
    handle.state::<AppState>().verified.lock().unwrap().insert(id.clone(), verified);
    let work = tempfile::tempdir().unwrap();
    let session = commands::run_snapshot(handle.clone(), handle.state::<AppState>(), id, work.path().display().to_string())
        .await
        .expect("the receiver's Run works");
    let served = wait_for_body(PORT).await;
    commands::stop_session(handle.state::<AppState>(), session.session_id).await.unwrap();
    assert_eq!(served, tenants, "the container serves the outside file's content");

    // The receiver's own copy of the config was rewritten; the sender's wasn't.
    let unpacked: Vec<_> = files_of(work.path()).into_iter().filter(|(p, _)| p.ends_with("source/application.properties")).collect();
    assert_eq!(unpacked.len(), 1);
    let received_config = String::from_utf8(unpacked[0].1.clone()).unwrap();
    assert!(received_config.contains(&format!("tenants.file={}", ext.container_path)), "{received_config}");
    assert!(received_config.contains("#tenants.file=/home/someone-else/tenants.xml"), "comments kept: {received_config}");

    assert_eq!(files_of(&project), project_before, "the project's files are byte-for-byte unchanged");
    assert_eq!(std::fs::read(&outside).unwrap(), outside_before, "the outside file is unchanged");

    // A modified external file changes the fingerprint.
    assert_eq!(externals::externals_fingerprint(std::slice::from_ref(&ext)).unwrap(), fingerprint);
    std::fs::write(&outside, "<companies/>\n").unwrap();
    assert_ne!(externals::externals_fingerprint(std::slice::from_ref(&ext)).unwrap(), fingerprint);
}
