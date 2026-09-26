//! Commands for the project-session model (see `project_session`).
//!
//! The rules this file enforces:
//! - A session is created **once** per project: the snapshot is built then,
//!   and kept as the session's artifact. Sending to more devices, and
//!   retrying a failed/expired send, reuse it - the project is only
//!   rebuilt when a folder's commit has actually moved (or when a push
//!   needs a diff against one particular device's marker).
//! - A connection is **ephemeral**: `send_project_session` connects,
//!   transfers, and drops the connection before returning. Nothing here (or
//!   in `AppState`) keeps one open between sends.
//! - Persistence is **opt-in**: a session is written to disk only by
//!   `save_project_session`, never as a side effect of using it.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::commands::{await_recipient_consent, generated_files_for, plan_to_snapshot_inputs, FolderPlanDto, Progress, ReceiverJoined};
use crate::project_session::{DeviceMarker, FolderCommit, ProjectSession};
use crate::state::{AppState, CachedArtifact};

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn new_session_id() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("s{nanos:x}{:x}", SEQ.fetch_add(1, Ordering::Relaxed))
}

/// The commit each folder is at right now. Cheap (`git rev-parse HEAD`), and
/// what decides whether a cached artifact is still current.
async fn current_commits(folders: &[FolderPlanDto]) -> Result<Vec<FolderCommit>, String> {
    let mut out = Vec::with_capacity(folders.len());
    for f in folders {
        let path = f.path.clone();
        let commit = tauri::async_runtime::spawn_blocking(move || ls_snapshot::head_commit(Path::new(&path)))
            .await
            .map_err(|e| format!("git task panicked: {e}"))?
            .map_err(|e| format!("reading the current commit of {}: {e:#}", f.path))?;
        out.push(FolderCommit { path: f.path.clone(), commit });
    }
    Ok(out)
}

fn artifact_key(session_id: &str, parents: &[Option<String>]) -> String {
    let parts: Vec<&str> = parents.iter().map(|p| p.as_deref().unwrap_or("-")).collect();
    format!("{session_id}|{}", parts.join(","))
}

async fn build_artifact(
    state: &AppState,
    folders: &[FolderPlanDto],
    parents: &[Option<String>],
) -> Result<CachedArtifact, String> {
    let (specs, dumps) = plan_to_snapshot_inputs(folders, parents)?;
    let generated = generated_files_for(folders, &specs, None)?;
    state.artifact_builds.fetch_add(1, Ordering::SeqCst);
    let snapshot = tauri::async_runtime::spawn_blocking(move || ls_snapshot::create_snapshot_multi_with(&specs, &dumps, &generated))
        .await
        .map_err(|e| format!("snapshot task panicked: {e}"))?
        .map_err(|e| format!("{e:#}"))?;

    let snapshot_id = format!("{}@{}", snapshot.manifest.project_name, snapshot.manifest.git_commit);
    let commits = folders
        .iter()
        .zip(snapshot.manifest.folders.iter())
        .map(|(f, info)| FolderCommit { path: f.path.clone(), commit: info.git_commit.clone() })
        .collect();
    let bytes = tauri::async_runtime::spawn_blocking(move || serde_json::to_vec(&snapshot))
        .await
        .map_err(|e| format!("serialize task panicked: {e}"))?
        .map_err(|e| e.to_string())?;
    Ok(CachedArtifact { snapshot_id, bytes, commits, built_at: now_rfc3339() })
}

/// The session's artifact, reused if its folders haven't moved since it was
/// built, otherwise rebuilt. `since` names a device whose own marker the
/// artifact's diff should be relative to (a push); `None` is the plain
/// artifact for a first send or retry.
async fn ensure_artifact(
    state: &AppState,
    session: &ProjectSession,
    since: Option<&str>,
) -> Result<Arc<CachedArtifact>, String> {
    let current = current_commits(&session.folders).await?;
    let parents = match since {
        Some(device) => session.parent_commits_for(device),
        None => vec![None; session.folders.len()],
    };
    let key = artifact_key(&session.id, &parents);
    if let Some(cached) = state.artifacts.lock().map_err(|e| e.to_string())?.get(&key) {
        if cached.commits == current {
            return Ok(cached.clone());
        }
    }
    let built = match build_artifact(state, &session.folders, &parents).await {
        Ok(a) => a,
        // A marker commit that no longer exists in the repo (history was
        // rewritten) can't be diffed against - fall back to a full diff
        // rather than failing the whole push.
        Err(e) if parents.iter().any(Option::is_some) => {
            log::warn!("ensure_artifact: diff against the device's marker failed ({e}); using a full diff instead");
            build_artifact(state, &session.folders, &vec![None; session.folders.len()]).await?
        }
        Err(e) => return Err(e),
    };
    let built = Arc::new(built);
    state.artifacts.lock().map_err(|e| e.to_string())?.insert(key, built.clone());
    Ok(built)
}

