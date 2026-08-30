//! Tauri command handlers. Each command is a thin wrapper around one of the
//! four backend crates — the crates own all real logic (bundling, crypto,
//! diffing, container orchestration); this module only adapts their
//! `anyhow::Result`s to `Result<_, String>` for the IPC boundary, tracks
//! held snapshots/sessions in `AppState`, and emits progress events.
//!
//! `run_snapshot` is the ONLY command in this file allowed to call
//! `ls_containers::run_snapshot` — the one function in the whole app that
//! executes code that arrived over the network. It only ever runs against a
//! `VerifiedSnapshot` that `receive_snapshot` already checked, and only once
//! the frontend has shown the user a diff and they clicked Run.

use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::state::AppState;

#[derive(Clone, Serialize)]
pub struct Progress {
    pub bytes: usize,
    pub total: usize,
}

/// One `run-progress` event = one new line tailed live from
/// `ls_containers::ProvisioningLog`'s file while `run_snapshot` is in
/// flight. See `tail_provisioning_log` below.
#[derive(Clone, Serialize)]
pub struct RunProgress {
    pub line: String,
}

#[derive(Clone, Serialize)]
pub struct IncomingSnapshotInfo {
    /// `"<project_name>@<git_commit>"` — hand this back to `run_snapshot`.
    pub snapshot_id: String,
    pub manifest: ls_snapshot::Manifest,
    pub diff: ls_security::DiffSummary,
    /// Hex-encoded `manifest.sender_pubkey` — the frontend needs this to
    /// round-trip a first-time sender's key into `remember_peer`.
    pub sender_pubkey_hex: String,
    /// `Some(...)` if `manifest.sender_pubkey` is already in the local
    /// known-peers store, `None` for a first-time sender. Purely
    /// informational — this never affects verification (already done above,
    /// unconditionally) or the diff-review-then-Run gate below.
    pub recognized_peer: Option<RecognizedPeer>,
}

#[derive(Clone, Serialize)]
pub struct RecognizedPeer {
    pub name: String,
    /// RFC3339 string — simplest thing that round-trips over IPC/JSON.
    pub first_seen: String,
}

/// Returned by [`start_send_session`]. `room_code` is what the user shows
/// the receiver (paste-able, spoken aloud); `room_id`/`signaling_url` are
/// what the frontend hands straight to the existing, unmodified
/// `share_snapshot`.
#[derive(Clone, Serialize)]
pub struct SendSessionInfo {
    pub room_code: String,
    pub room_id: String,
    pub signaling_url: String,
    /// How long the receiver has to paste `room_code` and click Receive
    /// before the sender's `connect_as_sender` call (started by the
    /// frontend's very next call, `share_snapshot`, right after this one
    /// resolves) gives up — `ls_net::CONNECT_TIMEOUT`, exposed here so the
    /// UI's countdown can never drift out of sync with the real backend
    /// value (round 12: this used to be a silent 30s timer that expired
    /// during the normal human copy/paste window; see `CONNECT_TIMEOUT`'s
    /// doc comment for the root cause).
    pub code_expires_in_seconds: u64,
}

/// Returned by [`decode_room_code`]. Hand straight to the existing,
/// unmodified `receive_snapshot`.
#[derive(Clone, Serialize)]
pub struct DecodedRoomCode {
    pub room_id: String,
    pub signaling_url: String,
}

#[derive(Serialize)]
pub struct RunningSessionInfo {
    /// Equal to `RunningSession::compose_project_name` — hand this back to
    /// `stop_session`.
    pub session_id: String,
    pub project_name: String,
    pub service_ports: Vec<(String, String)>,
    pub db_cache_hit: bool,
}

