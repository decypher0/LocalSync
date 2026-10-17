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
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::commands::{await_recipient_consent, generated_files_for, plan_to_snapshot_inputs, FolderPlanDto, Progress, ReceiverJoined};
use crate::project_session::{push_base, DeviceMarker, FolderCommit, ProjectSession, PushBase, FULL_BASE_GONE, FULL_NO_ANSWER};
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

/// How long a push waits for the receiver to say which version it holds
/// (`ControlMessage::BaseCheck`) before sending the full snapshot - an older
/// receiver never answers. Answering is an in-memory lookup, so this is
/// only ever waited out in full by such a receiver.
const BASE_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// What each folder is at right now: its snapshot commit (HEAD, or HEAD plus
/// uncommitted edits to tracked files and the new files the session sends -
/// see `ls_snapshot::snapshot_commit_with`) and its dump's fingerprint. Cheap
/// (git status and a list of new files, plus staging only when something
/// changed), and what decides whether a cached artifact is still current and
/// whether a device is behind.
async fn current_commits(session: &ProjectSession) -> Result<Vec<FolderCommit>, String> {
    Ok(current_state(session).await?.into_iter().map(|(commit, _)| commit).collect())
}

/// [`current_commits`], with each folder's new files alongside
/// (`ls_snapshot::new_files`, the included ones and the rest).
async fn current_state(session: &ProjectSession) -> Result<Vec<(FolderCommit, Vec<ls_snapshot::NewFile>)>, String> {
    let mut out = Vec::with_capacity(session.folders.len());
    for f in &session.folders {
        let path = f.path.clone();
        let selection = session.new_file_selection(&f.path);
        let refs = f.externals.clone();
        let (commit, tree, new_files, externals) = tauri::async_runtime::spawn_blocking(move || {
            let root = Path::new(&path);
            let new_files = ls_snapshot::new_files(root, &selection)?;
            let commit = ls_snapshot::snapshot_commit_of(root, &new_files)?;
            let tree = ls_snapshot::folder_tree(root, &commit).ok();
            anyhow::Ok((commit, tree, new_files, crate::externals::externals_fingerprints(&refs)))
        })
        .await
        .map_err(|e| format!("git task panicked: {e}"))?
        .map_err(|e| format!("reading the current commit of {}: {e:#}", f.path))?;
        out.push((FolderCommit { path: f.path.clone(), commit, dump: dump_fingerprint(f), tree, externals }, new_files));
    }
    Ok(out)
}

/// Size and modified time of the folder's dump file - changes whenever the
/// dump is exported again. ponytail: not a content hash (re-hashing a
/// multi-GB dump on every check is too slow); an identical re-export still
/// counts as a change, which only costs one redundant push.
fn dump_fingerprint(f: &FolderPlanDto) -> Option<String> {
    let dump = f.dump.as_ref()?;
    let meta = std::fs::metadata(&dump.file_path).ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0);
    Some(format!("{}:{modified}", meta.len()))
}

fn artifact_key(session_id: &str, parents: &[Option<String>]) -> String {
    let parts: Vec<&str> = parents.iter().map(|p| p.as_deref().unwrap_or("-")).collect();
    format!("{session_id}|{}", parts.join(","))
}

async fn build_artifact(
    state: &AppState,
    session: &ProjectSession,
    parents: &[Option<String>],
) -> Result<CachedArtifact, String> {
    let folders = &session.folders;
    let (specs, dumps) = plan_to_snapshot_inputs(folders, parents)?;
    let generated = generated_files_for(folders, &specs, None)?;
    // The session id travels as the manifest's project id: what a receiver
    // files the project under, and what a later push's BaseCheck names.
    let options = ls_snapshot::SnapshotOptions {
        project_id: Some(session.id.clone()),
        new_files: folders.iter().map(|f| session.new_file_selection(&f.path)).collect(),
    };
    state.artifact_builds.fetch_add(1, Ordering::SeqCst);
    let snapshot =
        tauri::async_runtime::spawn_blocking(move || ls_snapshot::create_snapshot_multi_opts(&specs, &dumps, &generated, &options))
        .await
        .map_err(|e| format!("snapshot task panicked: {e}"))?
        .map_err(|e| format!("{e:#}"))?;

    let snapshot_id = format!("{}@{}", snapshot.manifest.project_name, snapshot.manifest.git_commit);
    let commits = folders
        .iter()
        .zip(snapshot.manifest.folders.iter())
        .map(|(f, info)| FolderCommit {
            path: f.path.clone(),
            commit: info.git_commit.clone(),
            dump: dump_fingerprint(f),
            tree: ls_snapshot::folder_tree(Path::new(&f.path), &info.git_commit).ok(),
            externals: Default::default(),
        })
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
    let parents = match since {
        Some(device) => session.parent_commits_for(device),
        None => vec![None; session.folders.len()],
    };
    Ok(ensure_artifact_against(state, session, parents).await?.0)
}

