//! Proves the round-7 fix (see `crates/ls-snapshot/src/bundle.rs`'s
//! `git_bytes`) against the real Tauri command layer, not the lower-level
//! crate functions `pipeline_test.rs`/`pipeline_node_test.rs` already
//! exercise directly. The field bug went through `commands::share_snapshot`
//! / `commands::receive_snapshot` — this test calls those two functions
//! directly, with a real `AppHandle`/`State<AppState>` built via Tauri's own
//! `tauri::test` utilities (needs the `test` cargo feature — see
//! `Cargo.toml`'s `[dev-dependencies]`), against a locally-spawned signaling
//! server, sender and receiver running concurrently in the same process
//! (two `tokio::spawn`s), same pattern `crates/ls-net/tests/transfer.rs`
//! uses for the signaling server itself.
//!
//! Run against both `sample-project/` (Spring Boot) and
//! `sample-project-node/` (Express) so the fix isn't accidentally coupled to
//! one stack, mirroring why `pipeline_node_test.rs` exists alongside
//! `pipeline_test.rs`.
//!
//! Before the round-7 fix, `share_snapshot` here would (if it reproduced the
//! field hang) block forever inside `create_snapshot`'s git subprocesses
//! with no timeout; after the fix, a real hang fails loudly within
//! `GIT_TIMEOUT` instead, and the normal (non-hanging) path — what actually
//! runs in this repo's environment — completes and is asserted end to end.
//!
//! Installs `send_log` at a tempdir path (not the real OS data dir) so the
//! resulting `send.log` can be read back as evidence of the real
//! stage-by-stage flow (bundling/each git command/bundling done -> signing
//! -> connecting to signaling -> offer/ICE/SDP -> data channel open ->
//! transfer progress -> done) without touching a real machine's log file.
//! Both sub-tests share one process (and therefore one send.log, since
//! `log::set_boxed_logger` only accepts the first installer) — that's fine,
//! it just means the file below shows both runs interleaved by timestamp.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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

/// Same requirement `pipeline_test.rs`/`pipeline_node_test.rs` document:
/// `create_snapshot` needs `project_root` to be a repo root of its own, and
/// derives `manifest.project_name` from that directory's basename.
fn make_git_project_from(source: &Path, name: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let project_dir = dir.path().join(name);
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
            "commit", "-q", "-m", "send flow test snapshot",
        ],
    );
    (dir, project_dir)
}

/// Same fallback `crates/ls-net/tests/transfer.rs` establishes: spawn
/// `apps/signaling-server/index.js` via `node` if reachable, else point at
/// `LS_NET_TEST_SIGNALING_URL` (needed on WSL2, where Node isn't on PATH).
async fn signaling_url(port: u16) -> (String, Option<tokio::process::Child>) {
    if let Ok(url) = std::env::var("LS_NET_TEST_SIGNALING_URL") {
        return (url, None);
    }

    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../apps/signaling-server/index.js"
    );
    let child = tokio::process::Command::new("node")
        .arg(script)
        .env("PORT", port.to_string())
        .kill_on_drop(true)
        .spawn()
        .expect(
            "failed to spawn `node` for the signaling server - either install Node on this \
             machine or point LS_NET_TEST_SIGNALING_URL at an already-running instance",
        );
    tokio::time::sleep(Duration::from_millis(300)).await;
    (format!("ws://127.0.0.1:{port}"), Some(child))
}

/// Fixed (not per-test-tempdir) path: both sub-tests in this binary share
/// one process, and `log::set_boxed_logger` only accepts the first
/// installer, so whichever test runs first wins regardless — pointing both
/// at the same well-known path means the resulting file (read back for the
/// task's "sample of real log output" evidence) actually captures both
/// runs instead of only whichever test happened to install first.
fn shared_send_log_path() -> PathBuf {
    std::env::temp_dir().join("localsync-send-flow-test-logs").join("send.log")
}

/// One end-to-end run of the real command layer: sender-side
/// `commands::share_snapshot` and receiver-side `commands::receive_snapshot`,
/// concurrently, against `sample_dir` (copied into a fresh git repo first).
async fn run_send_flow(sample_dir: &Path, project_dir_name: &str, port: u16) {
    localsync_desktop::send_log::install_at(shared_send_log_path());

    assert!(sample_dir.is_dir(), "expected {} to exist", sample_dir.display());
    let (url, _server) = signaling_url(port).await;
    let room = format!("send-flow-test-{project_dir_name}-{port}-{}", std::process::id());

    // Only the sender ever loads ~/.localsync/identity.key (see
    // crates/ls-snapshot/src/sign.rs and main.rs's isolate_data_dir_if_set
    // doc comment) - receive_snapshot never touches HOME/USERPROFILE (see
    // crates/ls-security, which has no such env lookup), so unlike
    // pipeline_test.rs's *sequential* two-HOME dance, one isolated HOME set
    // once before spawning is sufficient here even though sender and
    // receiver run concurrently.
    let sender_home = tempfile::tempdir().unwrap();
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::set_var(home_var, sender_home.path());

    let (_tempdir, project_dir) = make_git_project_from(sample_dir, project_dir_name);

    let sender_app = tauri::test::mock_app();
    sender_app.manage(AppState::default());
    let sender_handle = sender_app.handle().clone();

    let receiver_app = tauri::test::mock_app();
    receiver_app.manage(AppState::default());
    let receiver_handle = receiver_app.handle().clone();

    let sender_room = room.clone();
    let sender_url = url.clone();
    let project_path = project_dir.display().to_string();
    let sender_task = tokio::spawn(async move {
        let state = sender_handle.state::<AppState>();
        commands::share_snapshot(sender_handle.clone(), state, project_path, sender_room, sender_url).await
    });

    let receiver_room = room.clone();
    let receiver_url = url.clone();
    let receiver_task = tokio::spawn(async move {
        let state = receiver_handle.state::<AppState>();
        commands::receive_snapshot(receiver_handle.clone(), state, receiver_room, receiver_url).await
    });

    let sender_result = sender_task.await.expect("sender task panicked");
    let receiver_result = receiver_task.await.expect("receiver task panicked");

    let snapshot_id = sender_result.expect("share_snapshot should succeed via the real command layer");
    let info = receiver_result.expect("receive_snapshot should succeed via the real command layer");

    assert_eq!(info.snapshot_id, snapshot_id, "sender and receiver should agree on the snapshot id");
    assert_eq!(info.manifest.project_name, project_dir_name);
    assert!(!info.manifest.services.is_empty(), "docker-compose.yml should yield at least one service");
    assert!(!info.diff.entries.is_empty(), "an initial snapshot should report every file as added");
    assert!(info.diff.entries.iter().all(|e| e.change_type == "added"));

    // receive_snapshot only *holds* the verified snapshot (see commands.rs's
    // module doc comment - run_snapshot is gated on a real user click) -
    // confirm it's actually sitting in AppState under the id it returned,
    // proving the full command-layer contract, not just its return value.
    let held = receiver_app
        .state::<AppState>()
        .verified
        .lock()
        .unwrap()
        .contains_key(&info.snapshot_id);
    assert!(held, "receive_snapshot should stash the verified snapshot in AppState");
}

#[tokio::test]
async fn share_receive_via_command_layer_spring_boot() {
    let sample_project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../sample-project");
    run_send_flow(&sample_project, "sample-project", 8124).await;
}

#[tokio::test]
async fn share_receive_via_command_layer_node_express() {
    let sample_project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../sample-project-node");
    run_send_flow(&sample_project, "sample-project-node", 8125).await;
}