/// Starts a send session in one of two modes:
///
/// - `mode == "local"` (default/round-8 behavior, unchanged): hosts an
///   embedded signaling relay on the LAN and derives a room code from it, so
///   nobody has to run a separate signaling-server process or type its
///   address. `room_code` packs the relay's LAN IP + port + room id.
/// - `mode == "remote"`: a relay is already running elsewhere (self-hosted
///   `apps/signaling-server`, see README) at `relay_url` — nothing gets
///   hosted here. `room_code` is just the bare room id: since both apps
///   already have the same `relay_url` configured locally, the id alone is
///   the whole paste-able code.
///
/// Either way, the frontend shows `room_code` to the user, then calls the
/// existing, unmodified `share_snapshot(project_path, room_id,
/// signaling_url)` with the returned `room_id`/`signaling_url`.
///
/// The relay's background task (local mode only) is intentionally left
/// detached (dropping a `tokio::JoinHandle` does not abort it) - see
/// `ls_net::host_ephemeral_relay`'s doc comment for why that's fine at this
/// app's scale.
#[tauri::command]
pub async fn start_send_session(mode: String, relay_url: Option<String>) -> Result<SendSessionInfo, String> {
    if mode == "remote" {
        let signaling_url = relay_url
            .filter(|u| !u.trim().is_empty())
            .ok_or("relay_url is required in remote mode")?;
        let room_id = ls_net::generate_room_id();
        log::info!("start_send_session: remote mode, relay={signaling_url}, room_id={room_id}");
        return Ok(SendSessionInfo {
            room_code: room_id.clone(),
            room_id,
            signaling_url,
            code_expires_in_seconds: ls_net::CONNECT_TIMEOUT.as_secs(),
        });
    }

    let (port, _relay_task) = ls_net::host_ephemeral_relay().await.map_err(|e| e.to_string())?;
    let lan_ip = ls_net::detect_lan_ip().map_err(|e| e.to_string())?;
    let room_id = ls_net::generate_room_id();
    let addr = std::net::SocketAddrV4::new(lan_ip, port);
    let room_code = ls_net::encode_room_code(addr, &room_id);
    let signaling_url = format!("ws://{lan_ip}:{port}");
    log::info!("start_send_session: hosting relay on {signaling_url}, room_code={room_code}");
    Ok(SendSessionInfo {
        room_code,
        room_id,
        signaling_url,
        code_expires_in_seconds: ls_net::CONNECT_TIMEOUT.as_secs(),
    })
}

/// Decodes a room code pasted by the user into the `room_id`/`signaling_url`
/// pair the existing, unmodified `receive_snapshot` needs.
///
/// `mode == "local"` (unchanged): `code` is the packed LAN-IP/port/room-id
/// string `ls_net::decode_room_code` unpacks. `mode == "remote"`: `code` IS
/// the room id (see `start_send_session`) - paired with the locally
/// configured `relay_url`.
#[tauri::command]
pub fn decode_room_code(mode: String, code: String, relay_url: Option<String>) -> Result<DecodedRoomCode, String> {
    if mode == "remote" {
        let signaling_url = relay_url
            .filter(|u| !u.trim().is_empty())
            .ok_or("relay_url is required in remote mode")?;
        return Ok(DecodedRoomCode { room_id: code, signaling_url });
    }

    let (addr, room_id) = ls_net::decode_room_code(&code).map_err(|e| e.to_string())?;
    let signaling_url = format!("ws://{}:{}", addr.ip(), addr.port());
    Ok(DecodedRoomCode { room_id, signaling_url })
}

