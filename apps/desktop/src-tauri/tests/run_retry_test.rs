//! Proves the round-9 fix to `commands::run_snapshot`: before the fix, the
//! held `VerifiedSnapshot` was removed from `AppState` unconditionally, so
//! *any* Run failure — including a purely environment-level one (Podman
//! missing, a bad work dir, a port in use) — left the snapshot unusable and
//! forced a fresh Send/Receive just to retry. Signature/tamper verification
//! already happened back in `receive_snapshot`, well before a `snapshot_id`
//! even exists to call `run_snapshot` with, so nothing reachable from this
//! function is a verification failure — every error here should be
//! retry-able.
//!
//! Doesn't go through the real network/signaling path (that's
//! `send_flow_test.rs`'s job) — the bug and its fix live entirely in how
//! `run_snapshot` manages `AppState.verified`, so this builds a
//! `VerifiedSnapshot` directly, the same way `pipeline_test.rs` does, and
//! drives it straight through `commands::run_snapshot` twice: once against a
//! work dir engineered to fail deterministically (a regular file where a
//! directory needs to be created), once for real.

use std::path::Path;
use std::process::Command;

use localsync_desktop::{commands, state::AppState};
use tauri::Manager;

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

/// Same requirement `pipeline_test.rs` documents: `create_snapshot` needs
/// `project_root` to be a repo root of its own.
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
            "commit", "-q", "-m", "run-retry test snapshot",
        ],
    );
    (dir, project_dir)
}

#[tokio::test]
async fn failed_run_for_an_environment_reason_leaves_the_snapshot_retry_able() {
    if !podman_stack_available() {
        eprintln!("podman/podman-compose not on PATH — skipping (needs a real successful retry)");
        return;
    }

    let sample_project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../sample-project");
    let sender_home = tempfile::tempdir().unwrap();
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::set_var(home_var, sender_home.path());

    let (_tempdir, project_dir) = make_git_project_from(&sample_project);
    let snapshot = ls_snapshot::create_snapshot(&project_dir, None)
        .expect("create_snapshot should succeed against a git-initialized sample-project copy");
    // Same construction commands::receive_snapshot uses.
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

    // A regular file where run_snapshot needs to create a directory —
    // deterministic, environment-style failure (not a verification one),
    // same on every OS: create_dir_all onto an existing non-directory fails.
    let work_dir_blocker = tempfile::NamedTempFile::new().unwrap();
    let bad_work_dir = work_dir_blocker.path().to_path_buf();

    let first_attempt = commands::run_snapshot(
        handle.clone(),
        handle.state::<AppState>(),
        snapshot_id.clone(),
        bad_work_dir.display().to_string(),
    )
    .await;
    assert!(first_attempt.is_err(), "run_snapshot against an unusable work dir should fail");

    let still_held = handle
        .state::<AppState>()
        .verified
        .lock()
        .unwrap()
        .contains_key(&snapshot_id);
    assert!(
        still_held,
        "a failed Run for an environment reason must leave the snapshot held, not discard it"
    );

    // Retry, this time for real — no fresh receive, same snapshot_id, just a
    // usable work dir this time. This is the actual regression: before the
    // fix this failed with "no held snapshot with id ...".
    let real_work_dir = tempfile::tempdir().unwrap();
    let session = commands::run_snapshot(
        handle.clone(),
        handle.state::<AppState>(),
        snapshot_id.clone(),
        real_work_dir.path().display().to_string(),
    )
    .await
    .expect("retrying Run with a fixed work dir should succeed using the same held snapshot");

    assert_eq!(session.project_name, "sample-project");

    let no_longer_held = handle
        .state::<AppState>()
        .verified
        .lock()
        .unwrap()
        .contains_key(&snapshot_id);
    assert!(!no_longer_held, "a successful Run should consume the held snapshot");

    commands::stop_session(handle.state::<AppState>(), session.session_id)
        .await
        .expect("stop_session should tear the retried run down cleanly");
}
