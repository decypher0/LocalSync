//! The one model of "a receive" (receiver-side counterpart to
//! `project_session::ProjectSession`).
//!
//! A [`ReceivedSession`] is a persistent, project-scoped *workspace* on the
//! receiving end: "this project, as last pushed to me by this sender", plus
//! whatever this device has already unpacked and run of it. Unlike a
//! `ProjectSession` (which tracks many devices from one sender's point of
//! view), a `ReceivedSession` only ever has one counterpart: the sender it
//! came from. A connection is never stored here - see `receiver_session_
//! commands` for how a push lands on (or creates) one of these.
//!
//! Pure data + logic (no Tauri, no network), same as `project_session.rs`,
//! so its one real behavior - what a fresh push changes vs. leaves alone -
//! is unit-testable without a running app.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReceivedSession {
    /// This workspace's own id. Deliberately **not** `snapshot_id`: the same
    /// sender pushing the same unchanged project again produces the exact
    /// same `snapshot_id` (`"<project_name>@<git_commit>"`), which would
    /// collide if it were also used as the session's identity. Minted once,
    /// fresh, when the session is first created.
    pub id: String,
    /// `manifest.project_name`.
    pub title: String,
    /// Hex-encoded `manifest.sender_pubkey` - identity of who this came
    /// from, and half of the `armed_updates` matching key (see
    /// `state::AppState::armed_updates`).
    pub sender_pubkey_hex: String,
    /// `"<project_name>@<git_commit>"` of the last push that landed on this
    /// session.
    pub snapshot_id: String,
    pub git_commit: String,
    /// The directory the user chose to unpack into, last time this session
    /// was run. Empty until the first successful Run.
    pub work_dir: String,
    /// The exact directory `podman-compose` runs in for this session's
    /// current version - empty until the first successful Run for the
    /// current `snapshot_id`, and cleared again the moment a new push
    /// changes what `snapshot_id` this session is at (the old compose dir
    /// belongs to the *previous* commit, not this one).
    pub compose_dir: String,
    /// RFC3339. Set once, when this session is first created.
    pub created_at: String,
    /// RFC3339. Bumped on every push that lands on this session (including
    /// the first one, where it equals `created_at`).
    pub last_received_at: String,
}

impl ReceivedSession {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        title: String,
        sender_pubkey_hex: String,
        snapshot_id: String,
        git_commit: String,
        created_at: String,
    ) -> Self {
        Self {
            id,
            title,
            sender_pubkey_hex,
            snapshot_id,
            git_commit,
            work_dir: String::new(),
            compose_dir: String::new(),
            last_received_at: created_at.clone(),
            created_at,
        }
    }

    /// A fresh push landed on this already-existing session (an "armed"
    /// update, see `state::AppState::armed_updates`): move to the new
    /// version. `compose_dir` is the caller's call - in practice always `""`
    /// at the moment a push merely arrives (a directory built for the
    /// *previous* commit isn't valid for this one, and nothing has been
    /// unpacked/run for the new commit yet), but taking it as a parameter
    /// rather than hardcoding the clear keeps this method a plain "replace
    /// what a push changes" rather than baking in one caller's policy.
    /// `work_dir` is left alone: it's still exactly the right place to
    /// unpack the new version into.
    pub fn record_update(&mut self, snapshot_id: String, git_commit: String, compose_dir: String, received_at: String) {
        self.snapshot_id = snapshot_id;
        self.git_commit = git_commit;
        self.compose_dir = compose_dir;
        self.last_received_at = received_at;
    }

    /// Called once a Run (fresh or re-run) actually succeeds, so later opens
    /// of this session know where its containers were last brought up from.
    /// Not part of `record_update` - a Run is never itself "a push landing",
    /// and can happen well after the push it's running (e.g. after an app
    /// restart, via `ls_containers::run_existing`).
    pub fn record_run(&mut self, work_dir: String, compose_dir: String) {
        self.work_dir = work_dir;
        self.compose_dir = compose_dir;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> ReceivedSession {
        ReceivedSession::new(
            "r1".into(),
            "xusom-admin".into(),
            "abc123".into(),
            "xusom-admin@deadbeef".into(),
            "deadbeef".into(),
            "2026-01-01T00:00:00Z".into(),
        )
    }

    #[test]
    fn a_new_session_starts_with_no_work_dir_or_compose_dir_and_matching_timestamps() {
        let s = session();
        assert_eq!(s.work_dir, "");
        assert_eq!(s.compose_dir, "");
        assert_eq!(s.created_at, s.last_received_at);
    }

    #[test]
    fn record_run_sets_the_directories_a_successful_run_produced() {
        let mut s = session();
        s.record_run("/home/me/work".into(), "/home/me/work/xusom-admin-deadbeef".into());
        assert_eq!(s.work_dir, "/home/me/work");
        assert_eq!(s.compose_dir, "/home/me/work/xusom-admin-deadbeef");
    }

    #[test]
    fn record_update_moves_the_version_and_clears_the_stale_compose_dir_but_keeps_work_dir() {
        let mut s = session();
        s.record_run("/home/me/work".into(), "/home/me/work/xusom-admin-deadbeef".into());
        s.record_update("xusom-admin@cafef00d".into(), "cafef00d".into(), String::new(), "2026-01-02T00:00:00Z".into());
        assert_eq!(s.snapshot_id, "xusom-admin@cafef00d");
        assert_eq!(s.git_commit, "cafef00d");
        assert_eq!(s.compose_dir, "", "the old compose dir belonged to the previous commit");
        assert_eq!(s.work_dir, "/home/me/work", "work_dir is still the right place to unpack into");
        assert_eq!(s.last_received_at, "2026-01-02T00:00:00Z");
        assert_eq!(s.created_at, "2026-01-01T00:00:00Z", "created_at never moves");
    }

    #[test]
    fn a_session_round_trips_through_json_unchanged() {
        let mut s = session();
        s.record_run("/w".into(), "/w/xusom-admin-deadbeef".into());
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<ReceivedSession>(&json).unwrap(), s);
    }
}