/// Bundles `project_path` into a signed snapshot and sends it to whoever
/// joins `room_code` on the signaling server. Returns the id
/// (`project_name@git_commit`) the sender can use to recognize their own
/// send in the UI.
///
/// Round 11: once the initial transfer succeeds, the connection is *kept
/// open* rather than dropped — added to `state.connected_receivers` under
/// `room_code` as `peer_id`, with a background task listening for the
/// receiver's control-channel messages (currently just
/// [`ls_net::ControlMessage::PullRequest`], surfaced to the frontend as a
/// `pull-request` event). This is what makes multiple simultaneous
/// receivers, targeted push (`push_update`), and pull requests
/// (`respond_to_pull_request`) possible without a fresh room code each time.
// Generic over the Tauri `Runtime` (defaults to none picked here - the real
// app binds it to `Wry` via `invoke_handler!`) rather than the concrete
// `AppHandle` (= `AppHandle<Wry>`) alias, so `tests/send_flow_test.rs` can
// call this directly with a `tauri::test::mock_app()`'s `AppHandle<MockRuntime>`
// - the actual point of that test: exercising this exact function, not a
// re-implementation of it, against a runtime that doesn't need a display.
#[tauri::command]
pub async fn share_snapshot<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    project_path: String,
    room_code: String,
    signaling_url: String,
) -> Result<String, String> {
    log::info!("share_snapshot: starting for project_path={project_path} room={room_code}");
    let root = PathBuf::from(&project_path);
    // create_snapshot shells out to git and walks the filesystem — blocking
    // work that has no business running on the async command's task.
    let snapshot = tauri::async_runtime::spawn_blocking(move || ls_snapshot::create_snapshot(&root, None))
        .await
        .map_err(|e| format!("snapshot task panicked: {e}"))?
        .map_err(|e| {
            log::warn!("share_snapshot: create_snapshot failed: {e}");
            e.to_string()
        })?;

    let snapshot_id = format!("{}@{}", snapshot.manifest.project_name, snapshot.manifest.git_commit);
    log::info!("share_snapshot: snapshot created, id={snapshot_id}");

    // Same wire format ls-snapshot's own save_to_file uses: plain JSON,
    // payload bytes riding along as a JSON byte array. Simplest thing that
    // works with the (de)serializers ls-snapshot already ships.
    let bytes = serde_json::to_vec(&snapshot).map_err(|e| e.to_string())?;

    log::info!("share_snapshot: connecting to signaling");
    let conn = ls_net::connect_as_sender(&signaling_url, &room_code)
        .await
        .map_err(|e| {
            log::warn!("share_snapshot: connect_as_sender failed: {e}");
            e.to_string()
        })?;
    log::info!("share_snapshot: data channel open, sending payload ({} bytes)", bytes.len());

    ls_net::send_payload(&conn, &bytes, |sent, total| {
        let _ = app.emit("share-progress", Progress { bytes: sent, total });
    })
    .await
    .map_err(|e| {
        log::warn!("share_snapshot: send_payload failed: {e}");
        e.to_string()
    })?;

    log::info!("share_snapshot: done, id={snapshot_id}");

    let conn = std::sync::Arc::new(conn);
    state.connected_receivers.lock().map_err(|e| e.to_string())?.insert(
        room_code.clone(),
        crate::state::ConnectedReceiver {
            peer_id: room_code.clone(),
            connected_at: time::OffsetDateTime::now_utc(),
            conn: conn.clone(),
            project_path,
        },
    );
    tauri::async_runtime::spawn(listen_for_pull_requests(app, room_code));

    Ok(snapshot_id)
}

/// Background task (one per connected receiver): waits for control messages
/// on `peer_id`'s connection and surfaces a [`ls_net::ControlMessage::PullRequest`]
/// to the frontend as a `pull-request` event. Ends quietly (no panic, no
/// retry) once the connection closes or `peer_id` is removed from the
/// roster (e.g. the receiver disconnected) - there's nothing further useful
/// to listen for at that point.
async fn listen_for_pull_requests<R: tauri::Runtime>(app: AppHandle<R>, peer_id: String) {
    loop {
        let conn = {
            let receivers = app.state::<AppState>();
            let Ok(receivers) = receivers.connected_receivers.lock() else { return };
            let Some(entry) = receivers.get(&peer_id) else { return };
            entry.conn.clone()
        };

        match ls_net::recv_control(&conn).await {
            Ok(ls_net::ControlMessage::PullRequest) => {
                log::info!("share_snapshot: pull request from peer_id={peer_id}");
                let _ = app.emit("pull-request", PullRequestNotice { peer_id: peer_id.clone() });
            }
            Ok(other) => {
                // Only PullRequest ever flows receiver -> sender; anything
                // else on this side is unexpected but not fatal to the
                // listener - log and keep waiting.
                log::warn!("share_snapshot: unexpected control message from receiver: {other:?}");
            }
            Err(e) => {
                log::info!("share_snapshot: control channel for peer_id={peer_id} ended: {e:#}");
                return;
            }
        }
    }
}

