//! Commands for the receiver-side session model (see `received_session`).
//!
//! Mirrors `session_commands.rs`'s shape and rules, on the receiving half:
//! - Receiving something creates a workspace - or, when it's a project this
//!   device already has from the same sender, updates that one in place
//!   ("Update to review") - see [`track_received_snapshot`], called from
//!   `commands::finalize_received_snapshot_tracked`.
//! - The diff-review-then-Run gate is **unchanged**: an update is held in
//!   `state.verified` exactly like any other receive, waiting for the same
//!   explicit click - matching only decides which session it lands on.
//! - An update to a session that already ran is *applied*
//!   ([`apply_received_update`]): the running version is taken down, the new
//!   one built, started and waited on until healthy, and only then is the
//!   previous version let go. Until then [`rollback_received_update`] returns
//!   to it.
//! - A connection is never stored here (or anywhere) past the transfer that
//!   used it - same rule the sender side follows.
//! - Every session is persisted the moment it exists and whenever it changes
//!   ([`persist`]).
//!
//! A push is matched by *who sent it and which project it is* - the sender's
//! key and `Manifest::project_id` (the sender's own session id), or the title
//! for snapshots from before project ids existed - never by connection
//! identity: a push's connection is ephemeral (see `session_commands`'s own
//! doc comment), so there is none to key on.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::commands::{IncomingSnapshotInfo, RunningSessionInfo};
use crate::received_session::{best_match, Arrival, DbChoice, ReceivedDatabase, ReceivedSession, UpdateVolume};
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

/// The sender's name in this device's known peers, empty if they aren't one
/// (or the store can't be read - this is only ever shown, never trusted).
fn known_peer_name(pubkey: &[u8; 32]) -> String {
    ls_security::KnownPeers::load_default()
        .ok()
        .and_then(|peers| peers.find(pubkey).map(|p| p.name.clone()))
        .unwrap_or_default()
}

