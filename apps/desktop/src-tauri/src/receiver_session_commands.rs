//! Commands for the receiver-side session model (see `received_session`).
//!
//! Mirrors `session_commands.rs`'s shape and rules, on the receiving half:
//! - Receiving something creates a workspace (or, if a matching session is
//!   armed, updates it in place) - see [`track_received_snapshot`], called
//!   from `commands::finalize_received_snapshot_tracked`.
//! - The diff-review-then-Run gate is **unchanged**: an armed session's
//!   update is held in `state.verified` exactly like any other receive,
//!   waiting for the same explicit Run click - arming only decides which
//!   session/tab a later Run's result attaches to, never whether Run happens
//!   automatically.
//! - A connection is never stored here (or anywhere) past the transfer that
//!   used it - same rule the sender side follows.
//! - Persistence is opt-in: [`save_received_session`] only, never automatic.
//!
//! Arming (`state.armed_updates`) matches a push by *who sent it and what
//! project it is* (`"<sender_pubkey_hex>|<project title>"`), not by
//! connection identity, because a push's connection is ephemeral (see
//! `session_commands`'s own doc comment) and a device reached by pasted room
//! code has no persistent identity at all to key on - only a discovered
//! device's sender key is realistically stable across repeat pushes, which
//! is why "ready for repeat updates" mostly matters for discoverable-mode
//! receiving in practice, even though the arming mechanism itself doesn't
//! care how the connection was made.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::commands::{IncomingSnapshotInfo, RunningSessionInfo};
use crate::received_session::ReceivedSession;
use crate::state::AppState;

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Fresh id for a new `ReceivedSession`. Deliberately independent of
/// `snapshot_id` - see `ReceivedSession::id`'s own doc comment for why a
/// snapshot id can't double as a session id here the way it does nowhere
/// else in this app.
fn new_session_id() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("r{nanos:x}{:x}", SEQ.fetch_add(1, Ordering::Relaxed))
}

/// The `state.armed_updates` key for a given sender/project pair.
fn armed_key(sender_pubkey_hex: &str, title: &str) -> String {
    format!("{sender_pubkey_hex}|{title}")
}

// ---------- landing a push (called from commands::finalize_received_snapshot_tracked) ----------

/// What a push produced for the receiver-session model: either a fresh
/// workspace, or an update landing on one that was explicitly armed for it.
/// Either way, the diff-review-then-Run gate `finalize_received_snapshot`
/// already enforces is untouched - this only decides which `ReceivedSession`
/// the push is filed under.
#[derive(Debug, Clone, PartialEq)]
pub enum ReceivedSessionOutcome {
    Created(String),
    Updated(String),
}

impl ReceivedSessionOutcome {
    pub fn session_id(&self) -> &str {
        match self {
            ReceivedSessionOutcome::Created(id) | ReceivedSessionOutcome::Updated(id) => id,
        }
    }
}

/// Carried by the `"session-update-available"` event: an armed session just
/// received a fresh push. Additive alongside the plain `IncomingSnapshotInfo`
/// every caller already gets back directly - this is purely so the frontend
/// can route the update to the *existing* tab instead of only ever opening a
/// new one.
#[derive(Clone, Serialize)]
pub struct SessionUpdateAvailable {
    pub session_id: String,
    pub info: IncomingSnapshotInfo,
}

/// Files a just-verified push under the receiver-session model: creates a
/// fresh [`ReceivedSession`], unless `state.armed_updates` has an entry for
/// this exact sender+project (`armed_key`), in which case the push updates
/// that existing session instead - the arming entry is consumed either way
/// it's found, so a second unrelated push doesn't also land on it.
pub fn track_received_snapshot(
    state: &AppState,
    manifest: &ls_snapshot::Manifest,
    snapshot_id: &str,
    sender_pubkey_hex: &str,
) -> Result<ReceivedSessionOutcome, String> {
    let now = now_rfc3339();
    let key = armed_key(sender_pubkey_hex, &manifest.project_name);

    let armed_session_id = state.armed_updates.lock().map_err(|e| e.to_string())?.remove(&key);
    if let Some(session_id) = armed_session_id {
        let mut sessions = state.received_sessions.lock().map_err(|e| e.to_string())?;
        if let Some(session) = sessions.get_mut(&session_id) {
            session.record_update(snapshot_id.to_string(), manifest.git_commit.clone(), String::new(), now);
            return Ok(ReceivedSessionOutcome::Updated(session_id));
        }
        // The armed session was discarded/closed between arming and this
        // push arriving - fall through and treat it as a fresh receive
        // rather than losing the push entirely.
    }

    let id = new_session_id();
    let session = ReceivedSession::new(
        id.clone(),
        manifest.project_name.clone(),
        sender_pubkey_hex.to_string(),
        snapshot_id.to_string(),
        manifest.git_commit.clone(),
        now,
    );
    state.received_sessions.lock().map_err(|e| e.to_string())?.insert(id.clone(), session);
    Ok(ReceivedSessionOutcome::Created(id))
}

