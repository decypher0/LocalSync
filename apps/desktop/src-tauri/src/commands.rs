//! Tauri command handlers. Each command is a thin wrapper around one of the
//! four backend crates — the crates own all real logic (bundling, crypto,
//! diffing, container orchestration); this module only adapts their
//! `anyhow::Result`s to `Result<_, String>` for the IPC boundary, tracks
//! held snapshots/sessions in `AppState`, and emits progress events.
//!
//! `run_snapshot` is the ONLY command in this file allowed to call
//! `ls_containers::run_snapshot` — the one function in the whole app that
//! executes code that arrived over the network. It only ever runs against a
//! `VerifiedSnapshot` that `receive_snapshot` already checked, and only once
//! the frontend has shown the user a diff and they clicked Run.

use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::state::AppState;

#[derive(Clone, Serialize)]
pub struct Progress {
    pub bytes: usize,
    pub total: usize,
}

#[derive(Clone, Serialize)]
pub struct IncomingSnapshotInfo {
    /// `"<project_name>@<git_commit>"` — hand this back to `run_snapshot`.
    pub snapshot_id: String,
    pub manifest: ls_snapshot::Manifest,
    pub diff: ls_security::DiffSummary,
}

/// Returned by [`start_send_session`]. `room_code` is what the user shows
/// the receiver (paste-able, spoken aloud); `room_id`/`signaling_url` are
/// what the frontend hands straight to the existing, unmodified
/// `share_snapshot`.
#[derive(Clone, Serialize)]
pub struct SendSessionInfo {
    pub room_code: String,
    pub room_id: String,
    pub signaling_url: String,
}

/// Returned by [`decode_room_code`]. Hand straight to the existing,
/// unmodified `receive_snapshot`.
#[derive(Clone, Serialize)]
pub struct DecodedRoomCode {
    pub room_id: String,
    pub signaling_url: String,
}

#[derive(Serialize)]
pub struct RunningSessionInfo {
    /// Equal to `RunningSession::compose_project_name` — hand this back to
    /// `stop_session`.
    pub session_id: String,
    pub project_name: String,
    pub service_ports: Vec<(String, String)>,
    pub db_cache_hit: bool,
}

/// Starts an embedded signaling relay on the LAN and derives a room code
/// from it, so nobody has to run a separate signaling-server process or
/// type its address. The frontend shows `room_code` to the user, then
/// calls the existing, unmodified `share_snapshot(project_path, room_id,
/// signaling_url)` with the machine-derived `room_id`/`signaling_url`.
///
/// The relay's background task is intentionally left detached (dropping a
/// `tokio::JoinHandle` does not abort it) - see `ls_net::host_ephemeral_relay`'s
/// doc comment for why that's fine at this app's scale.
#[tauri::command]
pub async fn start_send_session() -> Result<SendSessionInfo, String> {
    let (port, _relay_task) = ls_net::host_ephemeral_relay().await.map_err(|e| e.to_string())?;
    let lan_ip = ls_net::detect_lan_ip().map_err(|e| e.to_string())?;
    let room_id = ls_net::generate_room_id();
    let addr = std::net::SocketAddrV4::new(lan_ip, port);
    let room_code = ls_net::encode_room_code(addr, &room_id);
    let signaling_url = format!("ws://{lan_ip}:{port}");
    log::info!("start_send_session: hosting relay on {signaling_url}, room_code={room_code}");
    Ok(SendSessionInfo { room_code, room_id, signaling_url })
}

/// Decodes a room code pasted by the user into the `room_id`/`signaling_url`
/// pair the existing, unmodified `receive_snapshot` needs.
#[tauri::command]
pub fn decode_room_code(code: String) -> Result<DecodedRoomCode, String> {
    let (addr, room_id) = ls_net::decode_room_code(&code).map_err(|e| e.to_string())?;
    let signaling_url = format!("ws://{}:{}", addr.ip(), addr.port());
    Ok(DecodedRoomCode { room_id, signaling_url })
}

