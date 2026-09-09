// Desktop-only app (no mobile target), so main.rs owns the whole entry
// point rather than splitting into a lib.rs + mobile_entry_point — that
// split exists in the default Tauri template to support `cargo tauri ios/
// android`, which this project doesn't use.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use localsync_desktop::{commands, send_log, state::AppState};
use tauri::Emitter;

/// Two demo instances (sender + receiver) commonly run on the same Linux
/// box at once. They don't collide on `~/.localsync/identity.key` (only the
/// sender ever loads it, and `ls_snapshot::sign` already handles concurrent
/// first-run creation safely) — but without this, both webviews would share
/// one WebKitGTK profile dir (cookies/local-storage/cache) under the same
/// XDG paths, which can wedge on file locks. Point each instance's whole
/// XDG_* tree at its own subdirectory when `LOCALSYNC_DATA_DIR` is set, so
/// the two processes never touch the same files. Must run before
/// `tauri::Builder` does anything, since WebKitGTK reads these once at init.
fn isolate_data_dir_if_set() {
    if let Ok(dir) = std::env::var("LOCALSYNC_DATA_DIR") {
        std::env::set_var("XDG_DATA_HOME", format!("{dir}/data"));
        std::env::set_var("XDG_CONFIG_HOME", format!("{dir}/config"));
        std::env::set_var("XDG_CACHE_HOME", format!("{dir}/cache"));
    }
}

/// Dev shortcut for reaching the "review this diff before you run it" screen
/// without a live P2P receive: if `LOCALSYNC_PRELOAD_SNAPSHOT` names a
/// snapshot file, load + verify it exactly the way `commands::receive_snapshot`
/// does, stash the `VerifiedSnapshot` in `AppState` under the same
/// `"<project_name>@<git_commit>"` key (so `run_snapshot` also works against
/// it unmodified), and return the same `IncomingSnapshotInfo` shape the
/// frontend's `renderReview` already knows how to draw.
///
/// Returns `None` if the env var is unset (app behaves exactly as today) or
/// if loading/verifying fails — a bad preload path is logged to stderr, not
/// a reason to stop the whole app from starting.
fn preload_snapshot(state: &AppState) -> Option<commands::IncomingSnapshotInfo> {
    let path = std::env::var("LOCALSYNC_PRELOAD_SNAPSHOT").ok()?;

    let load = || -> anyhow::Result<commands::IncomingSnapshotInfo> {
        let snapshot = ls_snapshot::load_from_file(std::path::Path::new(&path))?;
        // Same TOFU behavior receive_snapshot uses (see ls_security::verify's
        // doc comment) — this is a local preload, not a network receive, but
        // the review screen it feeds must be provably the real one.
        let verified = ls_security::verify(snapshot, &[])?;
        let diff = ls_security::diff_summary(&verified)?;
        let manifest = verified.snapshot().manifest.clone();
        let snapshot_id = format!("{}@{}", manifest.project_name, manifest.git_commit);
        let sender_pubkey_hex = manifest.sender_pubkey.iter().map(|b| format!("{b:02x}")).collect();

        // Same non-fatal, informational-only lookup commands::receive_snapshot
        // does — see its doc comment.
        let recognized_peer = match ls_security::KnownPeers::load_default() {
            Ok(peers) => peers.find(&manifest.sender_pubkey).map(|p| commands::RecognizedPeer {
                name: p.name.clone(),
                first_seen: p
                    .first_seen
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_else(|_| "unknown".to_string()),
            }),
            Err(e) => {
                eprintln!("preload_snapshot: could not load known peers ({e}) — treating as no known peers");
                None
            }
        };

        state
            .verified
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .insert(snapshot_id.clone(), verified);

        Ok(commands::IncomingSnapshotInfo { snapshot_id, manifest, diff, sender_pubkey_hex, recognized_peer })
    };

    match load() {
        Ok(info) => Some(info),
        Err(e) => {
            eprintln!("LOCALSYNC_PRELOAD_SNAPSHOT={path:?}: failed to load/verify ({e:#}) — starting without a preload.");
            None
        }
    }
}

/// Real friction this covers: on Linux, an active `ufw` firewall silently
/// drops the incoming P2P connection a Receive needs, with nothing in the
/// app to explain why it hung. Checks `ufw status` (most machines don't
/// have `ufw` installed at all - that's not a problem, just skip silently
/// via `.ok()?`) and returns a user-facing message if it's active. No fixed
/// port number is named since the signaling/WebRTC ports are dynamic.
fn ufw_warning() -> Option<String> {
    let output = std::process::Command::new("ufw").arg("status").output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.contains("Status: active") {
        Some(
            "ufw (firewall) is active on this machine. It may block incoming P2P \
             connections - if Send/Receive hangs, you may need to allow LocalSync \
             through ufw for the port it uses when you click Send."
                .to_string(),
        )
    } else {
        None
    }
}

fn main() {
    // As early as possible, so every later stage (bundling, signing,
    // signaling connect, transfer, ...) is captured from the start.
    send_log::install_default();
    isolate_data_dir_if_set();

    let state = AppState::default();
    let preload_info = preload_snapshot(&state);

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // Round 16: real auto-update. Config (pubkey, endpoints) lives in
        // tauri.conf.json - this just registers the plugin's commands
        // (check/download/install) for app.js to call. Never installs
        // anything without the user confirming first - see app.js's
        // update-check listener; nothing here auto-applies an update.
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Only used for its relaunch() command, called after a
        // user-confirmed update finishes installing.
        .plugin(tauri_plugin_process::init())
        .manage(state)
        .setup(move |app| {
            let firewall_msg = ufw_warning();
            if let Some(msg) = &firewall_msg {
                log::warn!("startup: {msg}");
            }
            if firewall_msg.is_some() || preload_info.is_some() {
                let handle = app.handle().clone();
                // ponytail: fixed delay to dodge the startup race against the
                // frontend's listen() call (setup() can run before app.js has
                // registered its listener). Upgrade path if this proves
                // flaky in practice: have the frontend ack readiness via an
                // invoke command instead of guessing a delay.
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    if let Some(msg) = firewall_msg {
                        let _ = handle.emit("firewall-warning", msg);
                    }
                    if let Some(info) = preload_info {
                        let _ = handle.emit("preload-review", info);
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::start_send_session,
            commands::decode_room_code,
            commands::share_snapshot,
            commands::receive_snapshot,
            commands::run_snapshot,
            commands::stop_session,
            commands::remember_peer,
            commands::reject_snapshot,
            commands::list_connected_receivers,
            commands::push_update,
            commands::respond_to_pull_request,
            commands::send_pull_request,
        ])
        .run(tauri::generate_context!())
        .expect("error while running LocalSync");
}
