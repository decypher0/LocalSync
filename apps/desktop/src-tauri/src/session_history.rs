//! Saved sessions, persisted across restarts via the same "OS data dir + JSON
//! file" convention this project already uses everywhere else
//! (`ls_security::peers`'s `known_peers.json`, `ls_clouddrop::store`'s
//! `google_tokens.json`). Originally round 29's automatic session history.
//!
//! Session-model refactor: nothing is written here automatically any more.
//! An entry exists only because the user explicitly chose "Save this
//! session" - and an entry that carries a `project` is a whole saved
//! [`ProjectSession`] (folders + database plan + per-device history) that can
//! be opened again later. This module is purely storage: load the whole
//! list, upsert one entry by id, or delete one. Upsert, not append-only:
//! saving the same session again updates its one entry in place.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::project_session::ProjectSession;
use crate::received_session::ReceivedSession;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionHistoryEntry {
    /// The session's own id - what makes upsert possible without a second
    /// identifier scheme.
    pub id: String,
    /// "send" | "receive" - kept as a plain string rather than a Rust enum
    /// since this crosses the IPC boundary as a DTO; a typo here just shows
    /// up as a slightly wrong label in the history list, not a correctness
    /// issue anything downstream depends on.
    pub kind: String,
    /// Human-readable summary - the project's name(s).
    pub title: String,
    /// RFC3339. Set once, when the session starts.
    pub started_at: String,
    /// RFC3339, `None` while the session is still active. Entries written
    /// before the session-model refactor use this; a saved project session
    /// leaves it `None` (a workspace has no end).
    pub ended_at: Option<String>,
    /// The saved session itself, for an entry the user explicitly saved.
    /// `#[serde(default)]` so every entry written before this existed (title
    /// and timestamps only) still loads, as `None`.
    #[serde(default)]
    pub project: Option<ProjectSession>,
    /// The saved received session itself, for an entry the user explicitly
    /// saved on the receiving end. Parallel to `project` above - an entry
    /// carries at most one of the two, distinguished by `kind`. `#[serde(
    /// default)]` for the same reason as `project`: every entry written
    /// before this existed still loads, as `None`.
    #[serde(default)]
    pub received: Option<ReceivedSession>,
}

const FILE_NAME: &str = "session-history.json";
/// Caps the file from growing forever on a long-lived install - keeps the
/// most recent entries, drops the oldest once this is exceeded.
const MAX_ENTRIES: usize = 200;

fn default_dir() -> Result<PathBuf, String> {
    let base = dirs::data_dir().ok_or("could not determine the OS data directory")?;
    Ok(base.join("localsync"))
}

/// Missing file is not an error - same "nothing recorded yet" convention
/// `ls_security::peers::KnownPeers`/`ls_clouddrop::store` already use for
/// their own first-run case.
pub fn load() -> Result<Vec<SessionHistoryEntry>, String> {
    load_in(&default_dir()?)
}

pub fn upsert(entry: SessionHistoryEntry) -> Result<(), String> {
    upsert_in(&default_dir()?, entry)
}

pub fn remove(id: &str) -> Result<(), String> {
    remove_in(&default_dir()?, id)
}

/// The saved session stored under `id`, if the user saved one.
pub fn find_project(id: &str) -> Result<Option<ProjectSession>, String> {
    find_project_in(&default_dir()?, id)
}

/// "Save this session": stores the whole session (folders, database plan,
/// per-device history) so it can be opened again later.
pub fn save_project(session: &ProjectSession) -> Result<(), String> {
    save_project_in(&default_dir()?, session)
}

/// The saved received session stored under `id`, if the user saved one.
pub fn find_received(id: &str) -> Result<Option<ReceivedSession>, String> {
    find_received_in(&default_dir()?, id)
}

/// "Save this session" - receiver side: stores the whole `ReceivedSession` so
/// it can be reopened (and, via `ls_containers::run_existing`, re-run) later.
pub fn save_received(session: &ReceivedSession) -> Result<(), String> {
    save_received_in(&default_dir()?, session)
}