// ---------- what the frontend sees ----------

#[derive(Debug, Clone, Serialize)]
pub struct ReceivedSessionView {
    pub id: String,
    pub title: String,
    pub sender_pubkey_hex: String,
    pub snapshot_id: String,
    pub git_commit: String,
    pub work_dir: String,
    pub created_at: String,
    pub last_received_at: String,
    /// The user has explicitly saved this session (a saved copy exists).
    pub saved: bool,
    /// This session is armed to receive its next push in place (see
    /// `state.armed_updates`).
    pub armed: bool,
    /// This session's current version has a live entry in `state.sessions`
    /// (i.e. `run_received_session` brought it up and it hasn't been
    /// stopped).
    pub running: bool,
}

fn is_running(state: &AppState, session: &ReceivedSession) -> Result<bool, String> {
    let key = ls_containers::compose_project_name(&session.title, &session.git_commit);
    Ok(state.sessions.lock().map_err(|e| e.to_string())?.contains_key(&key))
}

fn build_view(state: &AppState, session: &ReceivedSession) -> Result<ReceivedSessionView, String> {
    let armed = state
        .armed_updates
        .lock()
        .map_err(|e| e.to_string())?
        .values()
        .any(|id| id == &session.id);
    Ok(ReceivedSessionView {
        id: session.id.clone(),
        title: session.title.clone(),
        sender_pubkey_hex: session.sender_pubkey_hex.clone(),
        snapshot_id: session.snapshot_id.clone(),
        git_commit: session.git_commit.clone(),
        work_dir: session.work_dir.clone(),
        created_at: session.created_at.clone(),
        last_received_at: session.last_received_at.clone(),
        saved: crate::session_history::find_received(&session.id).ok().flatten().is_some(),
        armed,
        running: is_running(state, session)?,
    })
}

fn get_session(state: &AppState, id: &str) -> Result<ReceivedSession, String> {
    state
        .received_sessions
        .lock()
        .map_err(|e| e.to_string())?
        .get(id)
        .cloned()
        .ok_or_else(|| format!("no open received session with id {id}"))
}

// ---------- commands ----------

#[derive(Serialize)]
pub struct RunReceivedResult {
    /// The `ReceivedSession::id` this run belongs to - not the same value as
    /// `running.session_id` (`RunningSession::compose_project_name`, what
    /// `stop_session` takes).
    pub id: String,
    #[serde(flatten)]
    pub running: RunningSessionInfo,
}

