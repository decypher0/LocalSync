//! End-to-end exercise of the logic behind every Tauri command in
//! `src/commands.rs`, run directly against the four backend crates (no
//! Tauri `AppHandle`/window needed — that's why this lives in `tests/`
//! rather than as a unit test of `commands.rs` itself, which takes Tauri
//! types the harness would have to fake).
//!
//! Pipeline: create_snapshot (sender) -> wire round-trip (the same
//! serde_json encoding `share_snapshot`/`receive_snapshot` use) -> verify
//! (TOFU, receiver) -> diff_summary -> run_snapshot -> stop_session.
//! ls-net itself isn't exercised here (that's a real WebRTC handshake,
//! already ls-net's own concern) — this proves everything on the two sides
//! of that hop.
//!
//! Two temp dirs stand in for the sender's and receiver's `~/.localsync`
//! (identity.key lives at `$HOME/.localsync/...` — see
//! `crates/ls-snapshot/src/sign.rs`), so this never touches the real one.
//! `HOME` is a process-global env var and this file has exactly one test,
//! so mutating it isn't a cross-test race.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

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

/// `sample-project/` lives at the repo root and isn't its own git repo (it's
/// tracked as part of the LocalSync monorepo). `create_snapshot` shells out
/// to git scoped to `project_root`, so it needs `project_root` to itself be
/// a repo root — copy sample-project into a fresh tempdir and git-init it
/// there, same as ls-snapshot's own tests do.
/// Copies into a subdirectory literally named "sample-project" (not the
/// tempdir root, whose name is random) - `create_snapshot` derives
/// `manifest.project_name` from the snapshotted directory's own basename, and
/// the whole point of this test is proving that name flows through correctly.
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
            "commit", "-q", "-m", "sample-project snapshot",
        ],
    );
    (dir, project_dir)
}

#[tokio::test]
async fn share_receive_verify_diff_run_stop_pipeline() {
    let sample_project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../sample-project");
    assert!(sample_project.is_dir(), "expected {} to exist", sample_project.display());

    let sender_home = tempfile::tempdir().unwrap();
    let receiver_home = tempfile::tempdir().unwrap();
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };

    // --- sender: bundle sample-project into a signed snapshot ---
    let (_tempdir, project_dir) = make_git_project_from(&sample_project);
    std::env::set_var(home_var, sender_home.path());
    let snapshot = ls_snapshot::create_snapshot(&project_dir, None)
        .expect("create_snapshot should succeed against a git-initialized sample-project copy");
    assert!(!snapshot.manifest.services.is_empty(), "sample-project's docker-compose.yml should yield services");

    // --- wire hop: exactly what share_snapshot / receive_snapshot do ---
    let wire_bytes = serde_json::to_vec(&snapshot).expect("snapshot should serialize");
    let received: ls_snapshot::Snapshot =
        serde_json::from_slice(&wire_bytes).expect("snapshot should round-trip through JSON");

    // --- receiver: verify (TOFU) + diff ---
    std::env::set_var(home_var, receiver_home.path());
    let verified = ls_security::verify(received, &[]).expect("TOFU verify should accept a freshly-signed snapshot");
    let diff = ls_security::diff_summary(&verified).expect("diff_summary should read diff_stat.json from the payload");

    assert!(!diff.entries.is_empty(), "an initial snapshot should report every file as added");
    assert!(diff.entries.iter().any(|e| e.path == "docker-compose.yml"));
    assert!(diff.entries.iter().all(|e| e.change_type == "added"));
    assert_eq!(diff.total_insertions, diff.entries.iter().map(|e| e.insertions).sum::<u32>());

    if !podman_stack_available() {
        eprintln!("podman/podman-compose not on PATH — skipping run_snapshot/stop_session");
        return;
    }

    // --- run: only reachable, in the real app, after a human clicks Run ---
    let work_dir = tempfile::tempdir().unwrap();
    let t0 = Instant::now();
    let session = ls_containers::run_snapshot(&verified, work_dir.path())
        .await
        .expect("run_snapshot should bring sample-project up under podman-compose");
    let first_run_elapsed = t0.elapsed();

    assert_eq!(session.project_name, "sample-project");
    assert!(!session.db_cache_hit, "first run against a fresh volume should be a cold start");
    let app_port = session
        .service_ports
        .iter()
        .find(|(svc, _)| svc == "app")
        .map(|(_, ports)| ports.split(':').next().unwrap().to_string())
        .expect("compose file publishes a host port for the app service");

    let reachable = wait_for_port(&app_port, Duration::from_secs(240));

    let stop_result = ls_containers::stop_session(&session).await;
    assert!(stop_result.is_ok(), "stop_session should tear the project down: {stop_result:?}");

    assert!(
        reachable,
        "app service on port {app_port} never accepted a TCP connection within the timeout \
         (first run has to pull/build the Maven + MySQL images, which can be slow)"
    );

    // --- resend of the *same unchanged* project: same git_commit (so the
    // same compose_project_name) and the same db_seed_hash, exactly what
    // happens if a sender resends without touching their code. This is the
    // actual scenario the "second snapshot reuses the cache and starts
    // faster" requirement is about - re-running with a fresh `git init` (a
    // different commit) wouldn't exercise it, since ls-containers keys the
    // volume by content hash but the compose project name by commit.
    let verified2 = ls_security::verify(snapshot.clone(), &[])
        .expect("re-verifying the same signed snapshot should succeed identically");
    let t1 = Instant::now();
    let session2 = ls_containers::run_snapshot(&verified2, work_dir.path())
        .await
        .expect("second run_snapshot of the same unchanged project should succeed");
    let second_run_elapsed = t1.elapsed();

    assert!(
        session2.db_cache_hit,
        "resending the same project with an unchanged seed should hit the existing DB volume, not reseed"
    );
    eprintln!(
        "cache check: first run (cold) {first_run_elapsed:?}, second run (cached) {second_run_elapsed:?}"
    );

    let stop_result2 = ls_containers::stop_session(&session2).await;
    assert!(stop_result2.is_ok(), "second stop_session should also tear down cleanly: {stop_result2:?}");
}

/// Polls `GET /health` (sample-project's app exposes it — see
/// sample-project/README.md) until it gets any HTTP response back, rather
/// than just a bare TCP accept: that proves the Spring Boot app itself is
/// answering requests, not just that the container's port is open.
fn wait_for_port(port: &str, timeout: Duration) -> bool {
    let addr = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(mut stream) = TcpStream::connect(&addr) {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let request = format!("GET /health HTTP/1.0\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n");
            if stream.write_all(request.as_bytes()).is_ok() {
                let mut buf = [0u8; 32];
                if let Ok(n) = stream.read(&mut buf) {
                    if buf[..n].starts_with(b"HTTP/") {
                        return true;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    false
}
