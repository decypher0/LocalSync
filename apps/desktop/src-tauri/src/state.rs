use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ls_containers::RunningSession;
use ls_security::VerifiedSnapshot;

/// One receiver whose connection was kept open past the initial
/// `share_snapshot` exchange (round 11), so the sender can push a targeted
/// update or receive a pull request without a fresh room-code handshake.
/// `peer_id` is the room id that connection was established under - the
/// natural per-connection unique id, already generated fresh per
/// `start_send_session` call.
pub struct ConnectedReceiver {
    pub peer_id: String,
    pub connected_at: time::OffsetDateTime,
    pub conn: Arc<ls_net::DataChannelConn>,
    /// The project path this receiver was originally sent from — reused for
    /// a later targeted push or an accepted pull request, so the frontend
    /// doesn't have to remember/resupply it per roster entry.
    pub project_path: String,
}

/// Sender-side (round 23): one Cloud drop upload whose signaling connection
/// is being kept open so a receiver can send a `CloudAccessRequest` on it.
/// Keyed by `peer_id` (the room id it was hosted under), same convention as
/// [`ConnectedReceiver`] - this is a parallel roster, not a replacement,
/// since a Cloud-drop send never opens a bulk-transfer data channel at all
/// (the payload already left over HTTP to Drive; this connection only ever
/// carries `ControlMessage`s).
pub struct CloudDropUpload {
    pub conn: Arc<ls_net::DataChannelConn>,
    pub file_id: String,
    pub retention: ls_clouddrop::retention::Retention,
}

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
    /// Sender-side roster (round 11): receivers whose connection is being
    /// kept open, keyed by `peer_id`. Populated by `commands::share_snapshot`
    /// after its initial transfer succeeds; read by `list_connected_receivers`
    /// / `push_update` / `respond_to_pull_request`.
    pub connected_receivers: Mutex<HashMap<String, ConnectedReceiver>>,
    /// Receiver-side (round 11): this receiver's own connection back to
    /// whichever sender it last received from, kept open so it can issue a
    /// pull request. A receiver only ever tracks one at a time in this
    /// round's scope - receiving from a new sender replaces it.
    pub outgoing_conn: Mutex<Option<Arc<ls_net::DataChannelConn>>>,
    /// Sender-side (round 23), keyed by `peer_id`. See [`CloudDropUpload`].
    pub cloud_drop_uploads: Mutex<HashMap<String, CloudDropUpload>>,
    /// Sender-side (round 23): a `CloudAccessRequest`'s `google_email`,
    /// stashed here between `listen_for_cloud_access_requests` surfacing the
    /// `cloud-access-request` event and the frontend's
    /// `respond_to_cloud_access_request` call - the email has to come from
    /// *somewhere* on accept (it's what `grant_reader_access` targets), and
    /// the control message that carried it is long gone by then. Keyed by
    /// `peer_id`, removed the moment it's responded to.
    pub cloud_access_requests: Mutex<HashMap<String, String>>,
}
