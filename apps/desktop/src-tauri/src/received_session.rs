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
    /// from, and half of what a later push is matched to this session by
    /// (see [`ReceivedSession::matches`]).
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
    /// The name the person gave this session (New Session -> Name). Shown
    /// everywhere instead of `title`, which is the project's own name and
    /// stays as-is because the session's containers are named from it.
    /// Empty = not named; the UI falls back to `title`.
    #[serde(default)]
    pub name: String,
    /// The version this session ran before the latest update landed, if it
    /// had been run: `(compose_dir, git_commit)`. Its containers may still be
    /// up (an update can arrive while the old version runs), so the next Run
    /// - and Stop - take them down first; otherwise they'd hold the ports the
    /// new version needs, with nothing left on record to stop them by.
    #[serde(default)]
    pub previous_run: Option<(String, String)>,
    /// `manifest.project_id` of the last push: the sender's own id for the
    /// project, the other half of the match. `None` for a session only ever
    /// sent snapshots from before it existed (matched by title instead).
    /// Every field from here on is `#[serde(default)]` so a session saved
    /// before it existed still loads.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The last push's per-folder commits, `manifest.folders` order (just
    /// `git_commit` for a single-folder manifest) - what a sender asking
    /// "which version do you hold?" (`BaseCheck`) is told.
    #[serde(default)]
    pub folder_commits: Vec<String>,
    /// The sender's name in this device's known peers when the last push
    /// arrived; empty if it wasn't one. Shown when the known peers can't say
    /// (`receiver_session_commands::build_view` asks them first).
    #[serde(default)]
    pub sender_name: String,
    /// The databases the current version carries a dump of.
    #[serde(default)]
    pub databases: Vec<ReceivedDatabase>,
    /// `manifest.db_seed_hash` of the current version.
    #[serde(default)]
    pub db_seed_hash: String,
    /// The database volume the current version last ran on - usually the one
    /// its `db_seed_hash` names, but an update that kept the receiver's data
    /// runs on the volume of the version before it. Empty: no database, or
    /// not run yet.
    #[serde(default)]
    pub db_volume: String,
    /// RFC3339 of the last Run that came up, empty if none.
    #[serde(default)]
    pub last_run_at: String,
    /// RFC3339 of the last Stop or failed Run, empty if none.
    #[serde(default)]
    pub last_stopped_at: String,
    /// How the last run ended: "stopped", "failed", or empty (still running,
    /// or never run).
    #[serde(default)]
    pub last_exit: String,
    /// An update that landed on this session after it had run, until it is
    /// applied (and comes up healthy) or rolled back. See [`PendingUpdate`].
    #[serde(default)]
    pub pending_update: Option<PendingUpdate>,
}

/// One database a received version carries a dump of
/// (`manifest.database_dumps`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReceivedDatabase {
    pub engine: String,
    pub schema: String,
}

/// What a session needs of a version to run it again - kept for the version
/// an update replaces, so Roll back can return to it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VersionRecord {
    /// The project's name at that version - its containers are named from it.
    pub title: String,
    pub snapshot_id: String,
    pub git_commit: String,
    pub work_dir: String,
    pub compose_dir: String,
    pub folder_commits: Vec<String>,
    pub db_seed_hash: String,
    pub databases: Vec<ReceivedDatabase>,
    pub db_volume: String,
}

/// An update waiting on a session that had already run: the version it
/// replaces stays on disk (and running, until Apply takes it down) until the
/// new one is up and healthy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingUpdate {
    /// The version that ran before this update - what Roll back returns to.
    pub previous: VersionRecord,
    /// The sender's database changed since that version (a different
    /// `db_seed_hash`) and that version had one: Apply asks whether to keep
    /// the receiver's data or replace it.
    pub db_changed: bool,
    /// Set by Apply: the previous version was running when it was taken
    /// down, so Roll back brings it back up.
    #[serde(default)]
    pub previous_was_running: bool,
}

/// The answer to "the sender's database changed": keep this device's data
/// (the volume the previous version ran on) or replace it with the sender's
/// dump, imported fresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbChoice {
    Keep,
    Replace,
}

