//! Backend of the compose wizard: the commands the form talks to, and the
//! "test run" that boots what the wizard generated on the *sender's* machine
//! before anything is sent.
//!
//! The test run goes through exactly the receiver's path
//! (`ls_security::verify` -> `ls_containers::run_snapshot`, same sandbox
//! policy, same `podman-compose up -d --build`), so a project that boots here
//! boots there. Its whole point is the failure case: when the boot doesn't
//! work, the real reason (the failed `npm ci`, the Python traceback, the port
//! already in use) comes back in the report, so the person can go back and fix
//! their answers instead of sending something unverified.

use std::collections::VecDeque;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ls_composegen::{ComposeSpec, DumpInfo, FieldError, GenerateContext, Generated};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::commands::{generated_files_for, plan_to_snapshot_inputs, FolderPlanDto};
use crate::state::AppState;

/// How long to wait, after `up`, for the app's host port to start serving.
/// `LOCALSYNC_TESTRUN_TIMEOUT_SECS` overrides it (slow databases, tests).
const DEFAULT_WAIT_SECS: u64 = 120;
const PROGRESS_EVENT: &str = "compose-test-progress";
/// Lines of output kept for the report (and shown after a failure).
const KEPT_LINES: usize = 100;

// ---------- simple commands ----------

#[tauri::command]
pub fn compose_catalog() -> ls_composegen::catalog::Catalog {
    ls_composegen::catalog::catalog()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectInspection {
    /// `<folder>/docker-compose.yml` exists - exactly, and only, the file the
    /// snapshot bundler looks for.
    pub has_compose: bool,
    pub is_git_repo: bool,
    /// HEAD resolves (a snapshot is built from a commit).
    pub has_commits: bool,
}

#[tauri::command]
pub async fn inspect_project(folder_path: String) -> ProjectInspection {
    tauri::async_runtime::spawn_blocking(move || inspect(Path::new(&folder_path)))
        .await
        .unwrap_or(ProjectInspection { has_compose: false, is_git_repo: false, has_commits: false })
}

fn inspect(folder: &Path) -> ProjectInspection {
    // A snapshot needs the folder to be a repo root of its own, so ".git"
    // there - not "some parent is a repo" - is what "is a git repo" means.
    let is_git_repo = folder.join(".git").exists();
    ProjectInspection {
        has_compose: folder.join("docker-compose.yml").is_file(),
        is_git_repo,
        has_commits: is_git_repo && ls_snapshot::head_commit(folder).is_ok(),
    }
}

/// Empty = valid.
#[tauri::command]
pub fn validate_compose_spec(spec: ComposeSpec) -> Vec<FieldError> {
    ls_composegen::validate(&spec).err().unwrap_or_default()
}

/// What would be generated, for the wizard's preview. The error string is the
/// `GenerateError`'s own message.
#[tauri::command]
pub fn preview_compose(spec: ComposeSpec, folder_label: String, dump: Option<DumpInfo>) -> Result<Generated, String> {
    ls_composegen::generate(&spec, &GenerateContext { folder_label, dump, host_port: None }).map_err(|e| e.to_string())
}

// ---------- test run ----------

#[derive(Debug, Clone, Serialize)]
pub struct TestRunReport {
    pub ok: bool,
    /// Why it failed - the real reason, in the container's own words.
    pub error: Option<String>,
    /// The end of what the containers printed (build output on a failed
    /// build, the app's own output otherwise).
    pub output_tail: String,
    /// The host port the test run published the app on (not necessarily the
    /// spec's port - see `notes`).
    pub host_port: u16,
    pub notes: Vec<String>,
}

#[derive(Clone, Serialize)]
struct TestProgress {
    line: String,
}

fn emit_line<R: tauri::Runtime>(app: &AppHandle<R>, line: impl Into<String>) {
    let _ = app.emit(PROGRESS_EVENT, TestProgress { line: line.into() });
}

/// Boots the folder's generated compose project in Podman, waits for the app
/// to actually serve, and always tears it down again. See the module docs.
///
/// `Err` only for a bad request (no compose spec, an invalid spec, an
/// unreadable dump file); a boot that fails is `Ok` with `ok: false`.
// Generic over the runtime for the same reason the other commands are: tests
// call it with a mock_app()'s handle.
#[tauri::command]
pub async fn test_run_compose<R: tauri::Runtime>(
    app: AppHandle<R>,
    // Kept in the signature the frontend contract names; the test run holds
    // its own session locally and is always gone by the time it returns, so
    // nothing is registered in the shared session map.
    _state: State<'_, AppState>,
    folder: FolderPlanDto,
) -> Result<TestRunReport, String> {
    run_compose_test(app, folder, wait_timeout()).await
}

/// [`test_run_compose`] with an explicit wait for the app to start serving.
pub async fn run_compose_test<R: tauri::Runtime>(
    app: AppHandle<R>,
    folder: FolderPlanDto,
    wait: Duration,
) -> Result<TestRunReport, String> {
    let spec = folder.compose.clone().ok_or("this folder has no compose settings to test")?;
    if let Err(errors) = ls_composegen::validate(&spec) {
        let joined: Vec<String> = errors.iter().map(|e| format!("{}: {}", e.field, e.message)).collect();
        return Err(format!("invalid compose settings - {}", joined.join("; ")));
    }

    let (host_port, mut notes) = choose_host_port(spec.port);
    let mut report = TestRunReport { ok: false, error: None, output_tail: String::new(), host_port, notes: vec![] };

    let folders = vec![folder];
    let (specs, dumps) = plan_to_snapshot_inputs(&folders, &[])?;
    let generated = generated_files_for(&folders, &specs, Some(host_port))?;

    emit_line(&app, "Packaging the project the way it will be sent...");
    let snapshot = match tauri::async_runtime::spawn_blocking(move || ls_snapshot::create_snapshot_multi_with(&specs, &dumps, &generated)).await {
        Ok(Ok(snapshot)) => snapshot,
        Ok(Err(e)) => return Ok(fail(report, notes, format!("couldn't package the project: {e:#}"), String::new())),
        Err(e) => return Ok(fail(report, notes, format!("packaging task panicked: {e}"), String::new())),
    };
    let verified = match ls_security::verify(snapshot, &[]) {
        Ok(v) => v,
        Err(e) => return Ok(fail(report, notes, format!("the packaged project failed verification: {e:#}"), String::new())),
    };

    let work_dir = fresh_work_dir();
    let outcome = boot_and_wait(&app, &verified, &work_dir, host_port, wait, &mut notes).await;
    if let Err(e) = std::fs::remove_dir_all(&work_dir) {
        // Not the person's problem, but worth knowing about.
        log::warn!("test run: couldn't remove {}: {e}", work_dir.display());
    }

    report.notes = notes;
    match outcome {
        Ok(app_output) => {
            report.ok = true;
            report.output_tail = app_output;
            emit_line(&app, format!("The app is up on port {host_port}."));
        }
        Err((error, output)) => {
            report.error = Some(error);
            report.output_tail = output;
        }
    }
    Ok(report)
}

fn fail(mut report: TestRunReport, notes: Vec<String>, error: String, output_tail: String) -> TestRunReport {
    report.notes = notes;
    report.error = Some(error);
    report.output_tail = output_tail;
    report
}

/// `Ok(app output)` when the app served; `Err((reason, output))` otherwise.
/// Tears the project down in every case.
async fn boot_and_wait<R: tauri::Runtime>(
    app: &AppHandle<R>,
    verified: &ls_security::VerifiedSnapshot,
    work_dir: &Path,
    host_port: u16,
    timeout: Duration,
    notes: &mut Vec<String>,
) -> Result<String, (String, String)> {
    let lines: Arc<Mutex<VecDeque<String>>> = Arc::default();
    let tail_task = tauri::async_runtime::spawn(tail_provisioning_log(app.clone(), lines.clone()));
    emit_line(app, "Building and starting the containers (the first run downloads images and can take a few minutes)...");
    let started = ls_containers::run_snapshot(verified, work_dir).await;
    // Let the tailer see the last lines the run wrote before it goes away.
    tokio::time::sleep(Duration::from_millis(400)).await;
    tail_task.abort();
    let log_tail = lines.lock().map(|l| l.iter().cloned().collect::<Vec<_>>().join("\n")).unwrap_or_default();

    let session = match started {
        // run_snapshot has already torn down whatever it started.
        Err(e) => return Err((format!("{e:#}"), log_tail)),
        Ok(session) => session,
    };

    // podman-compose exits 0 even when an image build failed or a container
    // couldn't start, so `run_snapshot` returning Ok doesn't mean it's up.
    // What went wrong is in the output it printed (`log_tail`).
    let unstarted = ls_containers::services_not_started(&session).await;
    if !unstarted.is_empty() {
        if let Err(e) = ls_containers::stop_session(&session).await {
            notes.push(format!("Cleaning up the test containers failed ({e:#}); check `podman ps -a` for leftovers."));
        }
        let names = unstarted.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", ");
        let reason = format!(
            "{names} could not be started - usually its image failed to build (the build output is below) or a port it needs is already in use.\n\n{log_tail}"
        );
        return Err((reason, log_tail));
    }

    emit_line(app, format!("Containers started. Waiting for the app on port {host_port}..."));
    let app_service = ls_composegen::spec::APP_SERVICE;
    let waited = wait_for_port(host_port, timeout, Duration::from_millis(500), || async {
        match ls_containers::service_state(&session, app_service).await {
            Ok(ls_containers::ServiceState::Exited(code)) => {
                Some(format!("The app stopped with exit code {code} right after starting."))
            }
            Ok(ls_containers::ServiceState::Missing | ls_containers::ServiceState::NotStarted) => {
                Some("The app's container is not running.".to_string())
            }
            _ => None,
        }
    })
    .await;

    // Whatever the app printed is the real explanation of a failure (and a
    // useful confirmation of success).
    let app_output = ls_containers::service_logs(&session, app_service, KEPT_LINES).await.unwrap_or_default();

    if let Err(e) = ls_containers::stop_session(&session).await {
        notes.push(format!("Cleaning up the test containers failed ({e:#}); check `podman ps -a` for leftovers."));
    }

    match waited {
        Wait::Ready => Ok(app_output),
        Wait::Stopped(reason) => Err((with_output(reason, &app_output), app_output)),
        Wait::TimedOut => {
            let reason = format!(
                "The app's container is running, but nothing accepted connections on port {host_port} within {}s. \
                 It may still be starting (a slow build or start-up), listening on a different port than the one \
                 you entered, or listening only on 127.0.0.1 inside the container (it has to listen on 0.0.0.0).",
                timeout.as_secs()
            );
            Err((with_output(reason, &app_output), app_output))
        }
    }
}