fn pubkey_bytes(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

// ---------- landing a push (called from commands::finalize_received_snapshot_tracked) ----------

/// What a push produced for the receiver-session model: either a fresh
/// workspace, or an update landing on the one this device already has of
/// that project. Either way, the diff-review-then-Run gate
/// `finalize_received_snapshot` already enforces is untouched - this only
/// decides which `ReceivedSession` the push is filed under.
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

/// Carried by the `"session-update-available"` event: an existing session
/// just received a fresh push. Additive alongside the plain
/// `IncomingSnapshotInfo` every caller already gets back directly - this is
/// purely so the frontend can route the update to the *existing* tab instead
/// of only ever opening a new one.
#[derive(Clone, Serialize)]
pub struct SessionUpdateAvailable {
    pub session_id: String,
    pub info: IncomingSnapshotInfo,
}

/// Every saved received session that isn't open in memory - after a restart,
/// that is all of them. A history that can't be read only means nothing
/// saved is matched.
fn saved_sessions_not_open(open: &[ReceivedSession]) -> Vec<ReceivedSession> {
    match crate::session_history::load() {
        Ok(entries) => entries
            .into_iter()
            .filter_map(|e| e.received)
            .filter(|s| !open.iter().any(|o| o.id == s.id))
            .collect(),
        Err(e) => {
            log::warn!("couldn't read the saved sessions ({e}); matching open ones only");
            Vec::new()
        }
    }
}

/// The id of the session a push from this sender of this project lands on
/// (see `received_session::best_match`): an open one, or a saved one that
/// isn't open right now, which is opened for it.
fn open_matching_session(state: &AppState, sender_pubkey_hex: &str, project_id: Option<&str>, title: &str) -> Result<Option<String>, String> {
    let open: Vec<ReceivedSession> = state.received_sessions.lock().map_err(|e| e.to_string())?.values().cloned().collect();
    let saved = saved_sessions_not_open(&open);
    let Some(found) = best_match(open.iter().chain(saved.iter()), sender_pubkey_hex, project_id, title).cloned() else {
        return Ok(None);
    };
    let id = found.id.clone();
    state.received_sessions.lock().map_err(|e| e.to_string())?.entry(id.clone()).or_insert(found);
    Ok(Some(id))
}

/// A session that ran before its database volume and dump hash were
/// recorded: read both off the compose file it ran from (`apply_policy` pins
/// the volume, which is named after the dump hash), so an update to it still
/// asks before the receiver's data is replaced.
fn backfill_db_volume(session: &mut ReceivedSession) {
    if !session.db_volume.is_empty() || session.compose_dir.is_empty() {
        return;
    }
    let Some(volume) = ls_containers::pinned_volumes_in(Path::new(&session.compose_dir)).into_iter().next() else { return };
    if session.db_seed_hash.is_empty() {
        session.db_seed_hash = volume.strip_prefix("localsync-db-").unwrap_or_default().to_string();
    }
    session.db_volume = volume;
}

/// Files a just-verified push under the receiver-session model: if this
/// device already has the project from this sender (same key, and the same
/// `project_id` - or title, for a snapshot without one), the push lands on
/// that session as an update, even one only saved on disk; otherwise it
/// becomes a fresh [`ReceivedSession`].
pub fn track_received_snapshot(
    state: &AppState,
    manifest: &ls_snapshot::Manifest,
    snapshot_id: &str,
    sender_pubkey_hex: &str,
) -> Result<ReceivedSessionOutcome, String> {
    let arrival = Arrival::from_manifest(manifest, snapshot_id, known_peer_name(&manifest.sender_pubkey), now_rfc3339());

    if let Some(id) = open_matching_session(state, sender_pubkey_hex, manifest.project_id.as_deref(), &manifest.project_name)? {
        let (updated, superseded) = {
            let mut sessions = state.received_sessions.lock().map_err(|e| e.to_string())?;
            let session = sessions.get_mut(&id).ok_or_else(|| format!("no open received session with id {id}"))?;
            backfill_db_volume(session);
            let old_snapshot = session.snapshot_id.clone();
            session.land_update(arrival);
            let updated = session.clone();
            // The version it moved off can't be run from this session any
            // more: its held snapshot goes, unless another session is at it.
            let superseded = (old_snapshot != updated.snapshot_id && !sessions.values().any(|s| s.snapshot_id == old_snapshot))
                .then_some(old_snapshot);
            (updated, superseded)
        };
        if let Some(old) = superseded {
            state.verified.lock().map_err(|e| e.to_string())?.remove(&old);
        }
        persist(&updated);
        return Ok(ReceivedSessionOutcome::Updated(id));
    }

    let id = new_session_id();
    let session = ReceivedSession::from_arrival(id.clone(), manifest.project_name.clone(), sender_pubkey_hex.to_string(), arrival);
    persist(&session);
    state.received_sessions.lock().map_err(|e| e.to_string())?.insert(id.clone(), session);
    Ok(ReceivedSessionOutcome::Created(id))
}

/// Answers a sender's `BaseCheck`: the per-folder commits of the version this
/// device holds of `project_id` from `sender_pubkey_hex` (the session an
/// update from them would land on), `None` if there is no such session.
/// "Holds" is the last version received - applied or still waiting - or, after
/// a Roll back, the version rolled back to.
pub fn held_base_commits(state: &AppState, sender_pubkey_hex: &str, project_id: &str) -> Option<Vec<String>> {
    let open: Vec<ReceivedSession> = state.received_sessions.lock().ok()?.values().cloned().collect();
    let saved = saved_sessions_not_open(&open);
    open.iter()
        .chain(saved.iter())
        .filter(|s| s.sender_pubkey_hex == sender_pubkey_hex && s.project_id.as_deref() == Some(project_id))
        .max_by(|a, b| a.last_received_at.cmp(&b.last_received_at))
        .map(ReceivedSession::held_commits)
}

/// Every received session is kept on disk from the moment it exists and
/// whenever it changes, so it survives a restart and shows on Home. A
/// failure to write is logged, never allowed to fail the receive itself.
fn persist(session: &ReceivedSession) {
    if let Err(e) = crate::session_history::save_received(session) {
        log::warn!("couldn't persist received session {}: {e}", session.id);
    }
}

/// Changes an open session's record and saves it; returns the result.
fn update_session(state: &AppState, session_id: &str, change: impl FnOnce(&mut ReceivedSession)) -> Result<ReceivedSession, String> {
    let updated = {
        let mut sessions = state.received_sessions.lock().map_err(|e| e.to_string())?;
        let s = sessions.get_mut(session_id).ok_or_else(|| format!("no open received session with id {session_id}"))?;
        change(s);
        s.clone()
    };
    persist(&updated);
    Ok(updated)
}

// ---------- what the frontend sees ----------

#[derive(Debug, Clone, Serialize)]
pub struct ReceivedSessionView {
    pub id: String,
    /// The name the person gave the session, or the project's name if none.
    pub name: String,
    pub title: String,
    pub sender_pubkey_hex: String,
    /// Who sent it: their name in this device's known peers, else the one
    /// recorded when the last push arrived; empty if neither knows.
    pub sender_name: String,
    pub snapshot_id: String,
    pub git_commit: String,
    /// The databases the current version carries a dump of.
    pub databases: Vec<ReceivedDatabase>,
    pub work_dir: String,
    pub created_at: String,
    pub last_received_at: String,
    /// RFC3339 of the last Run that came up / the last Stop or failed Run;
    /// empty if none. `last_exit`: "stopped", "failed", or empty.
    pub last_run_at: String,
    pub last_stopped_at: String,
    pub last_exit: String,
    /// The user has explicitly saved this session (a saved copy exists).
    pub saved: bool,
    /// This session's current version has running containers right now -
    /// asked of Podman, so it's right after an app restart too (when nothing
    /// from the original Run is left in memory).
    pub running: bool,
    /// When running: the ("service", "host:container") ports it published,
    /// read from the compose file it was brought up from. `None` when not
    /// running (or the file is gone).
    pub service_ports: Option<Vec<(String, String)>>,
    /// When running and started by *this* app process: whether the database
    /// volume was reused. `None` when unknown (e.g. after a restart - it isn't
    /// recorded anywhere that survives one).
    pub db_cache_hit: Option<bool>,
    /// An update waiting to be applied, if any.
    pub update: Option<UpdateView>,
}

/// What the receiver page needs of a [`crate::received_session::PendingUpdate`].
#[derive(Debug, Clone, Serialize)]
pub struct UpdateView {
    /// The sender's database changed: ask whether to keep this device's data.
    pub db_changed: bool,
    /// The commit of the version it replaces.
    pub previous_commit: String,
    /// That version is running (or was, when Apply took it down): Apply
    /// restarts rather than starts.
    pub previous_running: bool,
}

/// Asks Podman whether this session's compose project has running
/// containers. If Podman can't be asked at all, falls back to what this
/// process itself started (the pre-restart-aware behavior) rather than
/// failing the whole view.
fn is_running(state: &AppState, session: &ReceivedSession) -> Result<bool, String> {
    if let Some((_, prev_commit)) = &session.previous_run {
        if ls_containers::project_running(session.previous_title(), prev_commit).unwrap_or(false) {
            return Ok(true);
        }
    }
    match ls_containers::project_running(&session.title, &session.git_commit) {
        Ok(running) => Ok(running),
        Err(e) => {
            log::warn!("is_running: couldn't ask podman about {} ({e:#}); using in-memory state", session.id);
            let key = ls_containers::compose_project_name(&session.title, &session.git_commit);
            Ok(state.sessions.lock().map_err(|e| e.to_string())?.contains_key(&key))
        }
    }
}

fn build_view(state: &AppState, session: &ReceivedSession) -> Result<ReceivedSessionView, String> {
    let running = is_running(state, session)?;
    let (service_ports, db_cache_hit) = if running {
        let key = ls_containers::compose_project_name(&session.title, &session.git_commit);
        let live = state.sessions.lock().map_err(|e| e.to_string())?.get(&key).map(|r| (r.service_ports.clone(), r.db_cache_hit));
        match live {
            Some((ports, hit)) => (Some(ports), Some(hit)),
            None => (ls_containers::declared_service_ports(Path::new(&session.compose_dir)).ok(), None),
        }
    } else {
        (None, None)
    };
    let sender_name = pubkey_bytes(&session.sender_pubkey_hex)
        .map(|key| known_peer_name(&key))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| session.sender_name.clone());
    let update = session.pending_update.as_ref().map(|p| UpdateView {
        db_changed: p.db_changed,
        previous_commit: p.previous.git_commit.clone(),
        previous_running: p.previous_was_running || running,
    });
    Ok(ReceivedSessionView {
        id: session.id.clone(),
        name: if session.name.trim().is_empty() { session.title.clone() } else { session.name.clone() },
        title: session.title.clone(),
        sender_pubkey_hex: session.sender_pubkey_hex.clone(),
        sender_name,
        snapshot_id: session.snapshot_id.clone(),
        git_commit: session.git_commit.clone(),
        databases: session.databases.clone(),
        work_dir: session.work_dir.clone(),
        created_at: session.created_at.clone(),
        last_received_at: session.last_received_at.clone(),
        last_run_at: session.last_run_at.clone(),
        last_stopped_at: session.last_stopped_at.clone(),
        last_exit: session.last_exit.clone(),
        saved: crate::session_history::find_received(&session.id).ok().flatten().is_some(),
        running,
        service_ports,
        db_cache_hit,
        update,
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
/// With an update waiting, Run is applying it ([`apply_received_update`],
/// keeping this device's data).
#[tauri::command]
pub async fn run_received_session<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    session_id: String,
    work_dir: String,
) -> Result<RunReceivedResult, String> {
    let session = get_session(&state, &session_id)?;

    // Live progress for the session page's Logs panel: every line the Run
    // writes to the provisioning log (the readiness check, podman-compose's
    // image pulls and container starts, the database health wait) goes out
    // as a `run-progress` event tagged with this session's snapshot id -
    // the same events and tagging `commands::run_snapshot` uses.
    let tail = tauri::async_runtime::spawn(crate::commands::tail_provisioning_log(app.clone(), session.snapshot_id.clone()));
    let result = run_received_session_inner(&state, &session, &session_id, work_dir).await;
    // Let the tailer pick up the Run's last lines before it stops.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    tail.abort();
    if result.is_err() {
        let _ = update_session(&state, &session_id, |s| s.record_failed(now_rfc3339()));
    }
    result
}

async fn run_received_session_inner(
    state: &AppState,
    session: &ReceivedSession,
    session_id: &str,
    work_dir: String,
) -> Result<RunReceivedResult, String> {
    if session.pending_update.is_some() {
        return apply_update_inner(state, session_id, DbChoice::Keep, Some(work_dir)).await;
    }
    let session_id = session_id.to_string();

    // An update landed while the previous version ran: take it down first,
    // or it keeps the ports this version needs.
    stop_previous_run(state, &session_id).await?;

    // Before unpacking: even a failed Run leaves files there, and the Storage
    // view must still find them after this session is closed.
    crate::storage_commands::remember_work_dirs([work_dir.as_str()]);

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

    ensure_started(&session_out).await?;

    let info = RunningSessionInfo {
        session_id: session_out.compose_project_name.clone(),
        project_name: session_out.project_name.clone(),
        service_ports: session_out.service_ports.clone(),
        db_cache_hit: session_out.db_cache_hit,
    };
    let compose_dir = session_out.compose_dir.display().to_string();
    let db_volume = ls_containers::pinned_volumes_in(&session_out.compose_dir).into_iter().next().unwrap_or_default();

    state.sessions.lock().map_err(|e| e.to_string())?.insert(info.session_id.clone(), session_out);

    update_session(state, &session_id, |s| {
        s.record_run(work_dir, compose_dir);
        s.db_volume = db_volume;
        s.record_started(now_rfc3339());
    })?;

    Ok(RunReceivedResult { id: session_id, running: info })
}

/// podman-compose exits 0 even when a container couldn't be created or
/// started (a port already in use, an image that failed to build or pull),
/// so "it ran" isn't "it's up": a service with no started container fails
/// the Run instead of showing Running with nothing behind it. Same check
/// the compose wizard's test run makes.
async fn ensure_started(running: &ls_containers::RunningSession) -> Result<(), String> {
    let unstarted = ls_containers::services_not_started(running).await;
    if unstarted.is_empty() {
        return Ok(());
    }
    if let Err(e) = ls_containers::stop_session(running).await {
        log::warn!("run_received_session: cleanup after a service failed to start also failed: {e:#}");
    }
    let names = unstarted.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", ");
    Err(format!(
        "{names} could not be started, so the project isn't running - usually a port it needs is already in use \
         on this computer, or its image failed to build or download. The Logs panel shows podman-compose's output."
    ))
}

/// Applies the update waiting on a session: takes the version that's running
/// down (its folder and record stay), builds and starts the new one, and
/// waits until every service is healthy - only then is the previous version
/// let go. `db_choice` answers "the sender's database changed": "keep" (the
/// default, also for `None`) runs the new version on the volume the previous
/// one used, so this device's data stays; "replace" imports the sender's
/// dump fresh. An unchanged dump always keeps the volume. If the new version
/// doesn't come up healthy it is taken down again, the update stays waiting
/// (Apply can be tried again) and [`rollback_received_update`] returns to the
/// previous version. On a session that isn't running this is its Run.
#[tauri::command]
pub async fn apply_received_update<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    session_id: String,
    db_choice: Option<String>,
) -> Result<RunReceivedResult, String> {
    let choice = DbChoice::parse(db_choice.as_deref())?;
    let session = get_session(&state, &session_id)?;
    // The same live Logs-panel feed `run_received_session` gives.
    let tail = tauri::async_runtime::spawn(crate::commands::tail_provisioning_log(app.clone(), session.snapshot_id.clone()));
    let result = apply_update_inner(&state, &session_id, choice, None).await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    tail.abort();
    if result.is_err() {
        let _ = update_session(&state, &session_id, |s| s.record_failed(now_rfc3339()));
    }
    result
}

