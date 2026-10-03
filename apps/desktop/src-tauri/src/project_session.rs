//! The one model of "a send" (rebuilt in the session-model refactor).
//!
//! A [`ProjectSession`] is a persistent, project-scoped *workspace*, not a
//! connection: "this project (folders + database plan), prepared and ready
//! to send", plus a history of every device it has been sent to and - per
//! device - what that specific device last received (its [`DeviceMarker`]).
//! A connection to a device is ephemeral and never lives in here: connect,
//! transfer, disconnect, every time (see `session_commands::
//! send_project_session`).
//!
//! Everything in this file is pure data + logic (no Tauri, no network), so
//! the model's core behaviors - per-device markers being independent,
//! deciding what a push needs to diff against, save-vs-discard - are
//! unit-testable here without a running app.

use serde::{Deserialize, Serialize};

use crate::commands::FolderPlanDto;

/// How many past sends are remembered per device. Bounded so a long-lived
/// saved session's file can't grow forever.
const MAX_SEND_HISTORY: usize = 50;

/// One folder's commit, keyed by the folder's path as the user chose it.
/// `commit` covers uncommitted edits too (`ls_snapshot::snapshot_commit`);
/// `dump` fingerprints the folder's database dump file, so a re-exported
/// dump is a change to push even when no code changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FolderCommit {
    pub path: String,
    pub commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dump: Option<String>,
    /// The folder's own tree id at `commit` (`ls_snapshot::folder_tree`):
    /// committing an edit that was already sent changes `commit` but not
    /// this, and is not a change to push. `None` on markers from before this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
}

impl FolderCommit {
    /// Same folder with the same content (same commit, or same tree) and dump.
    pub fn same_content(&self, other: &FolderCommit) -> bool {
        self.path == other.path
            && self.dump == other.dump
            && (self.commit == other.commit || (self.tree.is_some() && self.tree == other.tree))
    }
}

/// What one specific device last received from this session - the point a
/// later push to *that device* diffs against. Independent per device: two
/// devices that received different versions each keep their own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceMarker {
    pub snapshot_id: String,
    pub commits: Vec<FolderCommit>,
    /// RFC3339.
    pub sent_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendRecord {
    pub sent_at: String,
    pub snapshot_id: String,
}

/// A device this session has sent to. `key` is the stable identity the
/// session files it under: a discovered device's persistent id when it has
/// one, otherwise a per-send key for a device reached by pasted code (a
/// code-based receiver has no identity of its own to recognize later).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub key: String,
    pub name: String,
    pub marker: Option<DeviceMarker>,
    pub history: Vec<SendRecord>,
    /// Size and duration of the last completed transfer to this device, for
    /// its transfer speed. `None` for records saved before this existed.
    #[serde(default)]
    pub last_transfer: Option<TransferStats>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TransferStats {
    pub bytes: u64,
    pub duration_ms: u64,
}