fn with_output(reason: String, output: &str) -> String {
    if output.trim().is_empty() {
        format!("{reason}\n\nThe app printed nothing.")
    } else {
        format!("{reason}\n\nWhat the app printed:\n{output}")
    }
}

fn wait_timeout() -> Duration {
    let secs = std::env::var("LOCALSYNC_TESTRUN_TIMEOUT_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_WAIT_SECS);
    Duration::from_secs(secs)
}

/// A fresh directory for this run's unpacked project. Under the OS data
/// directory rather than the temp dir: on some Linux systems /tmp is a small
/// RAM disk, and on Windows/macOS the Podman VM shares the user's own folders.
fn fresh_work_dir() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("localsync").join("test-runs");
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    base.join(format!("{}-{nanos}", std::process::id()))
}

// ---------- ports ----------

/// The spec's port if it's free on this machine, else one the OS hands out -
/// the sender's own dev server is very often already on the app's port.
fn choose_host_port(preferred: u16) -> (u16, Vec<String>) {
    if port_is_free(preferred) {
        return (preferred, vec![]);
    }
    let port = std::net::TcpListener::bind(("0.0.0.0", 0)).and_then(|l| l.local_addr()).map(|a| a.port()).unwrap_or(preferred);
    let note = format!(
        "Port {preferred} is already in use on this computer (probably your own dev server), so the test run published the app on port {port} instead. \
         What you send still uses port {preferred}."
    );
    (port, vec![note])
}