#[derive(Clone, Serialize)]
pub struct PullRequestNotice {
    pub peer_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectedReceiverInfo {
    pub peer_id: String,
    /// RFC3339 string — simplest thing that round-trips over IPC/JSON.
    pub connected_at: String,
}

/// The sender's current roster (round 11): receivers whose connection is
/// still open, in the order they connected.
#[tauri::command]
pub fn list_connected_receivers(state: State<'_, AppState>) -> Result<Vec<ConnectedReceiverInfo>, String> {
    let receivers = state.connected_receivers.lock().map_err(|e| e.to_string())?;
    let mut list: Vec<_> = receivers
        .values()
        .map(|r| ConnectedReceiverInfo {
            peer_id: r.peer_id.clone(),
            connected_at: r
                .connected_at
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_else(|_| "unknown".to_string()),
        })
        .collect();
    list.sort_by(|a, b| a.connected_at.cmp(&b.connected_at));
    Ok(list)
}

/// Bundles the *current* state of `peer_id`'s original project (re-reading
/// it from disk right now — not anything cached from the first send) and
/// pushes it to that one specific connected receiver, reusing exactly the
/// same bundle/sign pipeline `share_snapshot` uses for its initial send. No
/// other connected receiver is touched — this is what makes the push
/// targeted rather than a broadcast.
#[tauri::command]
pub async fn push_update<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    peer_id: String,
) -> Result<String, String> {
    let (conn, project_path) = {
        let receivers = state.connected_receivers.lock().map_err(|e| e.to_string())?;
        let entry = receivers
            .get(&peer_id)
            .ok_or_else(|| format!("no connected receiver with id {peer_id}"))?;
        (entry.conn.clone(), entry.project_path.clone())
    };
    bundle_and_push(&app, &conn, &project_path).await
}

/// The sender's response to a `pull-request` event. Accepting bundles and
/// sends the current project state — exactly `push_update`'s pipeline, no
/// shortcuts. Declining does nothing further (no message is even sent back
/// — see `ls_net::ControlMessage`'s doc comment: a pull request only ever
/// asks, it never obligates a reply).
#[tauri::command]
pub async fn respond_to_pull_request<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    peer_id: String,
    accept: bool,
) -> Result<Option<String>, String> {
    if !accept {
        log::info!("respond_to_pull_request: declined for peer_id={peer_id}");
        return Ok(None);
    }
    let (conn, project_path) = {
        let receivers = state.connected_receivers.lock().map_err(|e| e.to_string())?;
        let entry = receivers
            .get(&peer_id)
            .ok_or_else(|| format!("no connected receiver with id {peer_id}"))?;
        (entry.conn.clone(), entry.project_path.clone())
    };
    bundle_and_push(&app, &conn, &project_path).await.map(Some)
}

/// Shared by `push_update` and an accepted `respond_to_pull_request`:
/// bundle+sign `project_path` fresh from disk right now, tell the receiver
/// one is coming (`ControlMessage::IncomingUpdate`, on the *control*
/// channel), then send it the normal way (`send_payload`, on the *bulk
/// transfer* channel — the same one `share_snapshot`'s initial send used).
async fn bundle_and_push<R: tauri::Runtime>(
    app: &AppHandle<R>,
    conn: &ls_net::DataChannelConn,
    project_path: &str,
) -> Result<String, String> {
    log::info!("bundle_and_push: starting for project_path={project_path}");
    let root = PathBuf::from(project_path);
    let snapshot = tauri::async_runtime::spawn_blocking(move || ls_snapshot::create_snapshot(&root, None))
        .await
        .map_err(|e| format!("snapshot task panicked: {e}"))?
        .map_err(|e| {
            log::warn!("bundle_and_push: create_snapshot failed: {e}");
            e.to_string()
        })?;
    let snapshot_id = format!("{}@{}", snapshot.manifest.project_name, snapshot.manifest.git_commit);
    let bytes = serde_json::to_vec(&snapshot).map_err(|e| e.to_string())?;

    ls_net::send_control(conn, &ls_net::ControlMessage::IncomingUpdate)
        .await
        .map_err(|e| e.to_string())?;
    ls_net::send_payload(conn, &bytes, |sent, total| {
        let _ = app.emit("share-progress", Progress { bytes: sent, total });
    })
    .await
    .map_err(|e| {
        log::warn!("bundle_and_push: send_payload failed: {e}");
        e.to_string()
    })?;

    log::info!("bundle_and_push: done, id={snapshot_id}");
    Ok(snapshot_id)
}

