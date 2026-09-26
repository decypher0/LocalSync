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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FolderCommit {
    pub path: String,
    pub commit: String,
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
            }),
        }
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
                    && current.iter().all(|c| marker.commits.iter().any(|m| m == c))
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
        pairs.iter().map(|(p, c)| FolderCommit { path: p.to_string(), commit: c.to_string() }).collect()
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
    fn a_session_round_trips_through_json_unchanged() {
        let mut s = session();
        s.record_send("dev-a", "Alice", marker("p@v1", &[("/a", "a1"), ("/b", "b1")]));
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<ProjectSession>(&json).unwrap(), s);
    }
}