impl DbChoice {
    /// `None` (not asked) means Keep - the default, which loses nothing.
    pub fn parse(choice: Option<&str>) -> Result<Self, String> {
        match choice.map(str::trim) {
            None | Some("") | Some("keep") => Ok(DbChoice::Keep),
            Some("replace") => Ok(DbChoice::Replace),
            Some(other) => Err(format!("unknown database choice \"{other}\"")),
        }
    }
}

/// Which database volume applying an update runs the new version on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateVolume {
    /// The one the new version's `db_seed_hash` names (nothing local to keep).
    Default,
    /// The previous version's volume: the receiver's data stays.
    Reuse(String),
    /// The new version's own volume, emptied first so its dump is imported
    /// fresh.
    Fresh,
}

/// What one received snapshot says about the version it carries -
/// everything landing it on a session changes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Arrival {
    /// `manifest.project_name`: it can change between versions (a folder
    /// added or renamed) without being another project.
    pub title: String,
    pub snapshot_id: String,
    pub git_commit: String,
    pub project_id: Option<String>,
    pub folder_commits: Vec<String>,
    pub db_seed_hash: String,
    pub databases: Vec<ReceivedDatabase>,
    /// The sender's name in this device's known peers, empty if it isn't one.
    pub sender_name: String,
    pub received_at: String,
}

impl Arrival {
    pub fn from_manifest(manifest: &ls_snapshot::Manifest, snapshot_id: &str, sender_name: String, received_at: String) -> Self {
        let folder_commits = if manifest.folders.is_empty() {
            vec![manifest.git_commit.clone()]
        } else {
            manifest.folders.iter().map(|f| f.git_commit.clone()).collect()
        };
        Arrival {
            title: manifest.project_name.clone(),
            snapshot_id: snapshot_id.to_string(),
            git_commit: manifest.git_commit.clone(),
            project_id: manifest.project_id.clone(),
            folder_commits,
            db_seed_hash: manifest.db_seed_hash.clone(),
            databases: manifest
                .database_dumps
                .iter()
                .map(|d| ReceivedDatabase { engine: d.engine.clone(), schema: d.schema.clone() })
                .collect(),
            sender_name,
            received_at,
        }
    }
}

