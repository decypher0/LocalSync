use std::collections::HashMap;
use std::sync::Mutex;

use ls_containers::RunningSession;
use ls_security::VerifiedSnapshot;

/// App-wide state, held by Tauri and looked up by the ids handed back to the
/// frontend from `receive_snapshot` / `run_snapshot`.
///
/// `std::sync::Mutex` (not tokio's) is fine here: every lock is held only
/// long enough to insert/remove/clone an entry, never across an `.await`.
#[derive(Default)]
pub struct AppState {
    /// Snapshots that passed signature verification but have not been run
    /// yet. Keyed by `"<project_name>@<git_commit>"`. Nothing outside
    /// `commands::run_snapshot` ever reads this map.
    pub verified: Mutex<HashMap<String, VerifiedSnapshot>>,
    /// Live sessions, keyed by `RunningSession::compose_project_name`
    /// (already unique per project+commit, so it doubles as the session id
    /// without minting a second identifier).
    pub sessions: Mutex<HashMap<String, RunningSession>>,
}