/// Runs a received session's current version: the common case (the
/// `VerifiedSnapshot` is still held in `state.verified` from the receive
/// that either created this session or is being run for the first time)
/// unpacks and brings it up via `ls_containers::run_snapshot`, exactly like
/// `commands::run_snapshot` does for a plain receive. If nothing is held any
/// more (e.g. this is a saved session reopened after an app restart), the
/// session's own `compose_dir` from a prior successful run is brought back
/// up directly via `ls_containers::run_existing`, with no re-unpack and no
/// `VerifiedSnapshot` needed at all. Either way, the resulting live session
/// is tracked in `state.sessions` under `RunningSession::compose_project_name`
/// exactly like `run_snapshot` already does, so the existing, unmodified
/// `stop_session` keeps working against it with no changes of its own.
#[tauri::command]
pub async fn run_received_session<R: tauri::Runtime>(
    _app: AppHandle<R>,
    state: State<'_, AppState>,
    session_id: String,
    work_dir: String,
) -> Result<RunReceivedResult, String> {
    let session = get_session(&state, &session_id)?;

    let held = state.verified.lock().map_err(|e| e.to_string())?.remove(&session.snapshot_id);

    let session_out = match held {
        Some(verified) => {
            match ls_containers::run_snapshot(&verified, &PathBuf::from(&work_dir)).await {
                Ok(running) => running,
                Err(e) => {
                    // Same retry-friendliness commands::run_snapshot gives a
                    // plain receive: an environment-level Run failure must
                    // not lose the held snapshot.
                    state.verified.lock().map_err(|e| e.to_string())?.insert(session.snapshot_id.clone(), verified);
                    return Err(format!("{e:#}"));
                }
            }
        }
        None => {
            if session.compose_dir.is_empty() {
                return Err(
                    "this session was never fully run before closing, or its files are no longer \
                     on disk - receive it again"
                        .to_string(),
                );
            }
            let compose_root = PathBuf::from(&session.compose_dir);
            if !tokio::fs::try_exists(&compose_root).await.unwrap_or(false) {
                return Err(
                    "this session was never fully run before closing, or its files are no longer \
                     on disk - receive it again"
                        .to_string(),
                );
            }
            ls_containers::run_existing(&compose_root, &session.title, &session.git_commit)
                .await
                .map_err(|e| format!("{e:#}"))?
        }
    };

    let info = RunningSessionInfo {
        session_id: session_out.compose_project_name.clone(),
        project_name: session_out.project_name.clone(),
        service_ports: session_out.service_ports.clone(),
        db_cache_hit: session_out.db_cache_hit,
    };
    let compose_dir = session_out.compose_dir.display().to_string();

    state.sessions.lock().map_err(|e| e.to_string())?.insert(info.session_id.clone(), session_out);

    let updated = {
        let mut sessions = state.received_sessions.lock().map_err(|e| e.to_string())?;
        let s = sessions
            .get_mut(&session_id)
            .ok_or_else(|| format!("no open received session with id {session_id}"))?;
        s.record_run(work_dir, compose_dir);
        s.clone()
    };
    // Same "a saved session stays current on disk as it's used" rule the
    // sender side follows.
    if crate::session_history::find_received(&updated.id).ok().flatten().is_some() {
        if let Err(e) = crate::session_history::save_received(&updated) {
            log::warn!("run_received_session: couldn't update the saved copy of {}: {e}", updated.id);
        }
    }

    Ok(RunReceivedResult { id: session_id, running: info })
}

/// "Save this session" - receiver side.
#[tauri::command]
pub fn save_received_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let session = get_session(&state, &session_id)?;
    crate::session_history::save_received(&session)
}

/// Closes a session in memory. Does **not** delete an existing saved copy -
/// the caller decides that separately, same contract as
/// `session_commands::discard_project_session`. Also clears any arming that
/// pointed at it - an armed slot for a session that no longer exists in
/// memory would just be a silent dead end for the next push to fall into.
#[tauri::command]
pub fn discard_received_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    state.received_sessions.lock().map_err(|e| e.to_string())?.remove(&session_id);
    state.armed_updates.lock().map_err(|e| e.to_string())?.retain(|_, v| v != &session_id);
    Ok(())
}

/// Deletes a saved received session's stored copy.
#[tauri::command]
pub fn delete_saved_received_session(session_id: String) -> Result<(), String> {
    crate::session_history::remove(&session_id)
}

/// Re-opens a saved received session: loads it and files it back into
/// memory. Does **not** run anything - Run is still a separate, explicit
/// step (`run_received_session`).
#[tauri::command]
pub fn open_saved_received_session(state: State<'_, AppState>, session_id: String) -> Result<ReceivedSessionView, String> {
    if let Some(open) = state.received_sessions.lock().map_err(|e| e.to_string())?.get(&session_id).cloned() {
        return build_view(&state, &open);
    }
    let session = crate::session_history::find_received(&session_id)?
        .ok_or_else(|| "that session isn't saved any more".to_string())?;
    let view = build_view(&state, &session)?;
    state.received_sessions.lock().map_err(|e| e.to_string())?.insert(session.id.clone(), session);
    Ok(view)
}

/// Arms `session_id` to have its *next* push land on it in place, instead of
/// opening a new session. Keyed by this session's own sender+title, not by
/// any connection - see this module's own doc comment.
#[tauri::command]
pub fn arm_received_session_for_update(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let session = get_session(&state, &session_id)?;
    let key = armed_key(&session.sender_pubkey_hex, &session.title);
    state.armed_updates.lock().map_err(|e| e.to_string())?.insert(key, session_id);
    Ok(())
}

/// Undoes `arm_received_session_for_update` - removes whichever arming entry
/// (there should be at most one) points at `session_id`.
#[tauri::command]
pub fn disarm_received_session_for_update(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    state.armed_updates.lock().map_err(|e| e.to_string())?.retain(|_, v| v != &session_id);
    Ok(())
}
