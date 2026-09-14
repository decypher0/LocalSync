//! Round 29 goal B2: session history, persisted across restarts via the
//! same "OS data dir + JSON file" convention this project already uses
//! everywhere else (`ls_security::peers`'s `known_peers.json`,
//! `ls_clouddrop::store`'s `google_tokens.json`) - not an in-memory list
//! that resets on restart, which is exactly what the round's own goal calls
//! out as insufficient.
//!
//! The frontend owns *when* an entry is written (it already tracks every
//! session's kind/title/timestamps for the tab UI - see app.js's `sessions`
//! map) - this module is purely storage: load the whole list, or upsert one
//! entry by id. Upsert, not append-only: a session's `ended_at` is `None`
//! while active and gets filled in once it finishes, and re-recording the
//! same session id should update that one entry in place, not accumulate a
//! second row for the same session.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionHistoryEntry {
    /// The same id the frontend's `sessions` map already uses for this
    /// session (a room code for send/receive sessions) - what makes upsert
    /// possible without a second identifier scheme.
    pub id: String,
    /// "send" | "receive" - kept as a plain string rather than a Rust enum
    /// since this crosses the IPC boundary as a DTO and the frontend is the
    /// only thing that ever constructs one; a typo here just shows up as a
    /// slightly wrong label in the history list, not a security or
    /// correctness issue anything downstream depends on.
    pub kind: String,
    /// Human-readable summary - project name(s) for a send, or the sending
    /// peer's project name once known for a receive.
    pub title: String,
    /// RFC3339. Set once, when the session starts.
    pub started_at: String,
    /// RFC3339, `None` while the session is still active. Set once the
    /// session ends (stopped, disconnected, or completed).
    pub ended_at: Option<String>,
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

fn load_in(dir: &Path) -> Result<Vec<SessionHistoryEntry>, String> {
    let path = dir.join(FILE_NAME);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("parsing {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
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

    fn entry(id: &str, ended: bool) -> SessionHistoryEntry {
        SessionHistoryEntry {
            id: id.to_string(),
            kind: "send".to_string(),
            title: "sample-project".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            ended_at: if ended { Some("2026-01-01T00:05:00Z".to_string()) } else { None },
        }
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
}
