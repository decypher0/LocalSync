//! End-to-end proof of the compose wizard's test run (`compose_wizard::
//! test_run_compose`): a project with no compose file plus a `ComposeSpec` is
//! really booted in Podman, the way the receiver will boot it, before
//! anything is sent - and when it can't boot, the report carries the real
//! reason from the container, not just "exit status 1".
//!
//! These need a real Podman + podman-compose (and network for base images and
//! package registries), so they skip with a printed reason where those are
//! absent. Native Windows can build but not run this crate's test binaries;
//! run them in WSL/Linux.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ls_composegen::{BuildTool, ComposeSpec, DbEnvPreset, Runtime};
use localsync_desktop::commands::{self, FolderPlanDto};
use localsync_desktop::compose_wizard::{self, TestRunReport};
use localsync_desktop::state::AppState;
use tauri::{Listener, Manager};

fn podman_stack_available() -> bool {
    let ok = |bin: &str| Command::new(bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    let available = ok("podman") && ok("podman-compose");
    if !available {
        eprintln!("podman/podman-compose not on PATH - skipping (this test boots real containers)");
    }
    available
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

/// A committed git repo named `name` (its folder name is the project name)
/// containing `files`, and no compose file.
fn make_project(name: &str, files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join(name);
    std::fs::create_dir(&dir).unwrap();
    for (path, contents) in files {
        std::fs::write(dir.join(path), contents).unwrap();
    }
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "initial"]);
    (base, dir)
}

fn spec(runtime: Runtime, version: &str, tool: BuildTool, run_command: &str, port: u16) -> ComposeSpec {
    ComposeSpec {
        runtime,
        runtime_version: version.into(),
        build_tool: tool,
        run_command: run_command.into(),
        port,
        artifact_path: None,
        database: None,
        db_env_preset: DbEnvPreset::Standard,
        extras: vec![],
        env: vec![],
    }
}

fn plan(dir: &Path, spec: ComposeSpec) -> FolderPlanDto {
    FolderPlanDto { path: dir.display().to_string(), dump: None, compose: Some(spec) }
}

async fn test_run(folder: FolderPlanDto) -> (TestRunReport, Vec<String>) {
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    let handle = app.handle().clone();
    let progress = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = progress.clone();
    handle.listen("compose-test-progress", move |event| {
        sink.lock().unwrap().push(event.payload().to_string());
    });
    let report = compose_wizard::test_run_compose(handle.clone(), handle.state::<AppState>(), folder)
        .await
        .expect("the request itself is fine");
    let lines = progress.lock().unwrap().clone();
    (report, lines)
}

/// Names of every container (any state) whose name contains `needle`.
fn containers_matching(needle: &str) -> Vec<String> {
    let out = Command::new("podman").args(["ps", "-a", "--format", "{{.Names}}"]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).lines().filter(|n| n.contains(needle)).map(String::from).collect()
}

fn assert_nothing_left_running(project: &str) {
    let left = containers_matching(project);
    assert!(left.is_empty(), "containers left behind for {project}: {left:?}");
}

fn http_status_line(port: u16) -> Option<String> {
    let mut s = TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse().unwrap(), Duration::from_secs(1)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    s.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n").ok()?;
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    buf.lines().next().map(String::from)
}