fn load_in(dir: &Path) -> Result<Vec<SessionHistoryEntry>, String> {
    let path = dir.join(FILE_NAME);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("parsing {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
}

fn save_project_in(dir: &Path, session: &ProjectSession) -> Result<(), String> {
    upsert_in(
        dir,
        SessionHistoryEntry {
            id: session.id.clone(),
            kind: "send".to_string(),
            title: session.title.clone(),
            started_at: session.created_at.clone(),
            ended_at: None,
            project: Some(session.clone()),
            received: None,
        },
    )
}

fn find_project_in(dir: &Path, id: &str) -> Result<Option<ProjectSession>, String> {
    Ok(load_in(dir)?.into_iter().find(|e| e.id == id).and_then(|e| e.project))
}

fn save_received_in(dir: &Path, session: &ReceivedSession) -> Result<(), String> {
    upsert_in(
        dir,
        SessionHistoryEntry {
            id: session.id.clone(),
            kind: "receive".to_string(),
            title: session.title.clone(),
            started_at: session.created_at.clone(),
            ended_at: None,
            project: None,
            received: Some(session.clone()),
        },
    )
}

fn find_received_in(dir: &Path, id: &str) -> Result<Option<ReceivedSession>, String> {
    Ok(load_in(dir)?.into_iter().find(|e| e.id == id).and_then(|e| e.received))
}

fn remove_in(dir: &Path, id: &str) -> Result<(), String> {
    let mut entries = load_in(dir)?;
    let before = entries.len();
    entries.retain(|e| e.id != id);
    if entries.len() == before {
        return Ok(());
    }
    let path = dir.join(FILE_NAME);
    let bytes = serde_json::to_vec_pretty(&entries).map_err(|e| e.to_string())?;
    std::fs::write(&path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))
}