/// Receives a snapshot over the P2P channel, verifies its signature
/// (trust-on-first-use — see `ls_security::verify`'s doc comment), and
/// returns the manifest + diff for the review screen. Does **not** unpack
/// `source/` or touch containers; the verified snapshot is held in
/// `AppState` until (and unless) the user clicks Run.
///
/// Round 11: once the initial transfer is verified and held, the connection
/// is kept open — stored in `state.outgoing_conn` (replacing any prior
/// one), with a background task listening for the sender's control-channel
/// messages. A `PullRequest` can be sent anytime via `send_pull_request`; if
/// the sender pushes a fresh update (`ControlMessage::IncomingUpdate`), it's
/// received the normal way and run through this exact same
/// `finalize_received_snapshot` pipeline — held, not auto-run, exactly like
/// any other receive — and surfaced to the frontend as a `snapshot-updated`
/// event.
// Generic over `R: tauri::Runtime` for the same reason as share_snapshot
// above.
#[tauri::command]
pub async fn receive_snapshot<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    room_code: String,
    signaling_url: String,
) -> Result<IncomingSnapshotInfo, String> {
    log::info!("receive_snapshot: starting for room={room_code}");
    let conn = ls_net::connect_as_receiver(&signaling_url, &room_code)
        .await
        .map_err(|e| {
            log::warn!("receive_snapshot: connect_as_receiver failed: {e}");
            e.to_string()
        })?;
    log::info!("receive_snapshot: data channel open, receiving payload");

    let bytes = ls_net::receive_payload(&conn, |received, total| {
        let _ = app.emit("receive-progress", Progress { bytes: received, total });
    })
    .await
    .map_err(|e| {
        log::warn!("receive_snapshot: receive_payload failed: {e}");
        e.to_string()
    })?;

    let snapshot: ls_snapshot::Snapshot = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let info = finalize_received_snapshot(&state, snapshot)?;
    log::info!("receive_snapshot: done, id={}", info.snapshot_id);

    let conn = std::sync::Arc::new(conn);
    *state.outgoing_conn.lock().map_err(|e| e.to_string())? = Some(conn.clone());
    tauri::async_runtime::spawn(listen_for_pushed_updates(app, conn));

    Ok(info)
}

/// Background task (receiver side): waits for the sender to push a fresh
/// update on `conn`'s control channel. Ends quietly once the connection
/// closes, or once `state.outgoing_conn` no longer points at *this specific*
/// connection (the receiver moved on to a different sender) — checked before
/// acting on an `IncomingUpdate` so a stale listener from a superseded
/// connection can't clobber `state.verified` after the fact.
async fn listen_for_pushed_updates<R: tauri::Runtime>(app: AppHandle<R>, conn: std::sync::Arc<ls_net::DataChannelConn>) {
    loop {
        match ls_net::recv_control(&conn).await {
            Ok(ls_net::ControlMessage::IncomingUpdate) => {
                let state = app.state::<AppState>();
                let still_current = state
                    .outgoing_conn
                    .lock()
                    .ok()
                    .map(|g| g.as_ref().is_some_and(|c| std::sync::Arc::ptr_eq(c, &conn)))
                    .unwrap_or(false);
                if !still_current {
                    log::info!("receive_snapshot: pushed update arrived on a superseded connection, ignoring");
                    return;
                }
                log::info!("receive_snapshot: sender is pushing an update, receiving it");
                let bytes = match ls_net::receive_payload(&conn, |received, total| {
                    let _ = app.emit("receive-progress", Progress { bytes: received, total });
                })
                .await
                {
                    Ok(b) => b,
                    Err(e) => {
                        log::warn!("receive_snapshot: receiving pushed update failed: {e:#}");
                        continue;
                    }
                };
                let snapshot: ls_snapshot::Snapshot = match serde_json::from_slice(&bytes) {
                    Ok(s) => s,
                    Err(e) => {
                        log::warn!("receive_snapshot: pushed update was not a valid snapshot: {e}");
                        continue;
                    }
                };
                match finalize_received_snapshot(&state, snapshot) {
                    Ok(info) => {
                        log::info!("receive_snapshot: pushed update held, id={}", info.snapshot_id);
                        let _ = app.emit("snapshot-updated", info);
                    }
                    Err(e) => log::warn!("receive_snapshot: finalizing pushed update failed: {e}"),
                }
            }
            Ok(other) => {
                log::warn!("receive_snapshot: unexpected control message from sender: {other:?}");
            }
            Err(e) => {
                log::info!("receive_snapshot: control channel ended: {e:#}");
                return;
            }
        }
    }
}

/// Sends a pull request ("do you have anything new?") to whichever sender
/// this receiver last received from. Carries no payload and never can — see
/// `ls_net::ControlMessage::PullRequest`'s doc comment. Purely a signal; the
/// sender decides whether to act on it via `respond_to_pull_request`, and
/// this function has no way to influence that decision beyond the fact that
/// it was sent.
#[tauri::command]
pub async fn send_pull_request(state: State<'_, AppState>) -> Result<(), String> {
    let conn = state
        .outgoing_conn
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or("not currently connected to a sender")?;
    ls_net::send_control(&conn, &ls_net::ControlMessage::PullRequest)
        .await
        .map_err(|e| e.to_string())
}