/// Free on both loopback and all interfaces - Podman publishes on the latter,
/// and a dev server bound to 127.0.0.1 alone still blocks it.
fn port_is_free(port: u16) -> bool {
    let can_bind = |addr: &str| std::net::TcpListener::bind((addr, port)).is_ok();
    can_bind("127.0.0.1") && can_bind("0.0.0.0")
}

/// Does something behind `127.0.0.1:port` actually serve? A plain connect is
/// not enough: Podman's port forwarder accepts the connection on the host
/// even while nothing listens inside the container, then closes it. So after
/// connecting, wait briefly - a closed/reset connection means "not there
/// yet"; silence (an HTTP server waiting for a request) or data means served.
fn port_serves(port: u16) -> bool {
    use std::io::Read;
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else { return false };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
    let mut byte = [0u8; 1];
    match stream.read(&mut byte) {
        Ok(0) => false,
        Ok(_) => true,
        Err(e) => matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Wait {
    Ready,
    /// `stop_reason` said the wait is pointless (the container died).
    Stopped(String),
    TimedOut,
}

/// Polls until the port serves, `stop_reason` returns a reason, or `timeout`
/// passes. The port is checked first, so a reason only counts if the app isn't
/// already serving.
async fn wait_for_port<F, Fut>(port: u16, timeout: Duration, poll: Duration, mut stop_reason: F) -> Wait
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<String>>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if tauri::async_runtime::spawn_blocking(move || port_serves(port)).await.unwrap_or(false) {
            return Wait::Ready;
        }
        if let Some(reason) = stop_reason().await {
            return Wait::Stopped(reason);
        }
        if Instant::now() >= deadline {
            return Wait::TimedOut;
        }
        tokio::time::sleep(poll).await;
    }
}