fn upsert_in(dir: &Path, entry: SessionHistoryEntry) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let mut entries = load_in(dir)?;
    if let Some(existing) = entries.iter_mut().find(|e| e.id == entry.id) {
        *existing = entry;
    } else {
        entries.push(entry);
        if entries.len() > MAX_ENTRIES {
            let excess = entries.len() - MAX_ENTRIES;
            entries.drain(0..excess);
        }
    }
    let path = dir.join(FILE_NAME);
    let bytes = serde_json::to_vec_pretty(&entries).map_err(|e| e.to_string())?;
    std::fs::write(&path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::FolderPlanDto;
    use crate::project_session::{DeviceMarker, FolderCommit};

    fn entry(id: &str, ended: bool) -> SessionHistoryEntry {
        SessionHistoryEntry {
            id: id.to_string(),
            kind: "send".to_string(),
            title: "sample-project".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            ended_at: if ended { Some("2026-01-01T00:05:00Z".to_string()) } else { None },
            project: None,
            received: None,
        }
    }

    fn received_session(id: &str) -> ReceivedSession {
        let mut s = ReceivedSession::new(
            id.to_string(),
            "xusom-admin".to_string(),
            "abc123".to_string(),
            "xusom-admin@deadbeef".to_string(),
            "deadbeef".to_string(),
            "2026-01-01T00:00:00Z".to_string(),
        );
        s.record_run("/work".to_string(), "/work/xusom-admin-deadbeef".to_string());
        s
    }

    fn project_session(id: &str) -> ProjectSession {
        let mut s = ProjectSession::new(
            id.to_string(),
            "xusom-admin".to_string(),
            vec![FolderPlanDto { path: "/work/xusom-admin".to_string(), dump: None, compose: None }],
            "2026-01-01T00:00:00Z".to_string(),
        );
        s.record_send(
            "dev-a",
            "Alice",
            DeviceMarker {
                snapshot_id: "xusom-admin@abc".to_string(),
                commits: vec![FolderCommit { path: "/work/xusom-admin".to_string(), commit: "abc".to_string() }],
                sent_at: "2026-01-01T00:01:00Z".to_string(),
            },
        );
        s
    }

    #[test]
    fn missing_file_loads_as_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_in(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn upsert_adds_a_new_entry() {
        let dir = tempfile::tempdir().unwrap();
        upsert_in(dir.path(), entry("room-1", false)).unwrap();
        let loaded = load_in(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "room-1");
        assert_eq!(loaded[0].ended_at, None);
    }

    #[test]
    fn upsert_with_the_same_id_updates_in_place_not_a_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        upsert_in(dir.path(), entry("room-1", false)).unwrap();
        upsert_in(dir.path(), entry("room-1", true)).unwrap();
        let loaded = load_in(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1, "the same session id must update, not accumulate a second row");
        assert!(loaded[0].ended_at.is_some(), "the update should have carried through");
    }

    #[test]
    fn different_ids_both_persist() {
        let dir = tempfile::tempdir().unwrap();
        upsert_in(dir.path(), entry("room-1", true)).unwrap();
        upsert_in(dir.path(), entry("room-2", false)).unwrap();
        let loaded = load_in(dir.path()).unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn caps_at_max_entries_dropping_the_oldest() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(MAX_ENTRIES + 10) {
            upsert_in(dir.path(), entry(&format!("room-{i}"), true)).unwrap();
        }
        let loaded = load_in(dir.path()).unwrap();
        assert_eq!(loaded.len(), MAX_ENTRIES);
        // The oldest ones (room-0..room-9) should have been dropped, the
        // newest (room-{MAX_ENTRIES+9}) should have survived.
        assert!(!loaded.iter().any(|e| e.id == "room-0"));
        assert!(loaded.iter().any(|e| e.id == format!("room-{}", MAX_ENTRIES + 9)));
    }

    #[test]
    fn nothing_is_stored_unless_the_user_saves() {
        // "Discard" is simply never calling save: the file must not exist and
        // nothing may be recoverable afterwards.
        let dir = tempfile::tempdir().unwrap();
        let _session = project_session("s1"); // created, used, then discarded
        assert!(load_in(dir.path()).unwrap().is_empty());
        assert!(!dir.path().join(FILE_NAME).exists());
        assert!(find_project_in(dir.path(), "s1").unwrap().is_none());
    }

    #[test]
    fn a_saved_session_comes_back_whole_with_its_device_history() {
        let dir = tempfile::tempdir().unwrap();
        let session = project_session("s1");
        save_project_in(dir.path(), &session).unwrap();
        // A fresh read (as after an app restart) returns the same session.
        assert_eq!(find_project_in(dir.path(), "s1").unwrap(), Some(session));
    }

    #[test]
    fn saving_again_replaces_the_saved_copy_rather_than_duplicating_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = project_session("s1");
        save_project_in(dir.path(), &session).unwrap();
        session.record_send(
            "dev-b",
            "Bob",
            DeviceMarker { snapshot_id: "x@def".into(), commits: vec![], sent_at: "2026-01-02T00:00:00Z".into() },
        );
        save_project_in(dir.path(), &session).unwrap();
        assert_eq!(load_in(dir.path()).unwrap().len(), 1);
        assert_eq!(find_project_in(dir.path(), "s1").unwrap().unwrap().devices.len(), 2);
    }

    #[test]
    fn deleting_a_saved_session_removes_it_and_leaves_others() {
        let dir = tempfile::tempdir().unwrap();
        save_project_in(dir.path(), &project_session("s1")).unwrap();
        save_project_in(dir.path(), &project_session("s2")).unwrap();
        remove_in(dir.path(), "s1").unwrap();
        assert!(find_project_in(dir.path(), "s1").unwrap().is_none());
        assert!(find_project_in(dir.path(), "s2").unwrap().is_some());
        remove_in(dir.path(), "does-not-exist").unwrap(); // not an error
    }

    #[test]
    fn a_history_entry_written_before_saved_sessions_existed_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(FILE_NAME),
            r#"[{"id":"old","kind":"send","title":"t","started_at":"2026-01-01T00:00:00Z","ended_at":null}]"#,
        )
        .unwrap();
        let loaded = load_in(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].project.is_none());
        assert!(loaded[0].received.is_none(), "an entry written before `received` existed still loads");
    }

    // ---------- received sessions (receiver-side saves) ----------

    #[test]
    fn missing_received_file_loads_as_none_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_received_in(dir.path(), "r1").unwrap().is_none());
    }

    #[test]
    fn a_saved_received_session_comes_back_whole() {
        let dir = tempfile::tempdir().unwrap();
        let session = received_session("r1");
        save_received_in(dir.path(), &session).unwrap();
        assert_eq!(find_received_in(dir.path(), "r1").unwrap(), Some(session));
    }

    #[test]
    fn saving_a_received_session_again_updates_in_place_not_a_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = received_session("r1");
        save_received_in(dir.path(), &session).unwrap();
        session.record_update("xusom-admin@cafef00d".to_string(), "cafef00d".to_string(), String::new(), "2026-01-02T00:00:00Z".to_string());
        save_received_in(dir.path(), &session).unwrap();
        assert_eq!(load_in(dir.path()).unwrap().len(), 1, "the same session id must update, not accumulate a second row");
        let reloaded = find_received_in(dir.path(), "r1").unwrap().unwrap();
        assert_eq!(reloaded.snapshot_id, "xusom-admin@cafef00d");
        assert_eq!(reloaded.git_commit, "cafef00d");
    }

    #[test]
    fn deleting_a_saved_received_session_removes_it_and_leaves_others() {
        let dir = tempfile::tempdir().unwrap();
        save_received_in(dir.path(), &received_session("r1")).unwrap();
        save_received_in(dir.path(), &received_session("r2")).unwrap();
        remove_in(dir.path(), "r1").unwrap();
        assert!(find_received_in(dir.path(), "r1").unwrap().is_none());
        assert!(find_received_in(dir.path(), "r2").unwrap().is_some());
    }

    #[test]
    fn received_sessions_share_the_same_max_entries_cap_as_sends() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(MAX_ENTRIES + 10) {
            save_received_in(dir.path(), &received_session(&format!("r-{i}"))).unwrap();
        }
        let loaded = load_in(dir.path()).unwrap();
        assert_eq!(loaded.len(), MAX_ENTRIES);
        assert!(!loaded.iter().any(|e| e.id == "r-0"));
        assert!(loaded.iter().any(|e| e.id == format!("r-{}", MAX_ENTRIES + 9)));
    }

    #[test]
    fn a_saved_received_session_has_kind_receive() {
        let dir = tempfile::tempdir().unwrap();
        save_received_in(dir.path(), &received_session("r1")).unwrap();
        let loaded = load_in(dir.path()).unwrap();
        assert_eq!(loaded[0].kind, "receive");
    }
}