/// The session a push from `sender_pubkey_hex` lands on, if any: the one
/// with the same `project_id`, or - when either side has none (a snapshot or
/// session from before it existed) - the same title. Two different project
/// ids never match, whatever the titles. The most recently received wins if
/// several do (two copies received before updates were matched).
pub fn best_match<'a>(
    candidates: impl IntoIterator<Item = &'a ReceivedSession>,
    sender_pubkey_hex: &str,
    project_id: Option<&str>,
    title: &str,
) -> Option<&'a ReceivedSession> {
    let newest = |found: Vec<&'a ReceivedSession>| found.into_iter().max_by(|a, b| a.last_received_at.cmp(&b.last_received_at));
    let candidates: Vec<&'a ReceivedSession> = candidates.into_iter().filter(|s| s.sender_pubkey_hex == sender_pubkey_hex).collect();
    let by_id: Vec<&'a ReceivedSession> = candidates.iter().copied().filter(|s| project_id.is_some() && s.project_id.as_deref() == project_id).collect();
    if !by_id.is_empty() {
        return newest(by_id);
    }
    newest(candidates.into_iter().filter(|s| s.matches(sender_pubkey_hex, project_id, title)).collect())
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
            name: String::new(),
            previous_run: None,
            project_id: None,
            folder_commits: Vec::new(),
            sender_name: String::new(),
            databases: Vec::new(),
            db_seed_hash: String::new(),
            db_volume: String::new(),
            last_run_at: String::new(),
            last_stopped_at: String::new(),
            last_exit: String::new(),
            pending_update: None,
        }
    }

    /// A fresh session for a push that matched none (see [`best_match`]).
    pub fn from_arrival(id: String, title: String, sender_pubkey_hex: String, arrival: Arrival) -> Self {
        let mut s = Self::new(
            id,
            title,
            sender_pubkey_hex,
            arrival.snapshot_id.clone(),
            arrival.git_commit.clone(),
            arrival.received_at.clone(),
        );
        s.take_arrival_fields(arrival);
        s
    }

    /// Whether a push from `sender_pubkey_hex` of `project_id` / `title`
    /// is an update to this session: same sender, and the same project id -
    /// or, when either side has none, the same title.
    pub fn matches(&self, sender_pubkey_hex: &str, project_id: Option<&str>, title: &str) -> bool {
        if self.sender_pubkey_hex != sender_pubkey_hex {
            return false;
        }
        match (project_id, self.project_id.as_deref()) {
            (Some(a), Some(b)) => a == b,
            _ => self.title == title,
        }
    }

    /// A push matched this session: move to its version. If the session had
    /// run (and no update is already waiting), the version that ran is kept
    /// as the pending update's `previous`, and its containers - which may
    /// still be up - stay on record in `previous_run`. An update already
    /// waiting is simply replaced: its `previous` is still the version that
    /// ran.
    pub fn land_update(&mut self, arrival: Arrival) {
        if self.pending_update.is_none() && !self.compose_dir.is_empty() {
            self.pending_update = Some(PendingUpdate { previous: self.current_version(), db_changed: false, previous_was_running: false });
        }
        self.record_update(arrival.snapshot_id.clone(), arrival.git_commit.clone(), String::new(), arrival.received_at.clone());
        if !arrival.title.is_empty() {
            self.title = arrival.title.clone();
        }
        self.db_volume.clear();
        self.take_arrival_fields(arrival);
        if let Some(p) = &mut self.pending_update {
            p.db_changed = !p.previous.db_volume.is_empty() && p.previous.db_seed_hash != self.db_seed_hash;
        }
    }

    fn take_arrival_fields(&mut self, arrival: Arrival) {
        if arrival.project_id.is_some() {
            self.project_id = arrival.project_id;
        }
        self.folder_commits = arrival.folder_commits;
        self.db_seed_hash = arrival.db_seed_hash;
        self.databases = arrival.databases;
        if !arrival.sender_name.is_empty() {
            self.sender_name = arrival.sender_name;
        }
    }

    /// The title `previous_run`'s containers are named from: the version an
    /// update replaces may have had another one.
    pub fn previous_title(&self) -> &str {
        match &self.pending_update {
            Some(p) if !p.previous.title.is_empty() => &p.previous.title,
            _ => &self.title,
        }
    }

    /// The commits a sender's `BaseCheck` is answered with.
    pub fn held_commits(&self) -> Vec<String> {
        if self.folder_commits.is_empty() {
            vec![self.git_commit.clone()]
        } else {
            self.folder_commits.clone()
        }
    }

    pub fn current_version(&self) -> VersionRecord {
        VersionRecord {
            title: self.title.clone(),
            snapshot_id: self.snapshot_id.clone(),
            git_commit: self.git_commit.clone(),
            work_dir: self.work_dir.clone(),
            compose_dir: self.compose_dir.clone(),
            folder_commits: self.folder_commits.clone(),
            db_seed_hash: self.db_seed_hash.clone(),
            databases: self.databases.clone(),
            db_volume: self.db_volume.clone(),
        }
    }

    /// The volume applying the pending update runs the new version on.
    /// An unchanged dump, or a changed one the person chose to keep their
    /// data over, reuses the previous version's volume; replacing it runs on
    /// the new dump's own volume, imported fresh.
    pub fn update_volume(&self, choice: DbChoice) -> UpdateVolume {
        let Some(p) = &self.pending_update else { return UpdateVolume::Default };
        if p.previous.db_volume.is_empty() {
            return UpdateVolume::Default;
        }
        if p.db_changed && choice == DbChoice::Replace {
            return UpdateVolume::Fresh;
        }
        UpdateVolume::Reuse(p.previous.db_volume.clone())
    }

    /// The pending update came up healthy: it is the session's version now,
    /// and the one before it is let go (its folder stays on disk until the
    /// Storage view cleans it up).
    pub fn finish_update(&mut self, work_dir: String, compose_dir: String, db_volume: String, at: String) {
        self.record_run(work_dir, compose_dir);
        self.db_volume = db_volume;
        self.pending_update = None;
        self.previous_run = None;
        self.record_started(at);
    }

    /// Drops the pending update and returns to the version it would have
    /// replaced. Returns the dropped update (`None` if there was none).
    pub fn roll_back(&mut self) -> Option<PendingUpdate> {
        let pending = self.pending_update.take()?;
        let v = pending.previous.clone();
        if !v.title.is_empty() {
            self.title = v.title;
        }
        self.snapshot_id = v.snapshot_id;
        self.git_commit = v.git_commit;
        self.work_dir = v.work_dir;
        self.compose_dir = v.compose_dir;
        self.folder_commits = v.folder_commits;
        self.db_seed_hash = v.db_seed_hash;
        self.databases = v.databases;
        self.db_volume = v.db_volume;
        self.previous_run = None;
        Some(pending)
    }

    /// A Run came up.
    pub fn record_started(&mut self, at: String) {
        self.last_run_at = at;
        self.last_exit.clear();
    }

    /// A Stop took the containers down.
    pub fn record_stopped(&mut self, at: String) {
        self.last_stopped_at = at;
        self.last_exit = "stopped".to_string();
    }

    /// A Run failed to come up.
    pub fn record_failed(&mut self, at: String) {
        self.last_stopped_at = at;
        self.last_exit = "failed".to_string();
    }

    /// A fresh push landed on this already-existing session (see
    /// [`ReceivedSession::land_update`]): move to the new version.
    /// `compose_dir` is the caller's call - in practice always `""` at the
    /// moment a push merely arrives (a directory built for the
    /// *previous* commit isn't valid for this one, and nothing has been
    /// unpacked/run for the new commit yet), but taking it as a parameter
    /// rather than hardcoding the clear keeps this method a plain "replace
    /// what a push changes" rather than baking in one caller's policy.
    /// `work_dir` is left alone: it's still exactly the right place to
    /// unpack the new version into.
    pub fn record_update(&mut self, snapshot_id: String, git_commit: String, compose_dir: String, received_at: String) {
        if !self.compose_dir.is_empty() {
            self.previous_run = Some((std::mem::take(&mut self.compose_dir), self.git_commit.clone()));
        }
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
        assert_eq!(
            s.previous_run,
            Some(("/home/me/work/xusom-admin-deadbeef".into(), "deadbeef".into())),
            "the old version may still be running - keep what it takes to stop it"
        );
    }

    #[test]
    fn an_update_to_a_never_run_version_leaves_no_previous_run() {
        let mut s = session();
        s.record_update("xusom-admin@cafef00d".into(), "cafef00d".into(), String::new(), "2026-01-02T00:00:00Z".into());
        assert_eq!(s.previous_run, None);
    }

    fn arrival(commit: &str, project_id: Option<&str>, db_seed_hash: &str) -> Arrival {
        Arrival {
            title: "xusom-admin".into(),
            snapshot_id: format!("xusom-admin@{commit}"),
            git_commit: commit.into(),
            project_id: project_id.map(String::from),
            folder_commits: vec![commit.into()],
            db_seed_hash: db_seed_hash.into(),
            databases: vec![ReceivedDatabase { engine: "mysql".into(), schema: "appdb".into() }],
            sender_name: "Alice".into(),
            received_at: format!("2026-01-02T00:00:{}Z", commit.len()),
        }
    }

    /// A session that received `deadbeef` with dump hash `h1` and ran it on
    /// that dump's volume.
    fn ran_session() -> ReceivedSession {
        let mut s = ReceivedSession::from_arrival("r1".into(), "xusom-admin".into(), "abc123".into(), arrival("deadbeef", Some("p1"), "h1"));
        s.record_run("/w".into(), "/w/xusom-admin-deadbeef".into());
        s.db_volume = "localsync-db-h1".into();
        s
    }

    #[test]
    fn a_push_matches_by_sender_and_project_id_or_by_title_when_either_has_none() {
        let s = ran_session();
        assert!(s.matches("abc123", Some("p1"), "renamed-project"), "the project id decides, not the title");
        assert!(!s.matches("abc123", Some("p2"), "xusom-admin"), "a different project from the same sender is not an update");
        assert!(!s.matches("someone-else", Some("p1"), "xusom-admin"), "nor is the same project id from another sender");
        assert!(s.matches("abc123", None, "xusom-admin"), "a snapshot from before project ids matches by title");
        assert!(!s.matches("abc123", None, "other"));
        let mut old = s.clone();
        old.project_id = None;
        assert!(old.matches("abc123", Some("p9"), "xusom-admin"), "so does a session from before them");
    }

    #[test]
    fn best_match_prefers_the_project_id_then_the_newest() {
        let a = ran_session();
        let mut b = ran_session();
        b.id = "r2".into();
        b.project_id = None;
        b.last_received_at = "2026-09-01T00:00:00Z".into();
        let mut c = ran_session();
        c.id = "r3".into();
        c.project_id = Some("p2".into());
        let all = [a.clone(), b.clone(), c.clone()];
        assert_eq!(best_match(&all, "abc123", Some("p1"), "xusom-admin").map(|s| s.id.as_str()), Some("r1"), "the exact project id wins over a newer title match");
        assert_eq!(best_match(&all, "abc123", Some("p3"), "xusom-admin").map(|s| s.id.as_str()), Some("r2"), "no id match: the title match on the session without one");
        assert_eq!(best_match(&all, "abc123", None, "xusom-admin").map(|s| s.id.as_str()), Some("r2"), "several title matches: the newest");
        assert!(best_match(&all, "abc123", Some("p3"), "other").is_none());
        assert!(best_match(&all, "nobody", None, "xusom-admin").is_none());
    }

    #[test]
    fn an_update_to_a_session_that_ran_keeps_the_version_it_replaces() {
        let mut s = ran_session();
        s.land_update(arrival("cafef00d", Some("p1"), "h1"));
        assert_eq!(s.git_commit, "cafef00d");
        assert_eq!(s.compose_dir, "", "the new version hasn't run");
        assert_eq!(s.held_commits(), vec!["cafef00d".to_string()], "a BaseCheck is answered with the last version received");
        let p = s.pending_update.clone().expect("an update to a session that ran waits to be applied");
        assert_eq!(p.previous.git_commit, "deadbeef");
        assert_eq!(p.previous.compose_dir, "/w/xusom-admin-deadbeef");
        assert_eq!(p.previous.db_volume, "localsync-db-h1");
        assert!(!p.db_changed, "same dump hash");
        assert_eq!(s.update_volume(DbChoice::Keep), UpdateVolume::Reuse("localsync-db-h1".into()));
        assert_eq!(s.update_volume(DbChoice::Replace), UpdateVolume::Reuse("localsync-db-h1".into()), "nothing to replace when the dump didn't change");

        // A second update before the first was applied: still measured against the version that ran.
        s.land_update(arrival("0badc0de", Some("p1"), "h2"));
        let p = s.pending_update.clone().unwrap();
        assert_eq!(p.previous.git_commit, "deadbeef", "the version that ran is still what Roll back returns to");
        assert!(p.db_changed, "the dump changed since the version that ran");
        assert_eq!(s.update_volume(DbChoice::Keep), UpdateVolume::Reuse("localsync-db-h1".into()), "keep: the old volume, not a new empty one");
        assert_eq!(s.update_volume(DbChoice::Replace), UpdateVolume::Fresh);
    }

    #[test]
    fn an_update_to_a_session_that_never_ran_has_nothing_to_apply_or_roll_back() {
        let mut s = ReceivedSession::from_arrival("r1".into(), "xusom-admin".into(), "abc123".into(), arrival("deadbeef", Some("p1"), "h1"));
        s.land_update(arrival("cafef00d", Some("p1"), "h2"));
        assert_eq!(s.pending_update, None);
        assert_eq!(s.update_volume(DbChoice::Replace), UpdateVolume::Default);
    }

    #[test]
    fn finishing_an_update_makes_it_the_version_and_lets_the_previous_go() {
        let mut s = ran_session();
        s.land_update(arrival("cafef00d", Some("p1"), "h2"));
        s.finish_update("/w".into(), "/w/xusom-admin-cafef00d".into(), "localsync-db-h1".into(), "2026-01-03T00:00:00Z".into());
        assert_eq!(s.pending_update, None);
        assert_eq!(s.previous_run, None);
        assert_eq!(s.compose_dir, "/w/xusom-admin-cafef00d");
        assert_eq!(s.db_volume, "localsync-db-h1", "kept data stays on the old volume");
        assert_eq!(s.db_seed_hash, "h2");
        assert_eq!(s.last_run_at, "2026-01-03T00:00:00Z");
        // The next update with the same dump is "unchanged" and stays on that volume too.
        s.land_update(arrival("0badc0de", Some("p1"), "h2"));
        assert!(!s.pending_update.as_ref().unwrap().db_changed);
        assert_eq!(s.update_volume(DbChoice::Keep), UpdateVolume::Reuse("localsync-db-h1".into()));
    }

    #[test]
    fn rolling_back_returns_to_the_previous_version() {
        let before = ran_session();
        let mut s = before.clone();
        let mut renamed = arrival("cafef00d", Some("p1"), "h2");
        renamed.title = "xusom-admin+api".into();
        s.land_update(renamed);
        assert_eq!(s.title, "xusom-admin+api", "the new version's containers are named from its own title");
        assert_eq!(s.previous_title(), "xusom-admin", "the previous version's still carry the old one");
        s.name = "Renamed meanwhile".into();
        let dropped = s.roll_back().expect("there was an update");
        assert_eq!(dropped.previous.git_commit, "deadbeef");
        assert_eq!(s.current_version(), before.current_version());
        assert_eq!(s.held_commits(), vec!["deadbeef".to_string()]);
        assert_eq!(s.previous_run, None);
        assert_eq!(s.pending_update, None);
        assert_eq!(s.name, "Renamed meanwhile", "only the version goes back");
        assert_eq!(s.title, "xusom-admin");
        assert!(s.roll_back().is_none());
    }

    #[test]
    fn run_outcomes_are_recorded() {
        let mut s = ran_session();
        s.record_started("t1".into());
        assert_eq!((s.last_run_at.as_str(), s.last_exit.as_str()), ("t1", ""));
        s.record_stopped("t2".into());
        assert_eq!((s.last_stopped_at.as_str(), s.last_exit.as_str()), ("t2", "stopped"));
        s.record_failed("t3".into());
        assert_eq!((s.last_stopped_at.as_str(), s.last_exit.as_str()), ("t3", "failed"));
    }

    #[test]
    fn the_database_choice_defaults_to_keep() {
        assert_eq!(DbChoice::parse(None), Ok(DbChoice::Keep));
        assert_eq!(DbChoice::parse(Some("keep")), Ok(DbChoice::Keep));
        assert_eq!(DbChoice::parse(Some("replace")), Ok(DbChoice::Replace));
        assert!(DbChoice::parse(Some("drop")).is_err());
    }

    #[test]
    fn a_session_saved_before_updates_were_matched_still_loads() {
        let json = r#"{"id":"r1","title":"t","sender_pubkey_hex":"k","snapshot_id":"t@c","git_commit":"c","work_dir":"/w","compose_dir":"/w/t-c","created_at":"x","last_received_at":"x"}"#;
        let s: ReceivedSession = serde_json::from_str(json).unwrap();
        assert_eq!(s.project_id, None);
        assert_eq!(s.pending_update, None);
        assert_eq!(s.held_commits(), vec!["c".to_string()]);
    }

    #[test]
    fn a_session_round_trips_through_json_unchanged() {
        let mut s = session();
        s.record_run("/w".into(), "/w/xusom-admin-deadbeef".into());
        s.land_update(arrival("cafef00d", Some("p1"), "h2"));
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<ReceivedSession>(&json).unwrap(), s);
    }
}
