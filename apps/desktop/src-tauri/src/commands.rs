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

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::state::AppState;

/// Round 29: `session_id` (the room code for a send, the same one for a
/// receive) is what makes this event routable to the correct tab once the
/// frontend can have several of these in flight at once (goal B1) - every
/// `share-progress`/`receive-progress` emission fires the same *event name*
/// regardless of which session it's for, so without this field, two
/// concurrent sessions' progress bars would each receive *both* sessions'
/// updates with no way to tell them apart. `commands::Progress` itself
/// already had every value needed to fill this in at each emit site -
/// added, not backfilled from somewhere new.
#[derive(Clone, Serialize)]
pub struct Progress {
    pub session_id: String,
    pub bytes: usize,
    pub total: usize,
}

/// One `run-progress` event = one new line tailed live from
/// `ls_containers::ProvisioningLog`'s file while `run_snapshot` is in
/// flight. See `tail_provisioning_log` below. Round 29: `session_id` for
/// the same reason as `Progress` above - keyed by the *receiver's* held
/// snapshot id (there's no room code still in scope by the time Run is
/// clicked; the snapshot id is the identifier the review/run UI already
/// keys everything else on).
#[derive(Clone, Serialize)]
pub struct RunProgress {
    pub session_id: String,
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
/// Round 25 fix: no longer takes a caller-supplied `mode` at all - the two
/// modes' codes are unambiguous by shape alone, so asking the receiver to
/// separately pick one (previously read from Settings' own relay-mode
/// toggle, meant for the *sender* side) was both redundant and actively
/// wrong whenever that toggle didn't happen to match whatever mode the
/// sender actually used for a given code. Per round 10's own design intent
/// ("both apps already have the same relay_url configured locally, the id
/// alone is the whole paste-able code" - see `start_send_session`'s doc
/// comment), the code itself is what should decide this, not a separate
/// receiver choice:
///
/// - A local-mode code is always exactly 14 base62 characters (`ls_net`'s
///   private `ROOM_CODE_DIGITS`; the packed LAN-IP/port/room-id string
///   `ls_net::decode_room_code` unpacks) - tried first.
/// - A remote-mode code is always the bare, 4-character id
///   `generate_room_id()` produces (see `start_send_session`) - paired
///   with the locally configured `relay_url`, since a bare id genuinely
///   carries no address of its own to derive one from.
///
/// The two shapes never collide, so trying local first and falling back to
/// remote is a real detection, not a guess: a local-mode code always
/// parses as one, and only something that isn't shaped like one ever
/// reaches the remote branch below.
#[tauri::command]
pub fn decode_room_code(code: String, relay_url: Option<String>) -> Result<DecodedRoomCode, String> {
    if let Ok((addr, room_id)) = ls_net::decode_room_code(&code) {
        let signaling_url = format!("ws://{}:{}", addr.ip(), addr.port());
        return Ok(DecodedRoomCode { room_id, signaling_url });
    }

    // Not shaped like a local-mode code - the only other real possibility
    // is a remote-mode bare room id, which does need a separately-known
    // relay_url (that's inherent to a bare id carrying no address of its
    // own, not a mode the receiver had to pick).
    let signaling_url = relay_url
        .filter(|u| !u.trim().is_empty())
        .ok_or("This doesn't look like a local-network code, so it needs a relay URL - enter the one the sender is using (Settings, or ask them).")?;
    Ok(DecodedRoomCode { room_id: code, signaling_url })
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

    let progress_session_id = room_code.clone();
    ls_net::send_payload(&conn, &bytes, |sent, total| {
        let _ = app.emit("share-progress", Progress { session_id: progress_session_id.clone(), bytes: sent, total });
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

// ---------- round 17: guided database-source wizard for Send ----------
//
// These commands are thin wrappers around `ls_dbsource` (detect/connect/
// export) plus one new packaging+send command, `share_snapshot_wizard`,
// that generalizes `share_snapshot` above to N folders and optional
// per-folder database dumps. `share_snapshot` itself is untouched — it's
// still what `push_update`/`respond_to_pull_request` use for an existing
// session's re-bundle, and every one of its own tests keeps exercising it
// directly, unmodified.

/// JS-facing mirror of `ls_dbsource::ConnectionDetails` — kept separate
/// (rather than making the ls-dbsource type itself derive Tauri/IPC-facing
/// traits) so ls-dbsource has no reason to know this app's IPC conventions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionDetailsDto {
    pub engine: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: String,
}

impl From<ls_dbsource::ConnectionDetails> for ConnectionDetailsDto {
    fn from(d: ls_dbsource::ConnectionDetails) -> Self {
        Self {
            engine: d.engine,
            host: d.host,
            port: d.port,
            database: d.database,
            username: d.username,
            password: d.password,
        }
    }
}

impl From<ConnectionDetailsDto> for ls_dbsource::ConnectionDetails {
    fn from(d: ConnectionDetailsDto) -> Self {
        Self {
            engine: d.engine,
            host: d.host,
            port: d.port,
            database: d.database,
            username: d.username,
            password: d.password,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DetectedConnectionDto {
    pub details: ConnectionDetailsDto,
    pub source_file: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TableInfoDto {
    pub name: String,
    pub approx_row_count: Option<u64>,
}

/// Wizard step "attempt automatic connection detection per folder" — pure,
/// fast, local file parsing (Spring Boot's application.properties/.yml
/// today). No network call, safe to run for every selected folder up
/// front, before the developer has said yes/no to anything.
#[tauri::command]
pub fn detect_db_connection(folder_path: String) -> Result<Option<DetectedConnectionDto>, String> {
    let folder = PathBuf::from(&folder_path);
    Ok(ls_dbsource::detect::detect_all(&folder).map(|d| DetectedConnectionDto {
        details: d.details.into(),
        source_file: d.source_file.display().to_string(),
    }))
}

/// Wizard step "establish a real connection" — used both right after
/// detection succeeds and after the developer manually enters details
/// (deliberately the same command either way; the wizard has no separate
/// code path for manual entry beyond how it got these `details`).
#[tauri::command]
pub async fn test_db_connection(details: ConnectionDetailsDto) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || ls_dbsource::connect::test_connection(&details.into()))
        .await
        .map_err(|e| format!("db connection task panicked: {e}"))?
        // Round 18: `{e:#}` (anyhow's alternate Display), not `.to_string()`
        // (plain Display) - plain Display only ever shows the outermost
        // `with_context` message ("failed to connect to ..."), silently
        // dropping the real underlying reason (connection refused, wrong
        // password, unknown database, ...). Verified directly against real
        // failures in this sandbox: `.to_string()` produced the exact
        // generic, undiagnosable text a real screenshot showed; `{:#}`
        // produces the real reason. Same fix applied to every db-wizard
        // command below.
        .map_err(|e| format!("{e:#}"))
}

/// Wizard step "show the developer the real list of tables" before they
/// select anything — the whole point being an informed choice, not a blind
/// one.
#[tauri::command]
pub async fn list_db_tables(details: ConnectionDetailsDto) -> Result<Vec<TableInfoDto>, String> {
    let tables = tauri::async_runtime::spawn_blocking(move || ls_dbsource::connect::list_tables(&details.into()))
        .await
        .map_err(|e| format!("db list-tables task panicked: {e}"))?
        .map_err(|e| format!("{e:#}"))?;
    Ok(tables
        .into_iter()
        .map(|t| TableInfoDto { name: t.name, approx_row_count: t.approx_row_count })
        .collect())
}

/// Round 22 goal 3: wizard step "show the developer the real list of
/// databases/schemas" right after a connection succeeds - so the
/// developer picks the real, correct one instead of a typed-in or auto-
/// detected name being trusted blindly. Deliberately does not require
/// `details.database` to already be correct (see each engine's own
/// `list_databases` doc comment for how it avoids pinning to it).
#[tauri::command]
pub async fn list_db_schemas(details: ConnectionDetailsDto) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || ls_dbsource::connect::list_databases(&details.into()))
        .await
        .map_err(|e| format!("db list-schemas task panicked: {e}"))?
        .map_err(|e| format!("{e:#}"))
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportedDumpDto {
    /// Absolute path on disk — handed straight back to `share_snapshot_wizard`
    /// as a `DumpPlanDto.file_path`, same as a developer-supplied dump file
    /// would be.
    pub file_path: String,
    pub hash: String,
    pub size_bytes: u64,
}

/// Wizard step "export the full content of the selected tables" — always
/// every row of every selected table, never sampled or trimmed. Writes the
/// dump to a real file (rather than returning the bytes over IPC) so a
/// large dump doesn't have to round-trip through the webview just to get
/// packaged a moment later.
#[tauri::command]
pub async fn export_db_tables(details: ConnectionDetailsDto, tables: Vec<String>) -> Result<ExportedDumpDto, String> {
    let engine = details.engine.clone();
    let dump_bytes = tauri::async_runtime::spawn_blocking(move || ls_dbsource::export::export_tables(&details.into(), &tables))
        .await
        .map_err(|e| format!("db export task panicked: {e}"))?
        .map_err(|e| format!("{e:#}"))?;

    let hash = {
        use sha2::{Digest, Sha256};
        Sha256::digest(&dump_bytes).iter().map(|b| format!("{b:02x}")).collect::<String>()
    };

    // Same OS-data-dir convention send_log.rs already uses, under its own
    // subfolder — a dump can be a real amount of data and has no reason to
    // live next to the log file itself.
    let Some(base) = dirs::data_dir() else {
        return Err("could not determine the OS data directory to stage the export in".to_string());
    };
    let dir = base.join("localsync").join("db-exports");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Round 18: extension follows the engine, not a blanket ".sql" - a
    // MongoDB export is a tar.gz of mongodump's own BSON output, not SQL
    // text, and naming it .sql would be actively misleading even though
    // the bytes themselves are read back raw regardless of extension.
    let ext = if engine == "mongodb" { "tar.gz" } else { "sql" };
    let file_path = dir.join(format!("{hash}.{ext}"));
    std::fs::write(&file_path, &dump_bytes).map_err(|e| e.to_string())?;

    Ok(ExportedDumpDto {
        file_path: file_path.display().to_string(),
        hash,
        size_bytes: dump_bytes.len() as u64,
    })
}

/// One folder's outcome from the wizard, as decided on the frontend: either
/// no database, a developer-supplied dump file, or a freshly-exported one
/// (`export_db_tables`'s own output file, used exactly the same way as a
/// supplied one from here — the wizard's "package the result" step doesn't
/// need to know or care which).
#[derive(Debug, Clone, Deserialize)]
pub struct DumpPlanDto {
    pub schema: String,
    pub file_path: String,
    /// Round 18: one of `ls_dbsource::SUPPORTED_ENGINES` - carried through
    /// into `ls_snapshot::PendingDump`/`DatabaseDumpEntry` so a future
    /// restore step knows which tool a given dump needs. Set by app.js
    /// from whichever `ConnectionDetails.engine` this folder's dump came
    /// from - detection/manual-entry always has one by the time a dump
    /// exists, so there's no engine-less case to handle here.
    pub engine: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FolderPlanDto {
    pub path: String,
    pub dump: Option<DumpPlanDto>,
}

/// Wizard step "package the result" + send — generalizes `share_snapshot`
/// (still unmodified, still used by `push_update`/`respond_to_pull_request`)
/// to N folders and per-folder database dumps via
/// `ls_snapshot::create_snapshot_multi`. Reads each `DumpPlanDto.file_path`
/// off disk (works identically whether that file came from
/// `export_db_tables` or a developer's own Browse… selection — the wizard
/// never has to distinguish the two once it has a path), then follows the
/// exact same create → serialize → connect → send sequence `share_snapshot`
/// does.
#[tauri::command]
pub async fn share_snapshot_wizard<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    folders: Vec<FolderPlanDto>,
    room_code: String,
    signaling_url: String,
) -> Result<String, String> {
    log::info!(
        "share_snapshot_wizard: starting for {} folder(s), room={room_code}",
        folders.len()
    );
    if folders.is_empty() {
        return Err("at least one folder is required".to_string());
    }

    let mut folder_specs = Vec::with_capacity(folders.len());
    let mut pending_dumps = Vec::new();
    for (i, f) in folders.iter().enumerate() {
        folder_specs.push(ls_snapshot::FolderSpec {
            path: PathBuf::from(&f.path),
            parent_commit: None,
        });
        if let Some(dump) = &f.dump {
            // Round: no `fs::read` here anymore — a real database dump can
            // be multi-GB, and reading it fully into memory just to hand it
            // to `create_snapshot_multi` a moment later (which used to
            // `.clone()` it again besides) is exactly what produced a real
            // `out of memory` failure. `DumpSource::FilePath` lets the file
            // stay on disk, opened and streamed in bounded chunks only when
            // `create_snapshot_multi`/`merge_folder_payloads` actually need
            // its bytes (hashing, then tar-appending). Fail fast here if the
            // path doesn't exist/isn't readable, rather than surfacing that
            // error deep inside a spawn_blocking task with a less obvious
            // message.
            let meta = std::fs::metadata(&dump.file_path)
                .map_err(|e| format!("reading dump file {}: {e}", dump.file_path))?;
            if !meta.is_file() {
                return Err(format!("reading dump file {}: not a regular file", dump.file_path));
            }
            pending_dumps.push(ls_snapshot::PendingDump {
                folder_index: i,
                schema: dump.schema.clone(),
                source: ls_snapshot::DumpSource::FilePath(PathBuf::from(&dump.file_path)),
                engine: dump.engine.clone(),
            });
        }
    }

    let snapshot =
        tauri::async_runtime::spawn_blocking(move || ls_snapshot::create_snapshot_multi(&folder_specs, &pending_dumps))
            .await
            .map_err(|e| format!("snapshot task panicked: {e}"))?
            .map_err(|e| {
                log::warn!("share_snapshot_wizard: create_snapshot_multi failed: {e}");
                e.to_string()
            })?;

    let snapshot_id = format!("{}@{}", snapshot.manifest.project_name, snapshot.manifest.git_commit);
    log::info!("share_snapshot_wizard: snapshot created, id={snapshot_id}");

    let bytes = serde_json::to_vec(&snapshot).map_err(|e| e.to_string())?;

    log::info!("share_snapshot_wizard: connecting to signaling");
    let conn = ls_net::connect_as_sender(&signaling_url, &room_code).await.map_err(|e| {
        log::warn!("share_snapshot_wizard: connect_as_sender failed: {e}");
        e.to_string()
    })?;
    log::info!(
        "share_snapshot_wizard: data channel open, sending payload ({} bytes)",
        bytes.len()
    );

    let progress_session_id = room_code.clone();
    ls_net::send_payload(&conn, &bytes, |sent, total| {
        let _ = app.emit("share-progress", Progress { session_id: progress_session_id.clone(), bytes: sent, total });
    })
    .await
    .map_err(|e| {
        log::warn!("share_snapshot_wizard: send_payload failed: {e}");
        e.to_string()
    })?;

    log::info!("share_snapshot_wizard: done, id={snapshot_id}");

    let conn = std::sync::Arc::new(conn);
    // Note (round 17): only the *first* folder's path is kept here, for the
    // "previously connected" roster and as the base a later `push_update`/
    // `respond_to_pull_request` re-bundles from. Both of those still call
    // the original, single-folder `create_snapshot` (unmodified this
    // round) — so a push/pull-request against a wizard-originated
    // multi-folder send re-bundles only that first folder, silently
    // dropping any other folders and any database dumps. Extending
    // push/pull-request to multi-folder sends is out of round 17's scope
    // (see that round's report); flagged here so it reads as a deliberate,
    // documented boundary rather than an oversight.
    let first_folder_path = folders[0].path.clone();
    state.connected_receivers.lock().map_err(|e| e.to_string())?.insert(
        room_code.clone(),
        crate::state::ConnectedReceiver {
            peer_id: room_code.clone(),
            connected_at: time::OffsetDateTime::now_utc(),
            conn: conn.clone(),
            project_path: first_folder_path,
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
    bundle_and_push(&app, &conn, &project_path, &peer_id).await
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
    bundle_and_push(&app, &conn, &project_path, &peer_id).await.map(Some)
}

/// Shared by `push_update` and an accepted `respond_to_pull_request`:
/// bundle+sign `project_path` fresh from disk right now, tell the receiver
/// one is coming (`ControlMessage::IncomingUpdate`, on the *control*
/// channel), then send it the normal way (`send_payload`, on the *bulk
/// transfer* channel — the same one `share_snapshot`'s initial send used).
/// Round 29: `session_id` (the receiver's `peer_id`, already how
/// `connected_receivers` keys this same connection) rides along on the
/// progress events so the sender's per-session tab can tell a push/pull
/// re-send's progress apart from any other session's.
async fn bundle_and_push<R: tauri::Runtime>(
    app: &AppHandle<R>,
    conn: &ls_net::DataChannelConn,
    project_path: &str,
    session_id: &str,
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
    let progress_session_id = session_id.to_string();
    ls_net::send_payload(conn, &bytes, |sent, total| {
        let _ = app.emit("share-progress", Progress { session_id: progress_session_id.clone(), bytes: sent, total });
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

    let progress_session_id = room_code.clone();
    let bytes = ls_net::receive_payload(&conn, |received, total| {
        let _ = app.emit("receive-progress", Progress { session_id: progress_session_id.clone(), bytes: received, total });
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
    tauri::async_runtime::spawn(listen_for_pushed_updates(app, conn, room_code));

    Ok(info)
}

/// Background task (receiver side): waits for the sender to push a fresh
/// update on `conn`'s control channel. Ends quietly once the connection
/// closes, or once `state.outgoing_conn` no longer points at *this specific*
/// connection (the receiver moved on to a different sender) — checked before
/// acting on an `IncomingUpdate` so a stale listener from a superseded
/// connection can't clobber `state.verified` after the fact.
async fn listen_for_pushed_updates<R: tauri::Runtime>(
    app: AppHandle<R>,
    conn: std::sync::Arc<ls_net::DataChannelConn>,
    session_id: String,
) {
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
                    let _ = app.emit("receive-progress", Progress { session_id: session_id.clone(), bytes: received, total });
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
// Round 29: `session_id` (the snapshot id `run_snapshot` was called with)
// tags every emitted line so the frontend can route it to the right tab.
// Known, narrower boundary this round doesn't fix: `ProvisioningLog` is one
// shared file across every Run attempt (see its own doc comment - never
// per-snapshot), so if two Runs are genuinely concurrent, both of their
// tail tasks poll the *same* file and each could see the other's lines
// mixed in with a real session_id tag that isn't actually always accurate
// for interleaved output - the container orchestration itself
// (`ls_containers::run_snapshot`) already supports concurrent runs via
// unique compose project names; only this live-log-tailing convenience
// view can blend two truly-simultaneous Runs' output. Flagged rather than
// silently presented as fully solved - see this round's own report.
async fn tail_provisioning_log<R: tauri::Runtime>(app: AppHandle<R>, session_id: String) {
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
                let _ = app.emit("run-progress", RunProgress { session_id: session_id.clone(), line: line.to_string() });
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
    let tail_task = tauri::async_runtime::spawn(tail_provisioning_log(app.clone(), snapshot_id.clone()));
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

// ---------- round 23: Cloud drop transport (Google Drive) ----------
//
// A third connectivity mode alongside Local network (round 8) and Remote
// relay (round 10): the bulk payload travels over the sender's and
// receiver's own Google Drive (`ls_clouddrop::drive`) instead of a P2P data
// channel. `ls_net`'s signaling/relay machinery is still reused, but only
// for its *control* channel — the identity-request/response handshake below
// — never for the payload itself. Everything past a successful download
// (`finalize_received_snapshot`, the review/consent/Run gate) is the exact
// same, unmodified pipeline every other transport already uses.

#[derive(Clone, Serialize)]
pub struct LinkedAccountInfo {
    pub email: String,
}

fn cloud_drop_config() -> Result<ls_clouddrop::oauth::OAuthConfig, String> {
    ls_clouddrop::oauth::OAuthConfig::from_env().map_err(|e| e.to_string())
}

/// Opens the system browser to Google's real consent screen and blocks until
/// the user finishes (or cancels) sign-in. Requires both
/// `GOOGLE_OAUTH_CLIENT_ID` and `GOOGLE_OAUTH_CLIENT_SECRET` to be set (round
/// 31: Google's real token endpoint rejects this app's exchange without the
/// secret, despite PKCE - see `docs/google-drive-setup.md`) — without either
/// one this fails immediately with a message pointing there, rather than
/// trying to build a request Google would just reject.
#[tauri::command]
pub async fn link_google_account<R: tauri::Runtime>(app: AppHandle<R>) -> Result<LinkedAccountInfo, String> {
    let config = cloud_drop_config()?;
    let opener = app.clone();
    let tokens = ls_clouddrop::oauth::run_oauth_flow(&config, ls_clouddrop::oauth::CLOUD_DROP_SCOPES, move |url| {
        use tauri_plugin_opener::OpenerExt;
        if let Err(e) = opener.opener().open_url(url, None::<&str>) {
            log::warn!("link_google_account: could not open the system browser automatically: {e}");
        }
    })
    .await
    .map_err(|e| e.to_string())?;

    let email = tokens.email.clone();
    ls_clouddrop::store::save_tokens(&tokens).map_err(|e| e.to_string())?;
    log::info!("link_google_account: linked {email}");
    Ok(LinkedAccountInfo { email })
}

/// `None` if no account is linked yet — Settings shows a "Link Google
/// account" action either way, this just decides whether to also show whose
/// account it already is.
#[tauri::command]
pub fn google_account_status() -> Result<Option<LinkedAccountInfo>, String> {
    Ok(ls_clouddrop::store::load_tokens()
        .map_err(|e| e.to_string())?
        .map(|t| LinkedAccountInfo { email: t.email }))
}

#[tauri::command]
pub fn unlink_google_account() -> Result<(), String> {
    ls_clouddrop::store::clear_tokens().map_err(|e| e.to_string())
}

/// Mirrors `ls_clouddrop::retention::Retention` at the IPC boundary — kept
/// separate so that crate has no reason to know this app's serde/IPC
/// conventions (same split `ConnectionDetailsDto` uses for `ls_dbsource`).
/// The frontend computes the concrete instant for both the "24h" and
/// "custom date/time" choices (they're the same case here — see
/// `Retention::After`'s own doc comment) and sends it as `after_rfc3339`.
// `rename_all = "camelCase"` (not `snake_case`) so this behaves the same as
// every other IPC-facing type in this file regardless of exactly how far
// Tauri's own camelCase<->snake_case argument conversion reaches - explicit
// here rather than relying on it, since this struct is new and untested
// against a real invoke() call.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RetentionChoiceDto {
    DeleteAfterDownload,
    After { after_rfc3339: String },
}

impl RetentionChoiceDto {
    fn into_retention(self) -> Result<ls_clouddrop::retention::Retention, String> {
        match self {
            RetentionChoiceDto::DeleteAfterDownload => Ok(ls_clouddrop::retention::Retention::DeleteAfterDownload),
            RetentionChoiceDto::After { after_rfc3339 } => {
                let at = time::OffsetDateTime::parse(&after_rfc3339, &time::format_description::well_known::Rfc3339)
                    .map_err(|e| format!("invalid retention date {after_rfc3339:?}: {e}"))?;
                Ok(ls_clouddrop::retention::Retention::After(at))
            }
        }
    }
}

#[derive(Clone, Serialize)]
pub struct CloudDropSessionInfo {
    pub room_code: String,
    pub room_id: String,
    pub signaling_url: String,
    pub code_expires_in_seconds: u64,
    pub file_id: String,
}

/// The relay room label `start_cloud_drop_session`'s own `connect_as_sender`
/// call must use: `room_id`, never `room_code`.
///
/// Pulled out into its own tiny, pure function specifically so it's testable
/// without a real Google OAuth config or a live Drive upload (both of which
/// `start_cloud_drop_session` itself requires, and neither of which exist in
/// this sandbox - see `cloud_drop_protocol_test.rs`'s own doc comment) —
/// see `cloud_drop_local_mode_room_id_test.rs`, which imports this exact
/// function to prove (and guard against regressing) the real bug found in
/// real Local-network-mode testing: this used to be inlined as
/// `send_info.room_code`, which is only ever correct by coincidence in
/// "remote" mode (where `room_code` IS the bare `room_id` -
/// `start_send_session`'s own doc comment) and silently wrong in "local"
/// mode, where `room_code` is the 14-character IP+port+id-*encoded* string a
/// human pastes, not the 4-character `room_id` a real receiver's
/// `decode_room_code` extracts and actually connects with
/// (`request_cloud_drop_access` below). A sender connected under the wrong,
/// longer string joins a relay room no real receiver could ever reach -
/// `connect_as_sender` then sits waiting for a second peer that never
/// arrives, until `ls_net::CONNECT_TIMEOUT` (300s) elapses - exactly the
/// reported "room code expires, receiver never connects" symptom.
pub fn cloud_drop_sender_room(send_info: &SendSessionInfo) -> &str {
    &send_info.room_id
}

/// Sender side: bundles `project_path` exactly like `share_snapshot` does,
/// uploads it to the sender's own Drive (never grants anyone access yet),
/// then hosts the same kind of signaling room `start_send_session` does —
/// not to carry the payload (Drive already has it), only so a receiver who
/// pastes `room_code` can reach this sender's control channel to ask for
/// access. Blocks until that receiver connects, same as `share_snapshot`'s
/// `connect_as_sender` call.
#[tauri::command]
pub async fn start_cloud_drop_session<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    mode: String,
    relay_url: Option<String>,
    project_path: String,
    retention: RetentionChoiceDto,
) -> Result<CloudDropSessionInfo, String> {
    let retention = retention.into_retention()?;
    let config = cloud_drop_config()?;
    let access_token = ls_clouddrop::oauth::ensure_valid_access_token(&config).await.map_err(|e| e.to_string())?;

    log::info!("start_cloud_drop_session: bundling project_path={project_path}");
    let root = PathBuf::from(&project_path);
    let snapshot = tauri::async_runtime::spawn_blocking(move || ls_snapshot::create_snapshot(&root, None))
        .await
        .map_err(|e| format!("snapshot task panicked: {e}"))?
        .map_err(|e| e.to_string())?;
    let filename = format!("{}@{}.localsync-snapshot", snapshot.manifest.project_name, snapshot.manifest.git_commit);
    let bytes = serde_json::to_vec(&snapshot).map_err(|e| e.to_string())?;

    log::info!("start_cloud_drop_session: uploading {filename} ({} bytes) to Drive", bytes.len());
    let folder_id = ls_clouddrop::drive::get_or_create_app_folder(&access_token).await.map_err(|e| e.to_string())?;
    let uploaded = ls_clouddrop::drive::upload_file(&access_token, &folder_id, &filename, &bytes)
        .await
        .map_err(|e| e.to_string())?;

    ls_clouddrop::retention::add_tracked_upload(ls_clouddrop::retention::TrackedUpload {
        file_id: uploaded.file_id.clone(),
        retention,
        downloaded: false,
        uploaded_at: time::OffsetDateTime::now_utc(),
    })
    .map_err(|e| e.to_string())?;

    let send_info = start_send_session(mode, relay_url).await?;
    // See `cloud_drop_sender_room`'s own doc comment for the real,
    // Local-network-mode-specific bug this guards against.
    let room = cloud_drop_sender_room(&send_info);
    log::info!("start_cloud_drop_session: waiting for a receiver on room={room}");
    let conn = ls_net::connect_as_sender(&send_info.signaling_url, room)
        .await
        .map_err(|e| e.to_string())?;

    // Keyed by the same `room` value the connect call above actually used
    // (not `send_info.room_code`), for the same reason, and to match the
    // convention every other transport in this file already uses
    // (`share_snapshot`/`share_snapshot_wizard`'s `connected_receivers`,
    // keyed by the value actually used to pair on the relay, not the
    // user-facing display code).
    let room = room.to_string();
    state.cloud_drop_uploads.lock().map_err(|e| e.to_string())?.insert(
        room.clone(),
        crate::state::CloudDropUpload { conn: std::sync::Arc::new(conn), file_id: uploaded.file_id.clone(), retention },
    );
    tauri::async_runtime::spawn(listen_for_cloud_access_requests(app, room));

    Ok(CloudDropSessionInfo {
        room_code: send_info.room_code,
        room_id: send_info.room_id,
        signaling_url: send_info.signaling_url,
        code_expires_in_seconds: send_info.code_expires_in_seconds,
        file_id: uploaded.file_id,
    })
}

#[derive(Clone, Serialize)]
pub struct CloudAccessRequestNotice {
    pub peer_id: String,
    pub google_email: String,
}

/// Background task (one per Cloud-drop upload, mirrors `listen_for_pull_requests`):
/// waits for the receiver's `CloudAccessRequest` and surfaces it to the
/// frontend as a `cloud-access-request` event. The email is stashed in
/// `state.cloud_access_requests` — it has nowhere else to live between now
/// and `respond_to_cloud_access_request` actually needing it to grant access.
async fn listen_for_cloud_access_requests<R: tauri::Runtime>(app: AppHandle<R>, peer_id: String) {
    loop {
        let conn = {
            let state = app.state::<AppState>();
            let Ok(uploads) = state.cloud_drop_uploads.lock() else { return };
            let Some(entry) = uploads.get(&peer_id) else { return };
            entry.conn.clone()
        };

        match ls_net::recv_control(&conn).await {
            Ok(ls_net::ControlMessage::CloudAccessRequest { google_email }) => {
                log::info!("start_cloud_drop_session: access request from peer_id={peer_id} email={google_email}");
                let state = app.state::<AppState>();
                if let Ok(mut pending) = state.cloud_access_requests.lock() {
                    pending.insert(peer_id.clone(), google_email.clone());
                }
                let _ = app.emit("cloud-access-request", CloudAccessRequestNotice { peer_id: peer_id.clone(), google_email });
            }
            Ok(ls_net::ControlMessage::CloudDownloadConfirmed) => {
                // Flips `DeleteAfterDownload` uploads due (see
                // `ls_clouddrop::retention::due_for_deletion`) - actual
                // deletion still only happens via `cleanup_expired_cloud_drops`
                // at next startup, not from inside this listener.
                let state = app.state::<AppState>();
                let file_id = state.cloud_drop_uploads.lock().ok().and_then(|u| u.get(&peer_id).map(|e| e.file_id.clone()));
                if let Some(file_id) = file_id {
                    log::info!("start_cloud_drop_session: receiver confirmed download of {file_id}");
                    if let Err(e) = ls_clouddrop::retention::mark_downloaded(&file_id) {
                        log::warn!("start_cloud_drop_session: could not mark {file_id} downloaded: {e:#}");
                    }
                }
            }
            Ok(other) => {
                log::warn!("start_cloud_drop_session: unexpected control message from receiver: {other:?}");
            }
            Err(e) => {
                log::info!("start_cloud_drop_session: control channel for peer_id={peer_id} ended: {e:#}");
                return;
            }
        }
    }
}

/// The sender's response to a `cloud-access-request` event. Accepting is the
/// only path in this whole round that calls `grant_reader_access` — a real,
/// targeted (`type: "user"`, one specific email) Drive permission grant, not
/// a promise. Declining sends `CloudAccessResponse { accepted: false, .. }`
/// and grants nothing at all.
#[tauri::command]
pub async fn respond_to_cloud_access_request(
    state: State<'_, AppState>,
    peer_id: String,
    accept: bool,
) -> Result<(), String> {
    let google_email = state
        .cloud_access_requests
        .lock()
        .map_err(|e| e.to_string())?
        .remove(&peer_id)
        .ok_or_else(|| format!("no pending cloud-access request from peer_id={peer_id}"))?;
    let (conn, file_id, retention) = {
        let uploads = state.cloud_drop_uploads.lock().map_err(|e| e.to_string())?;
        let entry = uploads
            .get(&peer_id)
            .ok_or_else(|| format!("no cloud-drop upload session for peer_id={peer_id}"))?;
        (entry.conn.clone(), entry.file_id.clone(), entry.retention)
    };

    if !accept {
        log::info!("respond_to_cloud_access_request: declined for peer_id={peer_id}");
        return ls_net::send_control(&conn, &ls_net::ControlMessage::CloudAccessResponse { accepted: false, drive_file_id: None })
            .await
            .map_err(|e| e.to_string());
    }

    let config = cloud_drop_config()?;
    let access_token = ls_clouddrop::oauth::ensure_valid_access_token(&config).await.map_err(|e| e.to_string())?;
    // Only `After(t)` carries an instant Drive's own expirationTime can be
    // asked to enforce - DeleteAfterDownload has no time cap of its own
    // (see ls_clouddrop::retention::Retention's doc comment), so nothing is
    // requested for it; the app-level retention cleanup is what enforces
    // that case regardless.
    let expiration = match retention {
        ls_clouddrop::retention::Retention::After(t) => Some(t),
        ls_clouddrop::retention::Retention::DeleteAfterDownload => None,
    };
    let grant = ls_clouddrop::drive::grant_reader_access(&access_token, &file_id, &google_email, expiration)
        .await
        .map_err(|e| e.to_string())?;
    log::info!(
        "respond_to_cloud_access_request: granted {google_email} access to {file_id} (expiration_applied={})",
        grant.expiration_applied
    );

    ls_net::send_control(
        &conn,
        &ls_net::ControlMessage::CloudAccessResponse { accepted: true, drive_file_id: Some(file_id) },
    )
    .await
    .map_err(|e| e.to_string())
}

#[derive(Clone, Serialize)]
pub struct CloudDropReceiveOutcome {
    pub accepted: bool,
    /// `Some` exactly when `accepted` is `true` — held, not auto-run, via
    /// the exact same `finalize_received_snapshot` every other transport
    /// uses.
    pub info: Option<IncomingSnapshotInfo>,
}

/// Receiver side: decodes `code` (via the existing, unmodified
/// `decode_room_code` - round 25: no longer takes a `mode` either, see its
/// own doc comment), connects to the sender's control channel, announces
/// this receiver's own linked Google account, and waits for the sender's
/// Accept/Reject. On accept, downloads straight from Drive and runs the
/// result through the same review/consent/Run pipeline every other
/// transport already uses.
#[tauri::command]
pub async fn request_cloud_drop_access(
    state: State<'_, AppState>,
    code: String,
    relay_url: Option<String>,
) -> Result<CloudDropReceiveOutcome, String> {
    let config = cloud_drop_config()?;
    let tokens = ls_clouddrop::store::load_tokens()
        .map_err(|e| e.to_string())?
        .ok_or("not linked to a Google account yet - link one in Settings first")?;
    let access_token = ls_clouddrop::oauth::ensure_valid_access_token(&config).await.map_err(|e| e.to_string())?;

    let decoded = decode_room_code(code, relay_url)?;
    log::info!("request_cloud_drop_access: connecting for room={}", decoded.room_id);
    let conn = ls_net::connect_as_receiver(&decoded.signaling_url, &decoded.room_id)
        .await
        .map_err(|e| e.to_string())?;

    ls_net::send_control(&conn, &ls_net::ControlMessage::CloudAccessRequest { google_email: tokens.email.clone() })
        .await
        .map_err(|e| e.to_string())?;

    match ls_net::recv_control(&conn).await.map_err(|e| e.to_string())? {
        ls_net::ControlMessage::CloudAccessResponse { accepted: false, .. } => {
            log::info!("request_cloud_drop_access: sender declined");
            Ok(CloudDropReceiveOutcome { accepted: false, info: None })
        }
        ls_net::ControlMessage::CloudAccessResponse { accepted: true, drive_file_id: Some(file_id) } => {
            log::info!("request_cloud_drop_access: accepted, downloading file_id={file_id}");
            let bytes = ls_clouddrop::drive::download_file(&access_token, &file_id).await.map_err(|e| e.to_string())?;
            let snapshot: ls_snapshot::Snapshot = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            let info = finalize_received_snapshot(&state, snapshot)?;
            ls_net::send_control(&conn, &ls_net::ControlMessage::CloudDownloadConfirmed)
                .await
                .map_err(|e| e.to_string())?;
            log::info!("request_cloud_drop_access: done, id={}", info.snapshot_id);
            Ok(CloudDropReceiveOutcome { accepted: true, info: Some(info) })
        }
        other => Err(format!("unexpected response from sender: {other:?}")),
    }
}

/// Deletes every Drive upload whose retention has come due, regardless of
/// whether it was ever downloaded (see `ls_clouddrop::retention`'s doc
/// comment: `After(t)` is a hard cap, not merely "until downloaded"). Called
/// once at app startup (see `main.rs`) rather than on a timer, matching
/// `ls_containers::ProvisioningLog`'s "check when convenient" convention —
/// this app has no background scheduler and round 23 doesn't need to add
/// one. Silently does nothing if no Google account is linked yet (nothing
/// to clean up) or if the stored token can't be refreshed (logged, not
/// fatal — cleanup just retries next launch).
pub async fn cleanup_expired_cloud_drops() {
    let Ok(Some(_)) = ls_clouddrop::store::load_tokens() else { return };
    let config = match ls_clouddrop::oauth::OAuthConfig::from_env() {
        Ok(c) => c,
        Err(_) => return,
    };
    let access_token = match ls_clouddrop::oauth::ensure_valid_access_token(&config).await {
        Ok(t) => t,
        Err(e) => {
            log::warn!("cleanup_expired_cloud_drops: could not refresh access token: {e:#}");
            return;
        }
    };
    let uploads = match ls_clouddrop::retention::load_tracked_uploads() {
        Ok(u) => u,
        Err(e) => {
            log::warn!("cleanup_expired_cloud_drops: could not load tracked uploads: {e:#}");
            return;
        }
    };
    let due: Vec<String> = ls_clouddrop::retention::due_for_deletion(&uploads, time::OffsetDateTime::now_utc())
        .into_iter()
        .map(|u| u.file_id.clone())
        .collect();
    for file_id in due {
        match ls_clouddrop::drive::delete_file(&access_token, &file_id).await {
            Ok(()) => {
                log::info!("cleanup_expired_cloud_drops: deleted {file_id}");
                if let Err(e) = ls_clouddrop::retention::remove_tracked_upload(&file_id) {
                    log::warn!("cleanup_expired_cloud_drops: deleted {file_id} from Drive but could not untrack it: {e:#}");
                }
            }
            Err(e) => log::warn!("cleanup_expired_cloud_drops: failed to delete {file_id}: {e:#}"),
        }
    }
}

// ---------- round 29 goal B2: session history ----------
//
// The frontend's own `sessions` map (app.js) already tracks everything
// worth recording - kind, title, when it started/ended - for the tab UI
// this round adds. These two commands are purely storage: read the whole
// list back, or upsert one entry by id. See `session_history`'s own doc
// comment for why upsert (not append-only).

#[tauri::command]
pub fn load_session_history() -> Result<Vec<crate::session_history::SessionHistoryEntry>, String> {
    crate::session_history::load()
}

#[tauri::command]
pub fn record_session_history_entry(entry: crate::session_history::SessionHistoryEntry) -> Result<(), String> {
    crate::session_history::upsert(entry)
}