/// Everything `receive_snapshot` does *after* the bytes are off the wire:
/// verify, diff, look up the sender's identity in the local known-peers
/// store, and hold the verified snapshot in `AppState`. Split out from
/// `receive_snapshot` so this — the actual consent-gate-relevant logic — is
/// callable from a test without a live P2P connection (`ls_net`'s WebRTC
/// handshake needs a real network and can't run in every CI/sandbox); the
/// network hop itself is `ls_net`'s own concern and already covered by
/// `tests/send_flow_test.rs`.
pub fn finalize_received_snapshot(
    state: &State<'_, AppState>,
    snapshot: ls_snapshot::Snapshot,
) -> Result<IncomingSnapshotInfo, String> {
    // Trust-on-first-use: empty trusted_keys accepts any signature that
    // checks out. Documented MVP behavior (crates/ls-security/src/verify.rs)
    // — a real keyring UI is out of scope here.
    let verified = ls_security::verify(snapshot, &[]).map_err(|e| e.to_string())?;
    let diff = ls_security::diff_summary(&verified).map_err(|e| e.to_string())?;
    let manifest = verified.snapshot().manifest.clone();
    let snapshot_id = format!("{}@{}", manifest.project_name, manifest.git_commit);
    let sender_pubkey_hex = to_hex(&manifest.sender_pubkey);

    // Identity *recognition* only — this runs after verification above has
    // already unconditionally succeeded, and only annotates the info handed
    // to the review screen. It cannot make receive_snapshot fail, and it
    // does not touch state.verified/run_snapshot's gate at all.
    let recognized_peer = match ls_security::KnownPeers::load_default() {
        Ok(peers) => peers.find(&manifest.sender_pubkey).map(|p| RecognizedPeer {
            name: p.name.clone(),
            first_seen: p
                .first_seen
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_else(|_| "unknown".to_string()),
        }),
        Err(e) => {
            log::warn!("receive_snapshot: could not load known peers ({e}) — treating as no known peers");
            None
        }
    };

    state
        .verified
        .lock()
        .map_err(|e| e.to_string())?
        .insert(snapshot_id.clone(), verified);

    Ok(IncomingSnapshotInfo { snapshot_id, manifest, diff, sender_pubkey_hex, recognized_peer })
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Result<[u8; 32], String> {
    if s.len() != 64 {
        return Err(format!("expected a 64-character hex string, got {} characters", s.len()));
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .map_err(|e| format!("invalid hex at byte {i}: {e}"))?;
    }
    Ok(out)
}

/// Saves (or renames) a sender's pubkey in the local known-peers store, so a
/// future receive from the same key shows up as recognized. Purely local
/// bookkeeping — does not touch the wire protocol, verification, or any held
/// snapshot.
#[tauri::command]
pub fn remember_peer(pubkey_hex: String, name: String) -> Result<(), String> {
    let pubkey = from_hex(&pubkey_hex)?;
    let mut peers = ls_security::KnownPeers::load_default().map_err(|e| e.to_string())?;
    peers.remember(pubkey, name).map_err(|e| e.to_string())
}

/// An explicit "no" at the connection-level review gate: discards a held
/// verified snapshot without ever running it. Independent of (not a
/// shortcut past) the separate Run gate in `run_snapshot` — this only ever
/// removes from `state.verified`, the same map `run_snapshot` uses, and
/// never calls `ls_containers::run_snapshot`.
#[tauri::command]
pub fn reject_snapshot(state: State<'_, AppState>, snapshot_id: String) -> Result<(), String> {
    state
        .verified
        .lock()
        .map_err(|e| e.to_string())?
        .remove(&snapshot_id)
        .map(|_| ())
        .ok_or_else(|| format!("no held snapshot with id {snapshot_id}"))
}

/// Polls `ls_containers::ProvisioningLog::open_default()`'s file for lines
/// appended after this task started (never replays lines from a prior Run)
/// and emits each as a `run-progress` event, so the frontend's optional
/// details view can stream `ensure_podman_ready()`'s real output live
/// instead of showing a fake progress bar. There's no OS-level tail/watch
/// primitive worth a new dependency for a file this small - a plain poll
/// loop is the whole thing. `run_snapshot` below aborts this the instant
/// `ls_containers::run_snapshot` resolves, success or failure.
///
/// If the OS data dir can't be determined (same rare case
/// `ls_containers::run_snapshot` itself falls back on), there's no file to
/// tail — the details view just stays empty, which is fine, this is a
/// nice-to-have.
async fn tail_provisioning_log<R: tauri::Runtime>(app: AppHandle<R>) {
    let path = match ls_containers::ProvisioningLog::open_default() {
        Ok(log) => log.path().to_path_buf(),
        Err(_) => return,
    };
    // Start from wherever the file already is - don't replay a previous
    // Run's history into this attempt's details view.
    let mut offset = tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0);

    loop {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let Ok(contents) = tokio::fs::read(&path).await else { continue };
        if (contents.len() as u64) <= offset {
            continue;
        }
        let new_bytes = &contents[offset as usize..];
        // Only emit whole lines - ProvisioningLog::log() writes a line per
        // call, but a poll could still land mid-write; whatever's after the
        // last '\n' is picked up on the next iteration instead of emitted
        // half-formed.
        if let Some(last_newline) = new_bytes.iter().rposition(|&b| b == b'\n') {
            for line in String::from_utf8_lossy(&new_bytes[..=last_newline]).lines() {
                let _ = app.emit("run-progress", RunProgress { line: line.to_string() });
            }
            offset += (last_newline + 1) as u64;
        }
    }
}