impl TransferStats {
    /// Bytes per second; `None` for a transfer too quick to time.
    pub fn bytes_per_sec(&self) -> Option<u64> {
        (self.duration_ms > 0).then(|| self.bytes * 1000 / self.duration_ms)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectSession {
    pub id: String,
    pub title: String,
    pub folders: Vec<FolderPlanDto>,
    /// RFC3339.
    pub created_at: String,
    pub devices: Vec<DeviceRecord>,
}

impl ProjectSession {
    pub fn new(id: String, title: String, folders: Vec<FolderPlanDto>, created_at: String) -> Self {
        Self { id, title, folders, created_at, devices: Vec::new() }
    }

    pub fn device(&self, key: &str) -> Option<&DeviceRecord> {
        self.devices.iter().find(|d| d.key == key)
    }

    /// Records a completed send to `key`: files the device under this
    /// session (creating its record the first time), moves *its own* marker
    /// to what it just received, and appends to *its own* history. No other
    /// device's marker is touched.
    pub fn record_send(&mut self, key: &str, name: &str, marker: DeviceMarker) {
        let record = SendRecord { sent_at: marker.sent_at.clone(), snapshot_id: marker.snapshot_id.clone() };
        match self.devices.iter_mut().find(|d| d.key == key) {
            Some(device) => {
                device.name = name.to_string();
                device.marker = Some(marker);
                device.history.push(record);
                if device.history.len() > MAX_SEND_HISTORY {
                    let excess = device.history.len() - MAX_SEND_HISTORY;
                    device.history.drain(0..excess);
                }
            }
            None => self.devices.push(DeviceRecord {
                key: key.to_string(),
                name: name.to_string(),
                marker: Some(marker),
                history: vec![record],
                last_transfer: None,
            }),
        }
    }

    /// Records how big and how long the last transfer to `key` was.
    pub fn set_last_transfer(&mut self, key: &str, stats: TransferStats) {
        if let Some(d) = self.devices.iter_mut().find(|d| d.key == key) {
            d.last_transfer = Some(stats);
        }
    }

    /// The device this session most recently sent to, if any.
    pub fn last_device(&self) -> Option<&DeviceRecord> {
        self.devices
            .iter()
            .filter(|d| d.marker.is_some())
            .max_by(|a, b| a.marker.as_ref().unwrap().sent_at.cmp(&b.marker.as_ref().unwrap().sent_at))
    }

    /// For each of this session's folders, in order, the commit `key` last
    /// received for that same folder - what a push to that device should
    /// diff against. `None` for a device with no marker, or for a folder
    /// that device never received (e.g. added to the session since).
    pub fn parent_commits_for(&self, key: &str) -> Vec<Option<String>> {
        let marker = self.device(key).and_then(|d| d.marker.as_ref());
        self.folders
            .iter()
            .map(|f| marker.and_then(|m| m.commits.iter().find(|c| c.path == f.path)).map(|c| c.commit.clone()))
            .collect()
    }

    /// True if `key` already has exactly what `current` is - nothing to push.
    /// A device with no marker is never up to date.
    pub fn is_up_to_date(&self, key: &str, current: &[FolderCommit]) -> bool {
        match self.device(key).and_then(|d| d.marker.as_ref()) {
            Some(marker) => {
                current.len() == marker.commits.len()
                    && current.iter().all(|c| marker.commits.iter().any(|m| m.same_content(c)))
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(path: &str) -> FolderPlanDto {
        FolderPlanDto { path: path.to_string(), dump: None, compose: None }
    }

    fn commits(pairs: &[(&str, &str)]) -> Vec<FolderCommit> {
        pairs.iter().map(|(p, c)| FolderCommit { path: p.to_string(), commit: c.to_string(), dump: None, tree: None }).collect()
    }

    fn marker(id: &str, pairs: &[(&str, &str)]) -> DeviceMarker {
        DeviceMarker { snapshot_id: id.to_string(), commits: commits(pairs), sent_at: "2026-01-01T00:00:00Z".to_string() }
    }

    fn session() -> ProjectSession {
        ProjectSession::new("s1".into(), "proj".into(), vec![folder("/a"), folder("/b")], "2026-01-01T00:00:00Z".into())
    }

    #[test]
    fn a_new_session_has_no_devices_and_no_markers() {
        let s = session();
        assert!(s.devices.is_empty());
        assert_eq!(s.parent_commits_for("anyone"), vec![None, None]);
        assert!(!s.is_up_to_date("anyone", &commits(&[("/a", "1"), ("/b", "1")])));
    }

    #[test]
    fn each_device_keeps_its_own_independent_marker() {
        let mut s = session();
        // Device A received v1, then later v2; device B only ever got v1.
        s.record_send("dev-a", "Alice", marker("p@v1", &[("/a", "a1"), ("/b", "b1")]));
        s.record_send("dev-b", "Bob", marker("p@v1", &[("/a", "a1"), ("/b", "b1")]));
        s.record_send("dev-a", "Alice", marker("p@v2", &[("/a", "a2"), ("/b", "b1")]));

        assert_eq!(s.parent_commits_for("dev-a"), vec![Some("a2".into()), Some("b1".into())]);
        assert_eq!(s.parent_commits_for("dev-b"), vec![Some("a1".into()), Some("b1".into())]);
        assert_eq!(s.devices.len(), 2, "same device must be one record, not one per send");
        assert_eq!(s.device("dev-a").unwrap().history.len(), 2);
        assert_eq!(s.device("dev-b").unwrap().history.len(), 1);
    }

    #[test]
    fn up_to_date_is_judged_per_device_against_the_current_commits() {
        let mut s = session();
        s.record_send("dev-a", "Alice", marker("p@v2", &[("/a", "a2"), ("/b", "b1")]));
        s.record_send("dev-b", "Bob", marker("p@v1", &[("/a", "a1"), ("/b", "b1")]));
        let now = commits(&[("/a", "a2"), ("/b", "b1")]);
        assert!(s.is_up_to_date("dev-a", &now), "A already has the current version");
        assert!(!s.is_up_to_date("dev-b", &now), "B is behind and still needs a push");
    }

    #[test]
    fn same_content_under_a_new_commit_is_up_to_date_but_a_new_dump_is_not() {
        let mut s = session();
        let fc = |commit: &str, tree: Option<&str>, dump: Option<&str>| FolderCommit {
            path: "/a".into(),
            commit: commit.into(),
            dump: dump.map(Into::into),
            tree: tree.map(Into::into),
        };
        let sent = DeviceMarker { snapshot_id: "p@wt".into(), commits: vec![fc("wt1", Some("t1"), Some("10:1"))], sent_at: "2026-01-01T00:00:00Z".into() };
        s.record_send("dev-a", "Alice", sent);
        assert!(s.is_up_to_date("dev-a", &[fc("c2", Some("t1"), Some("10:1"))]), "the sent edit, committed since");
        assert!(!s.is_up_to_date("dev-a", &[fc("c3", Some("t2"), Some("10:1"))]), "new content");
        assert!(!s.is_up_to_date("dev-a", &[fc("wt1", Some("t1"), Some("11:2"))]), "a re-exported dump");
        assert!(!s.is_up_to_date("dev-a", &[fc("c2", None, Some("10:1"))]), "unknown tree: only the commit can say");
    }

    #[test]
    fn a_folder_the_device_never_received_has_no_parent() {
        let mut s = session();
        s.record_send("dev-a", "Alice", marker("p@v1", &[("/a", "a1")])); // never got /b
        assert_eq!(s.parent_commits_for("dev-a"), vec![Some("a1".into()), None]);
    }

    #[test]
    fn re_sending_updates_the_device_name_and_bounds_its_history() {
        let mut s = session();
        for i in 0..(MAX_SEND_HISTORY + 5) {
            s.record_send("dev-a", &format!("Name {i}"), marker(&format!("p@{i}"), &[("/a", "x")]));
        }
        let d = s.device("dev-a").unwrap();
        assert_eq!(d.history.len(), MAX_SEND_HISTORY);
        assert_eq!(d.name, format!("Name {}", MAX_SEND_HISTORY + 4));
        assert_eq!(d.history.last().unwrap().snapshot_id, format!("p@{}", MAX_SEND_HISTORY + 4));
    }

    #[test]
    fn the_last_device_is_the_newest_send_and_keeps_its_transfer_speed() {
        let mut s = session();
        assert!(s.last_device().is_none(), "nothing sent yet: no previous recipient");
        let mut older = marker("p@v1", &[("/a", "a1")]);
        older.sent_at = "2026-01-01T00:00:00Z".into();
        let mut newer = marker("p@v1", &[("/a", "a1")]);
        newer.sent_at = "2026-02-01T00:00:00Z".into();
        s.record_send("dev-b", "Bob", newer);
        s.record_send("dev-a", "Alice", older);
        assert_eq!(s.last_device().unwrap().key, "dev-b");

        s.set_last_transfer("dev-b", TransferStats { bytes: 10_000_000, duration_ms: 4_000 });
        s.set_last_transfer("nobody", TransferStats { bytes: 1, duration_ms: 1 });
        assert_eq!(s.device("dev-b").unwrap().last_transfer.unwrap().bytes_per_sec(), Some(2_500_000));
        assert_eq!(s.device("dev-a").unwrap().last_transfer, None);
        assert_eq!(TransferStats { bytes: 5, duration_ms: 0 }.bytes_per_sec(), None);
    }

    #[test]
    fn a_record_saved_before_transfer_stats_still_loads() {
        let json = r#"{"key":"k","name":"n","marker":null,"history":[]}"#;
        assert_eq!(serde_json::from_str::<DeviceRecord>(json).unwrap().last_transfer, None);
    }

    #[test]
    fn a_session_round_trips_through_json_unchanged() {
        let mut s = session();
        s.record_send("dev-a", "Alice", marker("p@v1", &[("/a", "a1"), ("/b", "b1")]));
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<ProjectSession>(&json).unwrap(), s);
    }
}