/// Shown when the update's snapshot is gone: it was only ever held in memory.
const UPDATE_NOT_HELD: &str = "This update is no longer here (LocalSync was restarted since it arrived) - ask the sender to send it again.";

async fn apply_update_inner(state: &AppState, session_id: &str, choice: DbChoice, work_dir: Option<String>) -> Result<RunReceivedResult, String> {
    let session = get_session(state, session_id)?;
    if session.pending_update.is_none() {
        return Err("There's no update waiting on this session.".to_string());
    }
    let work_dir = work_dir.filter(|w| !w.trim().is_empty()).unwrap_or_else(|| session.work_dir.clone());
    let verified = state
        .verified
        .lock()
        .map_err(|e| e.to_string())?
        .remove(&session.snapshot_id)
        .ok_or_else(|| UPDATE_NOT_HELD.to_string())?;

    let running = match bring_up_update(state, &session, choice, &work_dir, &verified).await {
        Ok(running) => running,
        Err(e) => {
            // Held again, so Apply can be tried again.
            state.verified.lock().map_err(|e| e.to_string())?.insert(session.snapshot_id.clone(), verified);
            return Err(e);
        }
    };

    let info = RunningSessionInfo {
        session_id: running.compose_project_name.clone(),
        project_name: running.project_name.clone(),
        service_ports: running.service_ports.clone(),
        db_cache_hit: running.db_cache_hit,
    };
    let compose_dir = running.compose_dir.display().to_string();
    let db_volume = ls_containers::pinned_volumes_in(&running.compose_dir).into_iter().next().unwrap_or_default();
    state.sessions.lock().map_err(|e| e.to_string())?.insert(info.session_id.clone(), running);
    update_session(state, session_id, |s| s.finish_update(work_dir, compose_dir, db_volume, now_rfc3339()))?;
    Ok(RunReceivedResult { id: session_id.to_string(), running: info })
}

