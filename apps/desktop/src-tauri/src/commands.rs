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

#[derive(Serialize)]
pub struct RunningSessionInfo {
    /// Equal to `RunningSession::compose_project_name` — hand this back to
    /// `stop_session`.
    pub session_id: String,
    pub project_name: String,
    pub service_ports: Vec<(String, String)>,
    pub db_cache_hit: bool,
}

/// Bundles `project_path` into a signed snapshot and sends it to whoever
/// joins `room_code` on the signaling server. Returns the id
/// (`project_name@git_commit`) the sender can use to recognize their own
/// send in the UI.
#[tauri::command]
pub async fn share_snapshot(
    app: AppHandle,
    project_path: String,
    room_code: String,
    signaling_url: String,
) -> Result<String, String> {
    let root = PathBuf::from(project_path);
    // create_snapshot shells out to git and walks the filesystem — blocking
    // work that has no business running on the async command's task.
    let snapshot = tauri::async_runtime::spawn_blocking(move || ls_snapshot::create_snapshot(&root, None))
        .await
        .map_err(|e| format!("snapshot task panicked: {e}"))?
        .map_err(|e| e.to_string())?;

    let snapshot_id = format!("{}@{}", snapshot.manifest.project_name, snapshot.manifest.git_commit);

    // Same wire format ls-snapshot's own save_to_file uses: plain JSON,
    // payload bytes riding along as a JSON byte array. Simplest thing that
    // works with the (de)serializers ls-snapshot already ships.
    let bytes = serde_json::to_vec(&snapshot).map_err(|e| e.to_string())?;

    let conn = ls_net::connect_as_sender(&signaling_url, &room_code)
        .await
        .map_err(|e| e.to_string())?;

    ls_net::send_payload(&conn, &bytes, |sent, total| {
        let _ = app.emit("share-progress", Progress { bytes: sent, total });
    })
    .await
    .map_err(|e| e.to_string())?;

    Ok(snapshot_id)
}

/// Receives a snapshot over the P2P channel, verifies its signature
/// (trust-on-first-use — see `ls_security::verify`'s doc comment), and
/// returns the manifest + diff for the review screen. Does **not** unpack
/// `source/` or touch containers; the verified snapshot is held in
/// `AppState` until (and unless) the user clicks Run.
#[tauri::command]
pub async fn receive_snapshot(
    app: AppHandle,
    state: State<'_, AppState>,
    room_code: String,
    signaling_url: String,
) -> Result<IncomingSnapshotInfo, String> {
    let conn = ls_net::connect_as_receiver(&signaling_url, &room_code)
        .await
        .map_err(|e| e.to_string())?;

    let bytes = ls_net::receive_payload(&conn, |received, total| {
        let _ = app.emit("receive-progress", Progress { bytes: received, total });
    })
    .await
    .map_err(|e| e.to_string())?;

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