/// [`ensure_artifact`] diffing each folder against `parents`, and the
/// parents it actually diffs against: a marker commit that is no longer in
/// the folder's repository (an unsent-edit commit `git gc` pruned, or a
/// rewritten history) can't be diffed against, so that folder goes in full;
/// if building the diff fails anyway, the whole artifact goes in full rather
/// than failing the push.
async fn ensure_artifact_against(
    state: &AppState,
    session: &ProjectSession,
    parents: Vec<Option<String>>,
) -> Result<(Arc<CachedArtifact>, Vec<Option<String>>), String> {
    let current = current_commits(session).await?;
    let parents = existing_parents(session, parents).await;
    let key = artifact_key(&session.id, &parents);
    if let Some(cached) = state.artifacts.lock().map_err(|e| e.to_string())?.get(&key) {
        if cached.commits == current {
            return Ok((cached.clone(), parents));
        }
    }
    let (built, parents) = match build_artifact(state, session, &parents).await {
        Ok(a) => (a, parents),
        Err(e) if parents.iter().any(Option::is_some) => {
            log::warn!("ensure_artifact: diff against the device's marker failed ({e}); using a full diff instead");
            let full = vec![None; session.folders.len()];
            (build_artifact(state, session, &full).await?, full)
        }
        Err(e) => return Err(e),
    };
    let built = Arc::new(built);
    state.artifacts.lock().map_err(|e| e.to_string())?.insert(artifact_key(&session.id, &parents), built.clone());
    Ok((built, parents))
}

/// `parents`, with each commit that no longer exists in its folder's
/// repository replaced by `None` (that folder is sent in full).
async fn existing_parents(session: &ProjectSession, parents: Vec<Option<String>>) -> Vec<Option<String>> {
    let mut out = Vec::with_capacity(parents.len());
    for (f, parent) in session.folders.iter().zip(parents) {
        let Some(commit) = parent else {
            out.push(None);
            continue;
        };
        let (root, check) = (f.path.clone(), commit.clone());
        let exists = tauri::async_runtime::spawn_blocking(move || ls_snapshot::commit_exists(Path::new(&root), &check))
            .await
            .unwrap_or(false);
        if !exists {
            log::info!("ensure_artifact: {commit} is no longer in {}; sending that folder in full", f.path);
        }
        out.push(exists.then_some(commit));
    }
    out
}