/// Everything between "Apply" and "it's healthy": stop the running version,
/// pick the database volume, unpack/build/start the new one, wait for health.
/// On `Err` nothing of the new version is left running.
async fn bring_up_update(
    state: &AppState,
    session: &ReceivedSession,
    choice: DbChoice,
    work_dir: &str,
    verified: &ls_security::VerifiedSnapshot,
) -> Result<ls_containers::RunningSession, String> {
    // The version running now gives up its ports. Its folder and record stay
    // (Roll back needs them), and whether it ran is saved first, so a Roll
    // back - even after a crash right here - brings it back up.
    let previous_title = session.previous_title();
    let running_now = session
        .previous_run
        .as_ref()
        .is_some_and(|(_, commit)| ls_containers::project_running(previous_title, commit).unwrap_or(false));
    update_session(state, &session.id, |s| {
        if let Some(p) = &mut s.pending_update {
            p.previous_was_running |= running_now;
        }
    })?;
    if let (true, Some((dir, commit))) = (running_now, &session.previous_run) {
        ls_containers::stop_project(Path::new(dir), previous_title, commit)
            .await
            .map_err(|e| format!("Couldn't stop the version that's running: {e:#}"))?;
        state.sessions.lock().map_err(|e| e.to_string())?.remove(&ls_containers::compose_project_name(previous_title, commit));
    }

    let volume = match session.update_volume(choice) {
        UpdateVolume::Default => None,
        UpdateVolume::Reuse(volume) => Some(volume),
        UpdateVolume::Fresh => {
            // Its own volume may already exist from an earlier run of this
            // same dump; emptied, so the dump really is imported again.
            let fresh = ls_containers::db_volume_name(&verified.snapshot().manifest.db_seed_hash);
            if ls_containers::volume_exists(&fresh).await.unwrap_or(false) {
                ls_containers::volume_remove_unused(&fresh)
                    .await
                    .map_err(|e| format!("Couldn't replace the database - another session may be using it: {e:#}"))?;
            }
            None
        }
    };

    crate::storage_commands::remember_work_dirs([work_dir]);
    let running = ls_containers::run_snapshot_on_volume(verified, Path::new(work_dir), volume.as_deref())
        .await
        .map_err(|e| format!("{e:#}"))?;
    ensure_started(&running).await?;
    if let Err(reason) = ls_containers::wait_until_healthy(&running).await {
        if let Err(e) = ls_containers::stop_session(&running).await {
            log::warn!("apply_received_update: cleanup after a failed health check also failed: {e:#}");
        }
        return Err(reason);
    }
    Ok(running)
}