/// Executes a previously-received, verified snapshot in sandboxed Podman
/// containers. Only reachable after the user has seen `receive_snapshot`'s
/// diff and clicked a real Run button — there is no other path to this
/// function's one call into `ls_containers::run_snapshot`.
///
/// Signature/tamper verification already happened back in `receive_snapshot`
/// — nothing below this point can fail *that* way, only for environment
/// reasons (Podman missing, a port in use, disk I/O, ...). So the snapshot
/// is only taken out of `state.verified` for the duration of the attempt and
/// put back if it fails, rather than discarded up front: a failed Run for a
/// fixable environment reason must stay retry-able without forcing a fresh
/// Send/Receive. See `tests/run_retry_test.rs`.
// Generic over `R: tauri::Runtime` for the same reason as share_snapshot /
// receive_snapshot above - lets tests/run_retry_test.rs call this directly
// with a mock_app()'s AppHandle<MockRuntime>.
#[tauri::command]
pub async fn run_snapshot<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    snapshot_id: String,
    work_dir: String,
) -> Result<RunningSessionInfo, String> {
    let verified = state
        .verified
        .lock()
        .map_err(|e| e.to_string())?
        .remove(&snapshot_id)
        .ok_or_else(|| format!("no held snapshot with id {snapshot_id}"))?;

    // Tail the real provisioning log for the duration of the attempt only -
    // aborted the moment ls_containers::run_snapshot resolves, whichever way.
    let tail_task = tauri::async_runtime::spawn(tail_provisioning_log(app.clone()));
    let result = ls_containers::run_snapshot(&verified, &PathBuf::from(work_dir)).await;
    tail_task.abort();

    let session = match result {
        Ok(session) => session,
        Err(e) => {
            // Put it back so Run can be retried after the user fixes
            // whatever the environment problem was.
            state
                .verified
                .lock()
                .map_err(|e| e.to_string())?
                .insert(snapshot_id, verified);
            return Err(e.to_string());
        }
    };

    let info = RunningSessionInfo {
        session_id: session.compose_project_name.clone(),
        project_name: session.project_name.clone(),
        service_ports: session.service_ports.clone(),
        db_cache_hit: session.db_cache_hit,
    };

    state
        .sessions
        .lock()
        .map_err(|e| e.to_string())?
        .insert(info.session_id.clone(), session);

    Ok(info)
}

/// Tears down a running session's containers/network. The seeded DB volume
/// is intentionally left alone by `ls_containers::stop_session` — that's
/// the cache the next run reuses.
#[tauri::command]
pub async fn stop_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let session = state
        .sessions
        .lock()
        .map_err(|e| e.to_string())?
        .remove(&session_id)
        .ok_or_else(|| format!("no running session with id {session_id}"))?;

    ls_containers::stop_session(&session).await.map_err(|e| e.to_string())
}