// ---------- what the frontend sees ----------

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactView {
    pub snapshot_id: String,
    pub commits: Vec<FolderCommit>,
    pub size_bytes: u64,
    pub built_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceView {
    pub key: String,
    pub name: String,
    pub marker: Option<DeviceMarker>,
    pub sends: usize,
    pub last_sent_at: Option<String>,
    /// Already has the session's current version - nothing to push.
    pub up_to_date: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectSessionView {
    pub id: String,
    pub title: String,
    pub folders: Vec<FolderPlanDto>,
    pub created_at: String,
    /// The user has explicitly saved this session (a saved copy exists).
    pub saved: bool,
    pub artifact: Option<ArtifactView>,
    pub devices: Vec<DeviceView>,
}

fn base_artifact(state: &AppState, session: &ProjectSession) -> Option<Arc<CachedArtifact>> {
    let key = artifact_key(&session.id, &vec![None; session.folders.len()]);
    state.artifacts.lock().ok()?.get(&key).cloned()
}

fn build_view(state: &AppState, session: &ProjectSession, current: Option<&[FolderCommit]>) -> ProjectSessionView {
    let artifact = base_artifact(state, session);
    let reference: Vec<FolderCommit> = match (current, &artifact) {
        (Some(c), _) => c.to_vec(),
        (None, Some(a)) => a.commits.clone(),
        (None, None) => Vec::new(),
    };
    ProjectSessionView {
        id: session.id.clone(),
        title: session.title.clone(),
        folders: session.folders.clone(),
        created_at: session.created_at.clone(),
        saved: crate::session_history::find_project(&session.id).ok().flatten().is_some(),
        artifact: artifact.map(|a| ArtifactView {
            snapshot_id: a.snapshot_id.clone(),
            commits: a.commits.clone(),
            size_bytes: a.bytes.len() as u64,
            built_at: a.built_at.clone(),
        }),
        devices: session
            .devices
            .iter()
            .map(|d| DeviceView {
                key: d.key.clone(),
                name: d.name.clone(),
                marker: d.marker.clone(),
                sends: d.history.len(),
                last_sent_at: d.history.last().map(|h| h.sent_at.clone()),
                up_to_date: session.is_up_to_date(&d.key, &reference),
            })
            .collect(),
    }
}

fn get_session(state: &AppState, id: &str) -> Result<ProjectSession, String> {
    state
        .project_sessions
        .lock()
        .map_err(|e| e.to_string())?
        .get(id)
        .cloned()
        .ok_or_else(|| format!("no open session with id {id}"))
}

fn default_title(folders: &[FolderPlanDto]) -> String {
    folders
        .iter()
        .map(|f| {
            Path::new(&f.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| f.path.clone())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------- commands ----------

/// Creates a session for a project - once. Builds the snapshot now and keeps
/// it as the session's artifact.
#[tauri::command]
pub async fn create_project_session(
    state: State<'_, AppState>,
    folders: Vec<FolderPlanDto>,
    title: Option<String>,
) -> Result<ProjectSessionView, String> {
    if folders.is_empty() {
        return Err("at least one folder is required".to_string());
    }
    let title = title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| default_title(&folders));
    let session = ProjectSession::new(new_session_id(), title, folders, now_rfc3339());
    ensure_artifact(&state, &session, None).await?;
    let view = build_view(&state, &session, None);
    state.project_sessions.lock().map_err(|e| e.to_string())?.insert(session.id.clone(), session);
    Ok(view)
}

/// Re-checks the session against the project as it is on disk right now
/// (rebuilding the artifact only if a commit moved) and reports, per device,
/// whether it is behind. This is what "Push update" starts from.
#[tauri::command]
pub async fn refresh_project_session(state: State<'_, AppState>, session_id: String) -> Result<ProjectSessionView, String> {
    let session = get_session(&state, &session_id)?;
    ensure_artifact(&state, &session, None).await?;
    let current = current_commits(&session.folders).await?;
    Ok(build_view(&state, &session, Some(&current)))
}

#[derive(Debug, Clone, Deserialize)]
pub struct SendRequest {
    pub session_id: String,
    /// The room to join (`room_id` for a hosted relay, or a discovered
    /// device's own room) - also the id `share-progress` events carry.
    pub room_code: String,
    pub signaling_url: String,
    /// Ask the recipient to explicitly accept first (discovery-initiated).
    pub require_accept: bool,
    pub sender_name: String,
    /// The device being sent to, if it is already known to this session;
    /// `None` files a new device under a fresh key.
    pub device_key: Option<String>,
    pub device_name: String,
    /// Diff against this device's own last-received marker (a push update)
    /// instead of sending the plain artifact.
    pub since_last: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SendResult {
    pub snapshot_id: String,
    pub device_key: String,
    /// The device already had this version - nothing was sent, and no
    /// connection was made.
    pub up_to_date: bool,
    pub view: ProjectSessionView,
}

/// One transfer: connect, (optionally) get the recipient's consent, send the
/// session's artifact, **disconnect**, then move that one device's marker.
#[tauri::command]
pub async fn send_project_session<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    request: SendRequest,
) -> Result<SendResult, String> {
    let session = get_session(&state, &request.session_id)?;
    let known_key = request.device_key.as_deref().filter(|k| session.device(k).is_some());
    let since = if request.since_last { known_key } else { None };

    let artifact = ensure_artifact(&state, &session, since).await?;
    let device_key = request
        .device_key
        .clone()
        .unwrap_or_else(|| format!("dev-{}", request.room_code));

    if let Some(k) = since {
        if session.is_up_to_date(k, &artifact.commits) {
            let view = build_view(&state, &session, Some(&artifact.commits));
            return Ok(SendResult { snapshot_id: artifact.snapshot_id.clone(), device_key, up_to_date: true, view });
        }
    }

    log::info!("send_project_session: connecting, session={} device={device_key}", session.id);
    let conn = ls_net::connect_as_sender(&request.signaling_url, &request.room_code)
        .await
        .map_err(|e| e.to_string())?;
    if request.require_accept {
        await_recipient_consent(&conn, request.sender_name.clone()).await?;
    }
    let _ = app.emit("receiver-connecting", ReceiverJoined { session_id: request.room_code.clone() });

    let progress_id = request.room_code.clone();
    ls_net::send_payload(&conn, &artifact.bytes, |sent, total| {
        let _ = app.emit("share-progress", Progress { session_id: progress_id.clone(), bytes: sent, total });
    })
    .await
    .map_err(|e| e.to_string())?;
    // Ephemeral by design: the transfer is complete (send_payload waits for
    // the receiver's acknowledgement), so the connection ends here.
    drop(conn);

    let marker = DeviceMarker {
        snapshot_id: artifact.snapshot_id.clone(),
        commits: artifact.commits.clone(),
        sent_at: now_rfc3339(),
    };
    let updated = {
        let mut sessions = state.project_sessions.lock().map_err(|e| e.to_string())?;
        // The session may have been closed while the transfer ran; the send
        // still happened, there is just no session left to record it on.
        sessions.get_mut(&request.session_id).map(|s| {
            s.record_send(&device_key, &request.device_name, marker);
            s.clone()
        })
    };
    let Some(updated) = updated else {
        return Ok(SendResult {
            snapshot_id: artifact.snapshot_id.clone(),
            device_key,
            up_to_date: false,
            view: build_view(&state, &session, Some(&artifact.commits)),
        });
    };
    // A session the user already chose to save stays current on disk - they
    // opted in to that copy existing; an unsaved one is never written.
    if crate::session_history::find_project(&updated.id).ok().flatten().is_some() {
        if let Err(e) = crate::session_history::save_project(&updated) {
            log::warn!("send_project_session: couldn't update the saved copy of {}: {e}", updated.id);
        }
    }
    // A per-device diff artifact has done its job; only the plain one stays cached.
    if since.is_some() {
        let key = artifact_key(&session.id, &session.parent_commits_for(&device_key));
        state.artifacts.lock().map_err(|e| e.to_string())?.remove(&key);
    }
    let view = build_view(&state, &updated, Some(&artifact.commits));
    Ok(SendResult { snapshot_id: artifact.snapshot_id.clone(), device_key, up_to_date: false, view })
}

/// "Save this session": the user's explicit opt-in to storing it on disk.
#[tauri::command]
pub fn save_project_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let session = get_session(&state, &session_id)?;
    crate::session_history::save_project(&session)
}

/// Closes a session in memory. Does **not** write anything, and does not
/// delete an earlier saved copy - the caller decides that separately.
#[tauri::command]
pub fn discard_project_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    state.project_sessions.lock().map_err(|e| e.to_string())?.remove(&session_id);
    let prefix = format!("{session_id}|");
    state.artifacts.lock().map_err(|e| e.to_string())?.retain(|k, _| !k.starts_with(&prefix));
    Ok(())
}

/// Deletes a saved session's stored copy.
#[tauri::command]
pub fn delete_saved_project_session(session_id: String) -> Result<(), String> {
    crate::session_history::remove(&session_id)
}

/// Re-opens a saved session: loads it, and rebuilds its artifact from the
/// project as it is on disk now (the artifact itself is never stored).
#[tauri::command]
pub async fn open_saved_project_session(state: State<'_, AppState>, session_id: String) -> Result<ProjectSessionView, String> {
    if let Some(open) = state.project_sessions.lock().map_err(|e| e.to_string())?.get(&session_id).cloned() {
        return Ok(build_view(&state, &open, None));
    }
    let session = crate::session_history::find_project(&session_id)?
        .ok_or_else(|| "that session isn't saved any more".to_string())?;
    ensure_artifact(&state, &session, None).await.map_err(|e| {
        format!("couldn't reopen \"{}\" - its project folder may have moved or been deleted: {e}", session.title)
    })?;
    let view = build_view(&state, &session, None);
    state.project_sessions.lock().map_err(|e| e.to_string())?.insert(session.id.clone(), session);
    Ok(view)
}

// ---------- this device's own persistent identity ----------

/// A stable random id for *this* device, created once and kept in the OS
/// data dir - announced over mDNS so other devices' sessions can recognize
/// it again across restarts and renames (see `ls_net::DiscoveredPeer::
/// device_id`). Not a secret and not authentication - just an identity.
pub fn load_or_create_device_id_in(dir: &Path) -> Result<String, String> {
    let path = dir.join("device-id");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim();
        if !existing.is_empty() {
            return Ok(existing.to_string());
        }
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let id = format!("{}{}{}", ls_net::generate_room_id(), ls_net::generate_room_id(), ls_net::generate_room_id());
    std::fs::write(&path, &id).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(id)
}

pub fn device_id() -> Result<String, String> {
    let base = dirs::data_dir().ok_or("could not determine the OS data directory")?;
    load_or_create_device_id_in(&base.join("localsync"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_device_id_is_created_once_and_then_stable() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create_device_id_in(dir.path()).unwrap();
        assert_eq!(first.len(), 12);
        assert_eq!(load_or_create_device_id_in(dir.path()).unwrap(), first);
        let other = tempfile::tempdir().unwrap();
        assert_ne!(load_or_create_device_id_in(other.path()).unwrap(), first, "a different device gets a different id");
    }

    #[test]
    fn artifact_keys_separate_devices_with_different_markers() {
        let base = artifact_key("s1", &[None, None]);
        let push = artifact_key("s1", &[Some("abc".into()), None]);
        assert_ne!(base, push);
        assert!(base.starts_with("s1|"));
    }

    #[test]
    fn a_default_title_names_the_folders() {
        let folders = vec![
            FolderPlanDto { path: "/w/xusom-admin".into(), dump: None, compose: None },
            FolderPlanDto { path: "/w/xusom-api".into(), dump: None, compose: None },
        ];
        assert_eq!(default_title(&folders), "xusom-admin, xusom-api");
    }
}