/// Drops the update waiting on a session and returns to the version it
/// would have replaced - after a new version that didn't come up healthy, or
/// to skip an update. That version is brought back up if Apply had taken it
/// down while it ran; one still running is left as it is.
#[tauri::command]
pub async fn rollback_received_update(state: State<'_, AppState>, session_id: String) -> Result<ReceivedSessionView, String> {
    let session = get_session(&state, &session_id)?;
    let Some(pending) = session.pending_update.clone() else {
        return Err("There's no update to roll back on this session.".to_string());
    };
    let restored = update_session(&state, &session_id, |s| {
        s.roll_back();
    })?;
    // The update's snapshot can't be run from this session any more.
    state.verified.lock().map_err(|e| e.to_string())?.remove(&session.snapshot_id);

    let restart = pending.previous_was_running
        && !restored.compose_dir.is_empty()
        && !ls_containers::project_running(&restored.title, &restored.git_commit).unwrap_or(false);
    if restart {
        let brought_back = async {
            let running = ls_containers::run_existing(Path::new(&restored.compose_dir), &restored.title, &restored.git_commit)
                .await
                .map_err(|e| format!("{e:#}"))?;
            ensure_started(&running).await?;
            Ok::<_, String>(running)
        }
        .await;
        match brought_back {
            Ok(running) => {
                state.sessions.lock().map_err(|e| e.to_string())?.insert(running.compose_project_name.clone(), running);
                update_session(&state, &session_id, |s| s.record_started(now_rfc3339()))?;
            }
            Err(e) => {
                let _ = update_session(&state, &session_id, |s| s.record_failed(now_rfc3339()));
                return Err(format!("Rolled back, but the previous version didn't start again: {e}"));
            }
        }
    }
    build_view(&state, &get_session(&state, &session_id)?)
}