// ---------- live progress ----------

/// Follows the provisioning log (where `run_snapshot` streams every line the
/// build prints) from where it is now: emits each new line as a
/// `compose-test-progress` event and keeps the last few for the report.
/// Same idea as `commands::tail_provisioning_log`, whose event is the Run
/// flow's own and which is private there.
async fn tail_provisioning_log<R: tauri::Runtime>(app: AppHandle<R>, keep: Arc<Mutex<VecDeque<String>>>) {
    let path = match ls_containers::ProvisioningLog::open_default() {
        Ok(log) => log.path().to_path_buf(),
        Err(_) => return,
    };
    let mut offset = tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0);
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let Ok(contents) = tokio::fs::read(&path).await else { continue };
        if (contents.len() as u64) <= offset {
            continue;
        }
        let new_bytes = &contents[offset as usize..];
        // Whole lines only; a half-written one is picked up next round.
        if let Some(last_newline) = new_bytes.iter().rposition(|&b| b == b'\n') {
            for line in String::from_utf8_lossy(&new_bytes[..=last_newline]).lines() {
                if let Ok(mut kept) = keep.lock() {
                    kept.push_back(line.to_string());
                    while kept.len() > KEPT_LINES {
                        kept.pop_front();
                    }
                }
                emit_line(&app, line);
            }
            offset += (last_newline + 1) as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} failed");
    }

    #[test]
    fn inspect_reports_compose_and_git_state() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();

        // Not a repo, no compose file.
        assert_eq!(inspect(p), ProjectInspection { has_compose: false, is_git_repo: false, has_commits: false });

        // Repo with no commits yet.
        git(p, &["init", "-q"]);
        assert_eq!(inspect(p), ProjectInspection { has_compose: false, is_git_repo: true, has_commits: false });

        // A compose file (even untracked) is seen; commits are seen after one exists.
        std::fs::write(p.join("docker-compose.yml"), "services: {}\n").unwrap();
        assert!(inspect(p).has_compose);
        git(p, &["add", "-A"]);
        git(p, &["commit", "-q", "-m", "init"]);
        assert_eq!(inspect(p), ProjectInspection { has_compose: true, is_git_repo: true, has_commits: true });
    }

    #[test]
    fn inspect_only_counts_docker_compose_yml_not_lookalikes() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["compose.yml", "docker-compose.yaml", "docker-compose.yml.bak"] {
            std::fs::write(dir.path().join(name), "services: {}\n").unwrap();
        }
        assert!(!inspect(dir.path()).has_compose, "the bundler only looks for docker-compose.yml");
        // A directory with that name is not a compose file either.
        std::fs::create_dir(dir.path().join("docker-compose.yml")).unwrap();
        assert!(!inspect(dir.path()).has_compose);
    }

    #[test]
    fn inspect_does_not_treat_a_subfolder_of_a_repo_as_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("f"), "x").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "c"]);
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let i = inspect(&sub);
        assert!(!i.is_git_repo && !i.has_commits, "{i:?}");
    }

    #[test]
    fn a_busy_port_is_replaced_by_a_free_one_and_says_so() {
        let busy = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let busy_port = busy.local_addr().unwrap().port();
        assert!(!port_is_free(busy_port));
        let (port, notes) = choose_host_port(busy_port);
        assert_ne!(port, busy_port);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains(&busy_port.to_string()) && notes[0].contains(&port.to_string()), "{notes:?}");
    }

    #[test]
    fn a_loopback_only_listener_also_blocks_the_port() {
        let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        assert!(!port_is_free(busy.local_addr().unwrap().port()), "a dev server on 127.0.0.1 conflicts with Podman's publish");
    }

    #[test]
    fn a_free_port_is_kept_and_needs_no_note() {
        // Other tests grab OS-assigned ports concurrently, so the port found
        // free a moment ago can be taken by the time it's asked about: any one
        // of a few tries staying free is enough.
        let kept = (0..10).any(|_| {
            let port = std::net::TcpListener::bind("0.0.0.0:0").unwrap().local_addr().unwrap().port();
            choose_host_port(port) == (port, vec![])
        });
        assert!(kept, "a free port was never kept as-is");
    }

    #[test]
    fn port_serves_distinguishes_serving_from_closed_and_accept_then_drop() {
        // Nothing listening.
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_port = dead.local_addr().unwrap().port();
        drop(dead);
        assert!(!port_serves(dead_port));

        // Accepts, then hangs up at once (Podman's forwarder with no backend).
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            drop(s);
        });
        assert!(!port_serves(p), "accept-then-close must not count as serving");
        t.join().unwrap();

        // Accepts and holds the connection open, like an HTTP server waiting for a request.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let t = std::thread::spawn(move || {
            let (_s, _) = l.accept().unwrap();
            let _ = rx.recv_timeout(Duration::from_secs(5));
        });
        assert!(port_serves(p));
        tx.send(()).unwrap();
        t.join().unwrap();

        // Speaks first (a database).
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            use std::io::Write;
            let (mut s, _) = l.accept().unwrap();
            s.write_all(b"hello").unwrap();
            std::thread::sleep(Duration::from_millis(600));
        });
        assert!(port_serves(p));
        t.join().unwrap();
    }

    #[tokio::test]
    async fn wait_for_port_times_out_when_nothing_ever_serves() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let started = Instant::now();
        let out = wait_for_port(port, Duration::from_millis(700), Duration::from_millis(100), || async { None }).await;
        assert_eq!(out, Wait::TimedOut);
        assert!(started.elapsed() >= Duration::from_millis(700));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn wait_for_port_stops_early_with_the_reason_when_the_container_died() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let started = Instant::now();
        let out = wait_for_port(port, Duration::from_secs(60), Duration::from_millis(50), || async { Some("it crashed".to_string()) }).await;
        assert_eq!(out, Wait::Stopped("it crashed".to_string()));
        assert!(started.elapsed() < Duration::from_secs(5), "must not wait out the timeout");
    }

    #[tokio::test]
    async fn wait_for_port_returns_ready_as_soon_as_the_port_serves() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        // Holds every connection open.
        let t = std::thread::spawn(move || {
            let mut held = vec![];
            while let Ok((s, _)) = l.accept() {
                held.push(s);
                if held.len() >= 2 {
                    break;
                }
            }
        });
        // A stop reason is ignored when the port already serves.
        let out = wait_for_port(port, Duration::from_secs(10), Duration::from_millis(50), || async { Some("ignored".to_string()) }).await;
        assert_eq!(out, Wait::Ready);
        let _ = std::net::TcpStream::connect(("127.0.0.1", port));
        t.join().unwrap();
    }

    #[tokio::test]
    async fn test_run_rejects_a_folder_without_compose_settings() {
        use tauri::Manager;
        let app = tauri::test::mock_app();
        app.manage(AppState::default());
        let handle = app.handle().clone();
        let folder = FolderPlanDto { path: "/nonexistent".into(), dump: None, compose: None };
        let err = test_run_compose(handle.clone(), handle.state::<AppState>(), folder).await.unwrap_err();
        assert!(err.contains("no compose settings"), "{err}");
    }
}