async fn wait_for_http_200(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if http_status_line(port).is_some_and(|l| l.contains("200")) {
            return;
        }
        assert!(Instant::now() < deadline, "port {port} never answered HTTP 200");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// What the receiver gets: the generated files at the spec's own port (no
/// test-run override), packaged and verified like a received snapshot.
fn snapshot_as_sent(dir: &Path, spec: &ComposeSpec) -> ls_security::VerifiedSnapshot {
    let folders = vec![ls_snapshot::FolderSpec { path: dir.to_path_buf(), parent_commit: None }];
    let label = ls_snapshot::folder_labels(&folders).remove(0);
    let ctx = ls_composegen::GenerateContext { folder_label: label, dump: None, host_port: None };
    let generated = ls_composegen::generate(spec, &ctx).expect("generate");
    let files = ls_snapshot::GeneratedFiles {
        compose_yaml: generated.compose_yaml,
        files: generated.files.into_iter().map(|g| (g.path, g.contents.into_bytes())).collect(),
    };
    let snapshot = ls_snapshot::create_snapshot_multi_with(&folders, &[], &[Some(files)]).expect("snapshot");
    ls_security::verify(snapshot, &[]).expect("verify")
}

#[tokio::test]
async fn python_project_boots_and_the_same_snapshot_runs_like_a_receiver_would() {
    if !podman_stack_available() {
        return;
    }
    let (_base, dir) = make_project("tr-py-ok", &[("index.html", "<h1>hello from the test run</h1>")]);
    let spec = spec(Runtime::Python, "3.12", BuildTool::Pip, "python -m http.server 18101", 18101);

    let (report, progress) = test_run(plan(&dir, spec.clone())).await;
    assert!(report.ok, "test run failed: {:?}\n{}", report.error, report.output_tail);
    assert_eq!(report.host_port, 18101);
    assert!(report.notes.is_empty(), "{:?}", report.notes);
    assert!(report.output_tail.contains("Serving HTTP"), "the app's own output should be reported: {}", report.output_tail);
    assert!(progress.iter().any(|l| l.contains("Waiting for the app")), "no live progress events: {progress:?}");
    assert_nothing_left_running("tr-py-ok");
    assert!(http_status_line(18101).is_none(), "the test run must not leave the app serving");

    // Now what the receiver does with what is sent: the very same generated
    // files, through the real Run command.
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    let handle = app.handle().clone();
    let verified = snapshot_as_sent(&dir, &spec);
    let id = format!("{}@{}", verified.snapshot().manifest.project_name, verified.snapshot().manifest.git_commit);
    handle.state::<AppState>().verified.lock().unwrap().insert(id.clone(), verified);
    let work = tempfile::tempdir().unwrap();
    let session = commands::run_snapshot(handle.clone(), handle.state::<AppState>(), id, work.path().display().to_string())
        .await
        .expect("the receiver's Run of the same snapshot should work");
    assert!(session.service_ports.iter().any(|(svc, p)| svc == "app" && p.starts_with("18101:")), "{:?}", session.service_ports);
    wait_for_http_200(18101).await;
    commands::stop_session(handle.state::<AppState>(), session.session_id).await.unwrap();
    assert_nothing_left_running("tr-py-ok");
}

/// The project ships its own `Dockerfile` (but no docker-compose.yml, which
/// is what triggers the wizard). The compose file must name the generated
/// Dockerfile explicitly, or Podman would silently build the project's own
/// file - here one whose container fails on purpose, so building it can't pass.
#[tokio::test]
async fn a_projects_own_dockerfile_is_never_built_instead_of_the_generated_one() {
    if !podman_stack_available() {
        return;
    }
    let stale = "FROM docker.io/library/busybox
CMD [\"sh\", \"-c\", \"echo STALE_DOCKERFILE_WAS_BUILT; exit 1\"]
";
    let (_base, dir) = make_project(
        "tr-own-dockerfile",
        &[("Dockerfile", stale), ("index.html", "<h1>generated one</h1>")],
    );
    let spec = spec(Runtime::Python, "3.12", BuildTool::Pip, "python -m http.server 18106", 18106);

    let (report, _) = test_run(plan(&dir, spec.clone())).await;
    assert!(report.ok, "test run failed: {:?}
{}", report.error, report.output_tail);
    assert!(!report.output_tail.contains("STALE_DOCKERFILE_WAS_BUILT"), "{}", report.output_tail);
    assert!(report.output_tail.contains("Serving HTTP"), "{}", report.output_tail);
    assert_nothing_left_running("tr-own-dockerfile");

    // And what is sent carries both files side by side, the project's own one untouched.
    let verified = snapshot_as_sent(&dir, &spec);
    let payload = &verified.snapshot().payload;
    let decoded = zstd::stream::decode_all(&payload[..]).unwrap();
    let mut names = Vec::new();
    for entry in tar::Archive::new(&decoded[..]).entries().unwrap() {
        names.push(entry.unwrap().path().unwrap().display().to_string());
    }
    assert!(names.iter().any(|n| n.ends_with("source/Dockerfile")), "{names:?}");
    assert!(names.iter().any(|n| n.ends_with("source/Dockerfile.localsync")), "{names:?}");
}

#[tokio::test]
async fn node_project_boots() {
    if !podman_stack_available() {
        return;
    }
    let (_base, dir) = make_project(
        "tr-node-ok",
        &[
            ("package.json", r#"{"name":"tr-node-ok","version":"1.0.0","scripts":{"start":"node server.js"}}"#),
            (
                "server.js",
                "require('http').createServer((q, r) => r.end('ok')).listen(18103, '0.0.0.0', () => console.log('node listening'));",
            ),
        ],
    );
    let (report, _) = test_run(plan(&dir, spec(Runtime::Node, "20", BuildTool::Npm, "node server.js", 18103))).await;
    assert!(report.ok, "test run failed: {:?}\n{}", report.error, report.output_tail);
    assert!(report.output_tail.contains("node listening"), "{}", report.output_tail);
    assert_nothing_left_running("tr-node-ok");
}

#[tokio::test]
async fn a_crashing_app_reports_the_containers_own_error_not_just_an_exit_status() {
    if !podman_stack_available() {
        return;
    }
    let (_base, dir) = make_project("tr-py-crash", &[("readme.txt", "no server here")]);
    let (report, _) = test_run(plan(&dir, spec(Runtime::Python, "3.12", BuildTool::Pip, "python nosuchfile.py", 18104))).await;
    assert!(!report.ok);
    let error = report.error.clone().expect("a failed run has an error");
    let all = format!("{error}\n{}", report.output_tail);
    eprintln!("--- crash report ---
{all}");
    assert!(all.contains("can't open file") && all.contains("nosuchfile.py"), "the real reason is missing: {all}");
    assert!(all.contains("No such file"), "{all}");
    assert_nothing_left_running("tr-py-crash");
}

#[tokio::test]
async fn a_failed_build_reports_the_real_build_error() {
    if !podman_stack_available() {
        return;
    }
    let (_base, dir) = make_project(
        "tr-node-build-fails",
        &[
            ("package.json", r#"{"name":"x","version":"1.0.0","dependencies":{"localsync-no-such-package-xyz":"1.0.0"}}"#),
            ("server.js", "console.log('never gets here')"),
        ],
    );
    let (report, _) = test_run(plan(&dir, spec(Runtime::Node, "20", BuildTool::Npm, "node server.js", 18105))).await;
    assert!(!report.ok);
    let all = format!("{}\n{}", report.error.clone().unwrap_or_default(), report.output_tail);
    eprintln!("--- build failure report ---
{all}");
    assert!(all.contains("localsync-no-such-package-xyz"), "the npm error naming the package is missing: {all}");
    assert!(all.contains("404") || all.contains("E404") || all.contains("not found"), "npm's real reason is missing: {all}");
    assert_nothing_left_running("tr-node-build-fails");
}

#[tokio::test]
async fn a_busy_port_does_not_fail_the_test_run_and_is_explained() {
    if !podman_stack_available() {
        return;
    }
    // The sender's own dev server, already on the app's port.
    let dev_server = TcpListener::bind("0.0.0.0:18106").expect("port 18106 should be free for this test");
    let (_base, dir) = make_project("tr-py-busy", &[("index.html", "hi")]);
    let (report, _) = test_run(plan(&dir, spec(Runtime::Python, "3.12", BuildTool::Pip, "python -m http.server 18106", 18106))).await;
    assert!(report.ok, "test run failed: {:?}\n{}", report.error, report.output_tail);
    assert_ne!(report.host_port, 18106);
    assert!(report.notes.iter().any(|n| n.contains("18106") && n.contains(&report.host_port.to_string())), "{:?}", report.notes);
    drop(dev_server);
    assert_nothing_left_running("tr-py-busy");
}

#[tokio::test]
async fn an_app_that_never_listens_times_out_with_a_useful_message() {
    if !podman_stack_available() {
        return;
    }
    let (_base, dir) = make_project("tr-py-silent", &[("readme.txt", "x")]);
    // Runs fine but never opens its port.
    let folder = plan(&dir, spec(Runtime::Python, "3.12", BuildTool::Pip, "python -c \"import time; print('idle'); time.sleep(600)\"", 18107));
    let app = tauri::test::mock_app();
    let report = compose_wizard::run_compose_test(app.handle().clone(), folder, Duration::from_secs(8)).await.unwrap();
    assert!(!report.ok);
    let error = report.error.unwrap_or_default();
    eprintln!("--- timeout report ---
{error}");
    assert!(error.contains("nothing accepted connections on port") && error.contains("0.0.0.0"), "{error}");
    assert!(error.contains("idle"), "the app's own output should be included: {error}");
    assert_nothing_left_running("tr-py-silent");
}

#[tokio::test]
async fn a_request_without_compose_settings_is_an_error_not_a_failed_boot() {
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    let handle = app.handle().clone();
    let folder = FolderPlanDto { path: "/nowhere".into(), dump: None, compose: None };
    assert!(compose_wizard::test_run_compose(handle.clone(), handle.state::<AppState>(), folder).await.is_err());

    // An invalid spec (privileged port) is rejected before anything runs.
    let bad = plan(Path::new("/nowhere"), spec(Runtime::Python, "3.12", BuildTool::Pip, "python app.py", 80));
    let err = compose_wizard::test_run_compose(handle.clone(), handle.state::<AppState>(), bad).await.unwrap_err();
    assert!(err.contains("port"), "{err}");
}