/// Stops a received session's containers by its compose project directly -
/// works for a session reopened after an app restart, when the
/// `RunningSession` handle from the original Run is long gone (which
/// `commands::stop_session` needs). Forgets that handle too if this process
/// still has it.
#[tauri::command]
pub async fn stop_received_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let session = get_session(&state, &session_id)?;
    if session.compose_dir.is_empty() {
        // Only the version before an update ran (and may still be up).
        if session.previous_run.is_some() {
            stop_previous_run(&state, &session_id).await?;
            update_session(&state, &session_id, |s| s.record_stopped(now_rfc3339()))?;
            return Ok(());
        }
        return Err("this session hasn't been run, so there's nothing to stop".to_string());
    }
    ls_containers::stop_project(Path::new(&session.compose_dir), &session.title, &session.git_commit)
        .await
        .map_err(|e| format!("{e:#}"))?;
    let key = ls_containers::compose_project_name(&session.title, &session.git_commit);
    state.sessions.lock().map_err(|e| e.to_string())?.remove(&key);
    update_session(&state, &session_id, |s| s.record_stopped(now_rfc3339()))?;
    Ok(())
}

/// Takes down every container this session may have running - its current
/// version and, after an update, the version before it - the same teardown
/// Stop does (`podman-compose down`: containers and network). The database
/// volume is kept: it's the warm cache keyed by the dump, not the session's.
/// Removing a session from the app without this would leave its containers
/// running with nothing in the UI pointing at them any more.
pub(crate) async fn stop_session_containers(state: &AppState, session: &ReceivedSession) -> Result<(), String> {
    let mut versions = vec![(session.compose_dir.clone(), session.git_commit.clone(), session.title.as_str())];
    versions.extend(session.previous_run.clone().map(|(dir, commit)| (dir, commit, session.previous_title())));
    for (compose_dir, commit, title) in versions {
        if compose_dir.is_empty() {
            continue;
        }
        if ls_containers::project_running(title, &commit).unwrap_or(false) {
            ls_containers::stop_project(Path::new(&compose_dir), title, &commit)
                .await
                .map_err(|e| format!("couldn't stop its containers: {e:#}"))?;
        }
        let key = ls_containers::compose_project_name(title, &commit);
        state.sessions.lock().map_err(|e| e.to_string())?.remove(&key);
    }
    Ok(())
}

