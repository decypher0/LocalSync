//! Proves round-9's `run-progress` event streaming end to end:
//! `commands::run_snapshot` tails the *real*
//! `ls_containers::ProvisioningLog` file live (see `tail_provisioning_log`
//! in `commands.rs`) while `ls_containers::run_snapshot` is in flight,
//! rather than faking a progress bar. Attaches a real Tauri event listener
//! (`tauri::Listener::listen` - the Rust-side API, not the JS `listen()`)
//! to a `mock_app()`'s `AppHandle` *before* calling `run_snapshot`, and
//! asserts that actual `ensure_podman_ready()` log content arrived as
//! `run-progress` events during the call.
//!
//! Drives a real Send/Receive/Run through the command layer, same
//! construction `run_retry_test.rs` uses, so the tailer has a real
//! multi-second `run_snapshot` call to observe lines during (a podman
//! bring-up is slow enough that the tailer's ~200ms poll fires many times
//! over it - an instant PATH-check failure on a podman-less machine could
//! in principle resolve before the first poll tick, which is why this test
//! - like `run_retry_test.rs` - requires a real podman/podman-compose on
//! PATH rather than asserting on that race).

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use localsync_desktop::{commands, state::AppState};
use tauri::{Listener, Manager};

fn sh(dir: &Path, cmd: &str, args: &[&str]) {
    let status = Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {cmd} {args:?}: {e}"));
    assert!(status.success(), "{cmd} {args:?} failed in {}", dir.display());
}

fn podman_stack_available() -> bool {
    let ok = |bin: &str| {
        Command::new(bin)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    ok("podman") && ok("podman-compose")
}

/// Same requirement `pipeline_test.rs`/`run_retry_test.rs` document:
/// `create_snapshot` needs `project_root` to be a repo root of its own.
fn make_git_project_from(source: &Path) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let project_dir = dir.path().join("sample-project");
    std::fs::create_dir(&project_dir).unwrap();
    sh(
        Path::new("."),
        "cp",
        &["-r", &format!("{}/.", source.display()), &project_dir.display().to_string()],
    );
    sh(&project_dir, "git", &["init", "-q"]);
    sh(&project_dir, "git", &["add", "-A"]);
    sh(
        &project_dir,
        "git",
        &[
            "-c", "user.name=Test", "-c", "user.email=test@example.com",
            "commit", "-q", "-m", "run-progress test snapshot",
        ],
    );
    (dir, project_dir)
}

#[tokio::test]
async fn run_snapshot_streams_real_provisioning_log_lines_as_events() {
    if !podman_stack_available() {
        eprintln!("podman/podman-compose not on PATH — skipping (needs a real run to reliably observe timing)");
        return;
    }

    let sample_project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../sample-project");
    let sender_home = tempfile::tempdir().unwrap();
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::set_var(home_var, sender_home.path());

    let (_tempdir, project_dir) = make_git_project_from(&sample_project);
    let snapshot = ls_snapshot::create_snapshot(&project_dir, None)
        .expect("create_snapshot should succeed against a git-initialized sample-project copy");
    let snapshot_id = format!("{}@{}", snapshot.manifest.project_name, snapshot.manifest.git_commit);
    let verified =
        ls_security::verify(snapshot, &[]).expect("TOFU verify should accept a freshly-signed snapshot");

    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    let handle = app.handle().clone();
    handle
        .state::<AppState>()
        .verified
        .lock()
        .unwrap()
        .insert(snapshot_id.clone(), verified);

    // Attach the listener *before* calling run_snapshot - this is the whole
    // point of the test: proving events actually arrive during the call,
    // not just that the command returns successfully.
    let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let received_for_listener = received.clone();
    handle.listen("run-progress", move |event| {
        // Decode the real RunProgress payload (`{"line": "..."}"`) rather
        // than string-matching the raw JSON.
        if let Ok(payload) = serde_json::from_str::<serde_json::Value>(event.payload()) {
            if let Some(line) = payload.get("line").and_then(|l| l.as_str()) {
                received_for_listener.lock().unwrap().push(line.to_string());
            }
        }
    });

    let work_dir = tempfile::tempdir().unwrap();
    let session = commands::run_snapshot(
        handle.clone(),
        handle.state::<AppState>(),
        snapshot_id,
        work_dir.path().display().to_string(),
    )
    .await
    .expect("run_snapshot should succeed against a real podman stack");

    let lines = received.lock().unwrap().clone();
    assert!(
        !lines.is_empty(),
        "expected at least one run-progress event to arrive during a real run_snapshot call"
    );
    assert!(
        lines.iter().any(|l| l.contains("provisioning check")),
        "expected real ensure_podman_ready() log content among the streamed lines, got: {lines:?}"
    );

    commands::stop_session(handle.state::<AppState>(), session.session_id)
        .await
        .expect("stop_session should tear the run down cleanly");
}
