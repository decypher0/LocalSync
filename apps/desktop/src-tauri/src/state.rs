use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

use ls_containers::RunningSession;
use ls_security::VerifiedSnapshot;

use crate::project_session::{FolderCommit, ProjectSession};
use crate::received_session::ReceivedSession;

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

/// Receiver-side (round 37): this device's own mDNS presence, kept alive for
/// as long as the "Make this device discoverable" toggle is on. Torn down
/// (daemon unregistered/shut down, both tasks aborted) by
/// `commands::set_discoverable(false, ..)`, or replaced wholesale by a fresh
/// one if turned back on with a new nickname.
pub struct DiscoverySession {
    pub daemon: ls_net::ServiceDaemon,
    pub fullname: String,
    /// `abort()`ed on teardown - `ls_net::host_ephemeral_relay`'s own doc
    /// comment notes its task is normally left detached (fine for a
    /// one-per-send relay that dies with the process), but a standing,
    /// explicitly-toggled-off discoverability session should actually stop
    /// listening rather than linger.
    pub relay_task: tokio::task::JoinHandle<()>,
    // `tauri::async_runtime::spawn` (not plain `tokio::spawn`, unlike
    // `relay_task` above) returns Tauri's own JoinHandle wrapper type, not
    // `tokio::task::JoinHandle` - the two look interchangeable but aren't.
    pub listen_task: tauri::async_runtime::JoinHandle<()>,
}

/// Receiver-side (round 37): one connection a sender opened via discovery,
/// held here between `commands::listen_for_discovery_connections` surfacing
/// a `connection-request` event and the frontend's
/// `respond_to_connection_request` call - same "stash it because the
/// frontend needs a moment to ask a human" shape as
/// [`AppState::cloud_access_requests`], just holding the live connection
/// itself (there's no `receive_snapshot`-style caller already holding it)
/// rather than a string.
///
/// ponytail: keyed by the one standing discovery room id, so only one
/// pending request is tracked at a time - a second sender connecting to the
/// same discoverable device before the first request is answered overwrites
/// this entry (the abandoned first connection is simply dropped). Matches
/// this app's existing single-outgoing-connection-per-receiver boundary
/// (see `AppState::outgoing_conn`'s doc comment); a real per-connection
/// queue would need its own identifier scheme for what's expected to be a
/// rare race in practice.
pub struct PendingConnectionRequest {
    pub conn: Arc<ls_net::DataChannelConn>,
    pub sender_name: String,
}

/// Sender-side (round 37): a live mDNS browse session, kept alive for as
/// long as the Send wizard's Local-network step is showing the
/// nearby-devices list.
pub struct DiscoveryBrowseSession {
    pub daemon: ls_net::ServiceDaemon,
    pub task: tauri::async_runtime::JoinHandle<()>,
}

/// A built, compressed, signed snapshot, serialized and ready to send -
/// the "prepared artifact" a [`ProjectSession`] holds so that sending to
/// another device, or retrying a failed/expired send, never rebuilds the
/// project. Valid only while the folders' commits still equal `commits`;
/// see `session_commands::ensure_artifact`.
pub struct CachedArtifact {
    pub snapshot_id: String,
    /// The snapshot, serialized exactly as it goes over the wire.
    pub bytes: Vec<u8>,
    /// The commit each of the session's folders was at when this was built.
    pub commits: Vec<FolderCommit>,
    /// RFC3339.
    pub built_at: String,
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
    /// Receiver-side (round 37): `Some` for as long as "Make this device
    /// discoverable" is on. See [`DiscoverySession`].
    pub discovery: Mutex<Option<DiscoverySession>>,
    /// Receiver-side (round 37): a sender's `ConnectionRequest`, awaiting
    /// this device's Accept/Reject. See [`PendingConnectionRequest`].
    pub pending_connection_requests: Mutex<HashMap<String, PendingConnectionRequest>>,
    /// Sender-side (round 37): `Some` for as long as the nearby-devices list
    /// is being browsed for. See [`DiscoveryBrowseSession`].
    pub discovery_browse: Mutex<Option<DiscoveryBrowseSession>>,
    /// Sender-side (round 37): currently-resolved nearby devices, kept live
    /// by a background task translating mDNS `ServiceEvent`s -
    /// `commands::list_nearby_devices` just snapshots this rather than
    /// talking to mDNS directly itself, the same "background task owns the
    /// live state, a command polls a snapshot of it" shape
    /// `connected_receivers`/`list_connected_receivers` already uses. Keyed
    /// by the mDNS record's own fullname (stable per announcing instance,
    /// unlike a nickname two devices could share).
    pub nearby_devices: Mutex<HashMap<String, ls_net::DiscoveredPeer>>,
    /// Sender-side (session-model refactor): every project session currently
    /// open, keyed by its id - the single owner of "what is being sent, and
    /// to whom it has been sent". See [`ProjectSession`]. A connection is
    /// never stored here (or anywhere): each send connects, transfers, and
    /// disconnects.
    pub project_sessions: Mutex<HashMap<String, ProjectSession>>,
    /// Built artifacts, keyed `"<session id>|<per-folder parent commits>"` -
    /// `-` for "no parent", so the plain full-diff artifact used for a first
    /// send/retry is the `-` key and each device that needs a diff against
    /// its own marker gets its own entry.
    pub artifacts: Mutex<HashMap<String, Arc<CachedArtifact>>>,
    /// How many times a session's snapshot has actually been built - lets a
    /// test prove a retry/second device reuses the artifact instead of
    /// rebuilding it.
    pub artifact_builds: AtomicUsize,
    /// Receiver-side (session-model refactor): every received session
    /// currently open, keyed by its own id - the receiving counterpart to
    /// `project_sessions`. See [`ReceivedSession`].
    pub received_sessions: Mutex<HashMap<String, ReceivedSession>>,
    /// Receiver-side (session-model refactor): explicit "I'm ready for the
    /// next push to land on this session" arming - **not** persisted (reset
    /// every app start, by design: explicit, not passive - a session that
    /// was armed before a restart is not silently still armed after one).
    /// Keyed `"<sender_pubkey_hex>|<project title>"` so a push is matched to
    /// the right armed session by who sent it and what project it is, not by
    /// connection identity (a fresh ephemeral connection every time has
    /// none - see `receiver_session_commands`'s doc comment). Value is the
    /// `ReceivedSession::id` to update.
    pub armed_updates: Mutex<HashMap<String, String>>,
}