/// Takes down the containers of the version this session ran before its
/// latest update (if any), then forgets it - unless that update is still
/// waiting to be applied, which needs it (Apply, Roll back).
async fn stop_previous_run(state: &AppState, session_id: &str) -> Result<(), String> {
    let session = get_session(state, session_id)?;
    let Some((compose_dir, commit)) = session.previous_run.clone() else { return Ok(()) };
    if Path::new(&compose_dir).exists() {
        ls_containers::stop_project(Path::new(&compose_dir), session.previous_title(), &commit)
            .await
            .map_err(|e| format!("stopping the previous version: {e:#}"))?;
    }
    let key = ls_containers::compose_project_name(session.previous_title(), &commit);
    state.sessions.lock().map_err(|e| e.to_string())?.remove(&key);
    if session.pending_update.is_none() {
        update_session(state, session_id, |s| s.previous_run = None)?;
    }
    Ok(())
}

/// "Save this session" - receiver side.
#[tauri::command]
pub fn save_received_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let session = get_session(&state, &session_id)?;
    crate::session_history::save_received(&session)
}

/// Closes a session in memory. Stops its containers first if they're
/// running (`stop_session_containers`) - if that fails the session is kept,
/// so the containers are never left running with no session to stop them
/// from. Does **not** delete an existing saved copy - the caller decides that
/// separately, same contract as `session_commands::discard_project_session`.
#[tauri::command]
pub async fn discard_received_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let session = state.received_sessions.lock().map_err(|e| e.to_string())?.get(&session_id).cloned();
    if let Some(session) = session {
        stop_session_containers(&state, &session).await?;
    }
    state.received_sessions.lock().map_err(|e| e.to_string())?.remove(&session_id);
    Ok(())
}

/// Deletes a saved received session's stored copy.
#[tauri::command]
pub fn delete_saved_received_session(session_id: String) -> Result<(), String> {
    crate::session_history::remove(&session_id)
}

/// Re-opens a saved received session: loads it and files it back into
/// memory. Does **not** run anything - Run is still a separate, explicit
/// step (`run_received_session`). An update that arrived before a restart
/// can't be applied any more (its snapshot was only ever held in memory), so
/// such a session goes back to the version that ran - still on disk - and the
/// sender's next push brings the update again.
///
/// `async` in the attribute: it asks Podman whether the session is running (a
/// blocking `podman ps`), so it runs off the main thread rather than freezing
/// the window, while staying a plain `fn` for callers.
#[tauri::command(async)]
pub fn open_saved_received_session(state: State<'_, AppState>, session_id: String) -> Result<ReceivedSessionView, String> {
    let open = state.received_sessions.lock().map_err(|e| e.to_string())?.get(&session_id).cloned();
    if let Some(open) = open {
        return build_view(&state, &open);
    }
    let mut session = crate::session_history::find_received(&session_id)?
        .ok_or_else(|| "that session isn't saved any more".to_string())?;
    let held = state.verified.lock().map_err(|e| e.to_string())?.contains_key(&session.snapshot_id);
    if session.pending_update.is_some() && !held {
        session.roll_back();
        persist(&session);
    }
    let view = build_view(&state, &session)?;
    state.received_sessions.lock().map_err(|e| e.to_string())?.insert(session.id.clone(), session);
    Ok(view)
}