/// Asks the receiver which version of this project it holds
/// (`ControlMessage::BaseCheck`) and decides whether a diff against
/// `parents` is any use to it. No answer in [`BASE_CHECK_TIMEOUT`] (an older
/// receiver) counts as "unknown": the full snapshot goes.
async fn check_receiver_base(conn: &ls_net::DataChannelConn, project_id: &str, parents: &[Option<String>]) -> PushBase {
    let sender_pubkey_hex = match ls_snapshot::sender_pubkey_hex() {
        Ok(key) => key,
        Err(e) => {
            log::warn!("send_project_session: couldn't read this device's sender key ({e:#}); sending in full");
            return PushBase::Full(FULL_NO_ANSWER);
        }
    };
    let ask = ls_net::ControlMessage::BaseCheck { project_id: project_id.to_string(), sender_pubkey_hex };
    let answer = match ls_net::request_control(conn, &ask, BASE_CHECK_TIMEOUT).await {
        Ok(Some(ls_net::ControlMessage::BaseCheckResponse { commits })) => Some(commits),
        Ok(Some(other)) => {
            log::warn!("send_project_session: unexpected reply to BaseCheck: {other:?}");
            None
        }
        Ok(None) => {
            log::info!("send_project_session: no BaseCheck reply within {BASE_CHECK_TIMEOUT:?} (an older receiver?)");
            None
        }
        Err(e) => {
            log::warn!("send_project_session: BaseCheck failed: {e:#}");
            None
        }
    };
    push_base(parents, answer.as_ref().map(|commits| commits.as_deref()))
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
    /// The last transfer to this device: its size and speed (None if unknown).
    pub last_bytes: Option<u64>,
    pub bytes_per_sec: Option<u64>,
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
                last_bytes: d.last_transfer.map(|t| t.bytes),
                bytes_per_sec: d.last_transfer.and_then(|t| t.bytes_per_sec()),
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
    // Every session is kept on disk from the moment it exists, so it
    // survives a restart and shows on Home.
    if let Err(e) = crate::session_history::save_project(&session) {
        log::warn!("create_project_session: couldn't persist {}: {e}", session.id);
    }
    let view = build_view(&state, &session, None);
    state.project_sessions.lock().map_err(|e| e.to_string())?.insert(session.id.clone(), session);
    Ok(view)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PushStatus {
    /// The device this session last sent to (Push update's target).
    pub last_device_key: Option<String>,
    pub last_device_name: Option<String>,
    /// The project differs from what that device last received - a new
    /// commit, an uncommitted edit to a tracked file, a new file it sends, or
    /// a re-exported database dump. There is something to push.
    pub changed: bool,
}

/// Push update's target (the device this session last sent to) and whether
/// the project, as `current`, differs from what it last received - the one
/// answer `project_push_status` and `project_changes` both give.
fn push_target(session: &ProjectSession, current: &[FolderCommit]) -> PushStatus {
    match session.last_device() {
        Some(last) => PushStatus {
            last_device_key: Some(last.key.clone()),
            last_device_name: Some(last.name.clone()),
            changed: !session.is_up_to_date(&last.key, current),
        },
        None => PushStatus { last_device_key: None, last_device_name: None, changed: false },
    }
}

/// Whether "Push update" has anything to do: compares each folder's current
/// state (`current_commits`) with what the last-known device received. Cheap
/// (no rebuild), so the session page can ask it regularly.
#[tauri::command]
pub async fn project_push_status(state: State<'_, AppState>, session_id: String) -> Result<PushStatus, String> {
    let session = get_session(&state, &session_id)?;
    if session.last_device().is_none() {
        return Ok(push_target(&session, &[]));
    }
    let current = current_commits(&session).await?;
    Ok(push_target(&session, &current))
}

/// One line of the "Changes since last send" panel.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FileChange {
    /// Relative to the folder, `/`-separated.
    pub path: String,
    /// "added", "modified" or "deleted".
    pub change_type: String,
    /// A new (untracked) file: the user ticks it in or out.
    pub new_file: bool,
    /// The next send carries it (always, for a file that isn't new).
    pub included: bool,
    /// Secret-looking (`ls_snapshot::looks_secret`): starts out unticked.
    pub secret: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderChanges {
    pub path: String,
    pub files: Vec<FileChange>,
    /// Its database dump was exported again since the last send.
    pub dump_changed: bool,
    /// The device never received this folder (or nothing was sent yet):
    /// `files` lists only its new files, to choose before sending.
    pub not_sent: bool,
    /// What the device last received is no longer in git, so the changes
    /// can't be listed; a push sends the whole project.
    pub base_gone: bool,
    /// Files from outside the project (Setup's "Files outside the project")
    /// added, edited or dropped since the last send.
    pub externals: Vec<ExternalChange>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExternalChange {
    /// The file or folder's name.
    pub name: String,
    /// "added", "modified" or "removed".
    pub change_type: String,
}

/// `sent` against `now` (external id -> fingerprint); names from the
/// folder's chosen external files.
fn external_changes(
    sent: &std::collections::BTreeMap<String, String>,
    now: &std::collections::BTreeMap<String, String>,
    refs: &[crate::externals::ExternalRef],
) -> Vec<ExternalChange> {
    let name = |id: &str| {
        refs.iter()
            .find(|r| r.id == id)
            .and_then(|r| r.path.rsplit(['/', '\\']).find(|s| !s.is_empty()).map(str::to_string))
            .unwrap_or_else(|| id.to_string())
    };
    let mut out: Vec<ExternalChange> = now
        .iter()
        .filter_map(|(id, fp)| match sent.get(id) {
            None => Some("added"),
            Some(old) if old != fp => Some("modified"),
            Some(_) => None,
        }.map(|t| ExternalChange { name: name(id), change_type: t.into() }))
        .chain(sent.keys().filter(|id| !now.contains_key(*id)).map(|id| ExternalChange { name: name(id), change_type: "removed".into() }))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// What `project_changes` reports: [`PushStatus`] (same fields, same
/// answer) plus the per-folder list.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectChanges {
    #[serde(flatten)]
    pub status: PushStatus,
    pub folders: Vec<FolderChanges>,
}

/// A folder's panel lines: `changed` (git's list from what was sent to the
/// current snapshot commit), with new files marked - and the new files the
/// user unticked, which the snapshot leaves out, listed unticked. A file sent
/// before as a new file and unticked since shows as deleted, unticked.
fn file_changes(changed: Vec<ls_snapshot::ChangedFile>, new_files: &[ls_snapshot::NewFile]) -> Vec<FileChange> {
    let mut files: Vec<FileChange> = changed
        .into_iter()
        .map(|c| {
            let new = new_files.iter().find(|n| n.path == c.path);
            FileChange {
                new_file: new.is_some(),
                included: new.map_or(true, |n| n.included),
                secret: new.is_some_and(|n| n.secret),
                path: c.path,
                change_type: c.change_type,
            }
        })
        .collect();
    for n in new_files.iter().filter(|n| !n.included) {
        if !files.iter().any(|f| f.path == n.path) {
            files.push(FileChange { path: n.path.clone(), change_type: "added".into(), new_file: true, included: false, secret: n.secret });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// A not-yet-sent folder's panel lines: just its new files, to choose.
fn new_file_lines(new_files: &[ls_snapshot::NewFile]) -> Vec<FileChange> {
    new_files
        .iter()
        .map(|n| FileChange { path: n.path.clone(), change_type: "added".into(), new_file: true, included: n.included, secret: n.secret })
        .collect()
}

/// "Changes since last send", for the session page: per folder, what was
/// added, modified or deleted since the last send to Push update's target
/// device (`project_push_status`'s device, and the same `changed`), with a
/// tick box for each new file. Cheap enough to poll: the current snapshot
/// commit is computed as `project_push_status` does, then one `git diff
/// --name-status` against the device's marker - nothing is rebuilt.
#[tauri::command]
pub async fn project_changes(state: State<'_, AppState>, session_id: String) -> Result<ProjectChanges, String> {
    let session = get_session(&state, &session_id)?;
    let current = current_state(&session).await?;
    let commits: Vec<FolderCommit> = current.iter().map(|(c, _)| c.clone()).collect();
    let status = push_target(&session, &commits);
    let marker = session.last_device().and_then(|d| d.marker.clone());
    let mut folders = Vec::with_capacity(current.len());
    for (now, new_files) in current {
        let sent = marker.as_ref().and_then(|m| m.commits.iter().find(|c| c.path == now.path)).cloned();
        let Some(sent) = sent else {
            folders.push(FolderChanges { files: new_file_lines(&new_files), dump_changed: false, not_sent: true, base_gone: false, externals: vec![], path: now.path });
            continue;
        };
        let (root, from, to) = (now.path.clone(), sent.commit.clone(), now.commit.clone());
        let listed = if from == to {
            Ok(Some(Vec::new()))
        } else {
            tauri::async_runtime::spawn_blocking(move || {
                let root = Path::new(&root);
                match ls_snapshot::changed_files(root, &from, &to) {
                    Ok(files) => Ok(Some(files)),
                    Err(_) if !ls_snapshot::commit_exists(root, &from) => Ok(None),
                    Err(e) => Err(e),
                }
            })
            .await
            .map_err(|e| format!("git task panicked: {e}"))?
        };
        let listed = listed.map_err(|e| format!("listing the changes in {}: {e:#}", now.path))?;
        let base_gone = listed.is_none();
        folders.push(FolderChanges {
            files: listed.map(|changed| file_changes(changed, &new_files)).unwrap_or_default(),
            dump_changed: sent.dump != now.dump,
            not_sent: false,
            base_gone,
            externals: external_changes(&sent.externals, &now.externals, session.folders.iter().find(|f| f.path == now.path).map_or(&[], |f| &f.externals)),
            path: now.path,
        });
    }
    Ok(ProjectChanges { status, folders })
}

/// Ticks a new file of one of the session's folders in or out of what is
/// sent from now on, and keeps that choice with the session.
#[tauri::command]
pub fn set_project_change_selection(
    state: State<'_, AppState>,
    session_id: String,
    folder: String,
    path: String,
    include: bool,
) -> Result<(), String> {
    let updated = {
        let mut sessions = state.project_sessions.lock().map_err(|e| e.to_string())?;
        let session = sessions.get_mut(&session_id).ok_or_else(|| format!("no open session with id {session_id}"))?;
        if !session.folders.iter().any(|f| f.path == folder) {
            return Err(format!("{folder} isn't one of this session's folders"));
        }
        session.set_new_file_included(&folder, &path, include);
        session.clone()
    };
    if let Err(e) = crate::session_history::save_project(&updated) {
        log::warn!("set_project_change_selection: couldn't update the saved copy of {}: {e}", updated.id);
    }
    Ok(())
}

/// Re-checks the session against the project as it is on disk right now
/// (rebuilding the artifact only if a commit moved) and reports, per device,
/// whether it is behind. This is what "Push update" starts from.
#[tauri::command]
pub async fn refresh_project_session(state: State<'_, AppState>, session_id: String) -> Result<ProjectSessionView, String> {
    let session = get_session(&state, &session_id)?;
    ensure_artifact(&state, &session, None).await?;
    let current = current_commits(&session).await?;
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
    /// A push that went as the full snapshot instead of a diff, and why
    /// (`project_session::FULL_*`, e.g. "the receiver had a different
    /// version") - for the sender to show. `None` otherwise.
    pub full_reason: Option<String>,
    pub view: ProjectSessionView,
}

/// One transfer: connect, (optionally) get the recipient's consent, send the
/// session's artifact, **disconnect**, then move that one device's marker.
///
/// A push (`since_last`) diffs against what the device last received - but
/// only once the receiver confirms it still holds exactly that (BaseCheck,
/// right before the payload); otherwise, or without an answer, the full
/// snapshot goes. The payload always carries the whole project; the diff
/// only shapes what the receiver reviews.
#[tauri::command]
pub async fn send_project_session<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    request: SendRequest,
) -> Result<SendResult, String> {
    let session = get_session(&state, &request.session_id)?;
    let known_key = request.device_key.as_deref().filter(|k| session.device(k).is_some());
    let since = if request.since_last { known_key } else { None };

    let wanted = match since {
        Some(device) => session.parent_commits_for(device),
        None => vec![None; session.folders.len()],
    };
    let wanted_diff = wanted.iter().any(Option::is_some);
    let (mut artifact, parents) = ensure_artifact_against(&state, &session, wanted).await?;
    let diffed = parents.iter().any(Option::is_some);
    let mut full_reason = (wanted_diff && !diffed).then(|| FULL_BASE_GONE.to_string());
    let device_key = request
        .device_key
        .clone()
        .unwrap_or_else(|| format!("dev-{}", request.room_code));

    if let Some(k) = since {
        if session.is_up_to_date(k, &artifact.commits) {
            let view = build_view(&state, &session, Some(&artifact.commits));
            return Ok(SendResult { snapshot_id: artifact.snapshot_id.clone(), device_key, up_to_date: true, full_reason: None, view });
        }
    }

    log::info!("send_project_session: connecting, session={} device={device_key}", session.id);
    let conn = ls_net::connect_as_sender(&request.signaling_url, &request.room_code)
        .await
        .map_err(|e| {
            log::warn!("send_project_session: connect_as_sender failed: {e:#}");
            crate::commands::connect_error(&e, &request.signaling_url)
        })?;
    if request.require_accept {
        await_recipient_consent(&conn, request.sender_name.clone()).await?;
    }
    let _ = app.emit("receiver-connecting", ReceiverJoined { session_id: request.room_code.clone() });
    if diffed {
        if let PushBase::Full(reason) = check_receiver_base(&conn, &session.id, &parents).await {
            log::info!("send_project_session: sending {} in full: {reason}", session.id);
            artifact = ensure_artifact(&state, &session, None).await?;
            full_reason = Some(reason.to_string());
        }
    }

    let progress_id = request.room_code.clone();
    let started = std::time::Instant::now();
    ls_net::send_payload(&conn, &artifact.bytes, |sent, total| {
        let _ = app.emit("share-progress", Progress { session_id: progress_id.clone(), bytes: sent, total });
    })
    .await
    .map_err(|e| e.to_string())?;
    // Ephemeral by design: the transfer is complete (send_payload waits for
    // the receiver's acknowledgement), so the connection ends here.
    drop(conn);
    let stats = crate::project_session::TransferStats {
        bytes: artifact.bytes.len() as u64,
        duration_ms: started.elapsed().as_millis() as u64,
    };

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
            s.set_last_transfer(&device_key, stats);
            s.clone()
        })
    };
    let Some(updated) = updated else {
        return Ok(SendResult {
            snapshot_id: artifact.snapshot_id.clone(),
            device_key,
            up_to_date: false,
            full_reason,
            view: build_view(&state, &session, Some(&artifact.commits)),
        });
    };
    // Every session stays current on disk.
    if let Err(e) = crate::session_history::save_project(&updated) {
        log::warn!("send_project_session: couldn't update the saved copy of {}: {e}", updated.id);
    }
    // A per-device diff artifact has done its job (used or not); only the
    // plain one stays cached.
    if diffed {
        state.artifacts.lock().map_err(|e| e.to_string())?.remove(&artifact_key(&session.id, &parents));
    }
    let view = build_view(&state, &updated, Some(&artifact.commits));
    Ok(SendResult { snapshot_id: artifact.snapshot_id.clone(), device_key, up_to_date: false, full_reason, view })
}

// ---------- Cloud drop ----------

/// What a Cloud drop uploads for a session: its prepared artifact - the same
/// bytes `send_project_session` sends, so the compose file and Dockerfile the
/// Setup wizard generates, and the database dumps, are in it. (Packaging the
/// folder on its own would leave all of that out.) Returns the Drive file
/// name with it.
pub async fn cloud_drop_payload(state: &AppState, session_id: &str) -> Result<(String, Arc<CachedArtifact>), String> {
    let session = get_session(state, session_id)?;
    let artifact = ensure_artifact(state, &session, None).await?;
    Ok((format!("{}.localsync-snapshot", artifact.snapshot_id), artifact))
}

/// Records a granted Cloud drop on its sender session, the way a P2P send is
/// recorded: the receiver's Google account becomes a device
/// (`cloud:<email>`, named by the email) with a marker for what it got, so the
/// session's Devices, "Last sent", "Last known device" and Push update all
/// see it. Returns the session's updated view; `None` if the session was
/// closed meanwhile.
///
/// Each call is one delivery (a history entry). The grant and the download
/// confirmation both lead here, through
/// `commands::record_cloud_drop_delivery`, which lets only the first of the
/// two through per upload.
///
/// Push update to a cloud device: there's no connection to push a diff over,
/// so it is a new Cloud drop of the session - `start_cloud_drop_session`
/// again, which uploads the current whole artifact (never a diff against
/// the marker, so the receiver needs nothing it had before) under a new
/// code; once the receiver is granted again this moves the same
/// `cloud:<email>` device's marker and adds to its history. Its marker holds
/// the plain artifact's commits, so `project_push_status` /
/// `is_up_to_date` compare a cloud device exactly like any other.
/// (`send_project_session` can't reach a `cloud:` device: it has no room.)
pub fn record_cloud_delivery(
    state: &AppState,
    delivery: &crate::state::CloudDelivery,
    google_email: &str,
) -> Result<Option<ProjectSessionView>, String> {
    let device_key = format!("cloud:{google_email}");
    let marker = DeviceMarker {
        snapshot_id: delivery.snapshot_id.clone(),
        commits: delivery.commits.clone(),
        sent_at: now_rfc3339(),
    };
    let updated = state.project_sessions.lock().map_err(|e| e.to_string())?.get_mut(&delivery.session_id).map(|s| {
        s.record_send(&device_key, google_email, marker);
        s.set_last_transfer(
            &device_key,
            crate::project_session::TransferStats { bytes: delivery.bytes, duration_ms: delivery.upload_ms },
        );
        s.clone()
    });
    let Some(updated) = updated else { return Ok(None) };
    if let Err(e) = crate::session_history::save_project(&updated) {
        log::warn!("record_cloud_delivery: couldn't update the saved copy of {}: {e}", updated.id);
    }
    Ok(Some(build_view(state, &updated, Some(&delivery.commits))))
}

/// "Save this session": the user's explicit opt-in to storing it on disk.
#[tauri::command]
pub fn save_project_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let session = get_session(&state, &session_id)?;
    crate::session_history::save_project(&session)
}