/// Bundles `project_path` into a signed snapshot and sends it to whoever
/// joins `room_code` on the signaling server. Returns the id
/// (`project_name@git_commit`) the sender can use to recognize their own
/// send in the UI.
// Generic over the Tauri `Runtime` (defaults to none picked here - the real
// app binds it to `Wry` via `invoke_handler!`) rather than the concrete
// `AppHandle` (= `AppHandle<Wry>`) alias, so `tests/send_flow_test.rs` can
// call this directly with a `tauri::test::mock_app()`'s `AppHandle<MockRuntime>`
// - the actual point of that test: exercising this exact function, not a
// re-implementation of it, against a runtime that doesn't need a display.
#[tauri::command]
pub async fn share_snapshot<R: tauri::Runtime>(
    app: AppHandle<R>,
    project_path: String,
    room_code: String,
    signaling_url: String,
) -> Result<String, String> {
    log::info!("share_snapshot: starting for project_path={project_path} room={room_code}");
    let root = PathBuf::from(project_path);
    // create_snapshot shells out to git and walks the filesystem — blocking
    // work that has no business running on the async command's task.
    let snapshot = tauri::async_runtime::spawn_blocking(move || ls_snapshot::create_snapshot(&root, None))
        .await
        .map_err(|e| format!("snapshot task panicked: {e}"))?
        .map_err(|e| {
            log::warn!("share_snapshot: create_snapshot failed: {e}");
            e.to_string()
        })?;

    let snapshot_id = format!("{}@{}", snapshot.manifest.project_name, snapshot.manifest.git_commit);
    log::info!("share_snapshot: snapshot created, id={snapshot_id}");

    // Same wire format ls-snapshot's own save_to_file uses: plain JSON,
    // payload bytes riding along as a JSON byte array. Simplest thing that
    // works with the (de)serializers ls-snapshot already ships.
    let bytes = serde_json::to_vec(&snapshot).map_err(|e| e.to_string())?;

    log::info!("share_snapshot: connecting to signaling");
    let conn = ls_net::connect_as_sender(&signaling_url, &room_code)
        .await
        .map_err(|e| {
            log::warn!("share_snapshot: connect_as_sender failed: {e}");
            e.to_string()
        })?;
    log::info!("share_snapshot: data channel open, sending payload ({} bytes)", bytes.len());

    ls_net::send_payload(&conn, &bytes, |sent, total| {
        let _ = app.emit("share-progress", Progress { bytes: sent, total });
    })
    .await
    .map_err(|e| {
        log::warn!("share_snapshot: send_payload failed: {e}");
        e.to_string()
    })?;

    log::info!("share_snapshot: done, id={snapshot_id}");
    Ok(snapshot_id)
}

/// Receives a snapshot over the P2P channel, verifies its signature
/// (trust-on-first-use — see `ls_security::verify`'s doc comment), and
/// returns the manifest + diff for the review screen. Does **not** unpack
/// `source/` or touch containers; the verified snapshot is held in
/// `AppState` until (and unless) the user clicks Run.
// Generic over `R: tauri::Runtime` for the same reason as share_snapshot
// above.
#[tauri::command]
pub async fn receive_snapshot<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    room_code: String,
    signaling_url: String,
) -> Result<IncomingSnapshotInfo, String> {
    log::info!("receive_snapshot: starting for room={room_code}");
    let conn = ls_net::connect_as_receiver(&signaling_url, &room_code)
        .await
        .map_err(|e| {
            log::warn!("receive_snapshot: connect_as_receiver failed: {e}");
            e.to_string()
        })?;
    log::info!("receive_snapshot: data channel open, receiving payload");

    let bytes = ls_net::receive_payload(&conn, |received, total| {
        let _ = app.emit("receive-progress", Progress { bytes: received, total });
    })
    .await
    .map_err(|e| {
        log::warn!("receive_snapshot: receive_payload failed: {e}");
        e.to_string()
    })?;

    let snapshot: ls_snapshot::Snapshot = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;

    // Trust-on-first-use: empty trusted_keys accepts any signature that
    // checks out. Documented MVP behavior (crates/ls-security/src/verify.rs)
    // — a real keyring UI is out of scope here.
    let verified = ls_security::verify(snapshot, &[]).map_err(|e| e.to_string())?;
    let diff = ls_security::diff_summary(&verified).map_err(|e| e.to_string())?;
    let manifest = verified.snapshot().manifest.clone();
    let snapshot_id = format!("{}@{}", manifest.project_name, manifest.git_commit);

    state
        .verified
        .lock()
        .map_err(|e| e.to_string())?
        .insert(snapshot_id.clone(), verified);

    log::info!("receive_snapshot: done, id={snapshot_id}");
    Ok(IncomingSnapshotInfo { snapshot_id, manifest, diff })
}

/// Executes a previously-received, verified snapshot in sandboxed Podman
/// containers. Only reachable after the user has seen `receive_snapshot`'s
/// diff and clicked a real Run button — there is no other path to this
/// function's one call into `ls_containers::run_snapshot`.
#[tauri::command]
pub async fn run_snapshot(
    state: State<'_, AppState>,
    snapshot_id: String,
    work_dir: String,
) -> Result<RunningSessionInfo, String> {
    let verified = state
        .verified
        .lock()
        .map_err(|e| e.to_string())?
        .remove(&snapshot_id)
        .ok_or_else(|| format!("no held snapshot with id {snapshot_id}"))?;

    let session = ls_containers::run_snapshot(&verified, &PathBuf::from(work_dir))
        .await
        .map_err(|e| e.to_string())?;

    let info = RunningSessionInfo {
        session_id: session.compose_project_name.clone(),
        project_name: session.project_name.clone(),
        service_ports: session.service_ports.clone(),
        db_cache_hit: session.db_cache_hit,
    };

    state
        .sessions
        .lock()
        .map_err(|e| e.to_string())?
        .insert(info.session_id.clone(), session);

    Ok(info)
}

/// Tears down a running session's containers/network. The seeded DB volume
/// is intentionally left alone by `ls_containers::stop_session` — that's
/// the cache the next run reuses.
#[tauri::command]
pub async fn stop_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let session = state
        .sessions
        .lock()
        .map_err(|e| e.to_string())?
        .remove(&session_id)
        .ok_or_else(|| format!("no running session with id {session_id}"))?;

    ls_containers::stop_session(&session).await.map_err(|e| e.to_string())
}