/// Closes a session in memory. Does **not** write anything, and does not
/// delete an earlier saved copy - the caller decides that separately. A
/// sending session runs no containers of its own (only a receiver does - see
/// `receiver_session_commands::discard_received_session`), so there is
/// nothing to stop here.
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
    #[test]
    fn external_changes_name_what_was_added_edited_or_dropped() {
        let map = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let refs: Vec<crate::externals::ExternalRef> = serde_json::from_str(
            r#"[{"id":"x1","key":"k","config_file":"a.properties","value":"v","path":"C:\\Users\\Some One\\configuration\\company_configuration.xml","kind":"file","status":"outside_exists"},
                {"id":"x2","key":"k2","config_file":"a.properties","value":"v","path":"/srv/logos/","kind":"dir","status":"outside_exists"}]"#,
        )
        .unwrap();
        let got = external_changes(&map(&[("x1", "a"), ("x3", "c")]), &map(&[("x1", "b"), ("x2", "n")]), &refs);
        let got: Vec<(&str, &str)> = got.iter().map(|c| (c.name.as_str(), c.change_type.as_str())).collect();
        assert_eq!(got, [("company_configuration.xml", "modified"), ("logos", "added"), ("x3", "removed")]);
        assert!(external_changes(&map(&[("x1", "a")]), &map(&[("x1", "a")]), &refs).is_empty());
    }

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
    fn the_changes_list_marks_new_files_and_keeps_unticked_ones() {
        let changed = |path: &str, change_type: &str| ls_snapshot::ChangedFile { path: path.into(), change_type: change_type.into() };
        let new = |path: &str, secret: bool, included: bool| ls_snapshot::NewFile { path: path.into(), secret, included };
        let lines = file_changes(
            vec![changed("src/app.rs", "modified"), changed("old.txt", "deleted"), changed("notes.md", "added"), changed("sent-once.txt", "deleted")],
            &[new("notes.md", false, true), new(".env", true, false), new("sent-once.txt", false, false)],
        );
        let summary: Vec<(&str, &str, bool, bool, bool)> =
            lines.iter().map(|l| (l.path.as_str(), l.change_type.as_str(), l.new_file, l.included, l.secret)).collect();
        assert_eq!(
            summary,
            vec![
                (".env", "added", true, false, true),
                ("notes.md", "added", true, true, false),
                ("old.txt", "deleted", false, true, false),
                ("sent-once.txt", "deleted", true, false, false),
                ("src/app.rs", "modified", false, true, false),
            ],
            "an unticked new file is listed once: as added if never sent, as deleted if the device has it"
        );
        assert_eq!(new_file_lines(&[new("a.txt", false, true)]).len(), 1);
    }

    #[test]
    fn a_default_title_names_the_folders() {
        let folders = vec![
            FolderPlanDto { path: "/w/xusom-admin".into(), dump: None, compose: None, externals: vec![] },
            FolderPlanDto { path: "/w/xusom-api".into(), dump: None, compose: None, externals: vec![] },
        ];
        assert_eq!(default_title(&folders), "xusom-admin, xusom-api");
    }
}
