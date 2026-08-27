// Desktop-only app (no mobile target), so main.rs owns the whole entry
// point rather than splitting into a lib.rs + mobile_entry_point — that
// split exists in the default Tauri template to support `cargo tauri ios/
// android`, which this project doesn't use.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod state;

use state::AppState;
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

        state
            .verified
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .insert(snapshot_id.clone(), verified);

        Ok(commands::IncomingSnapshotInfo { snapshot_id, manifest, diff })
    };

    match load() {
        Ok(info) => Some(info),
        Err(e) => {
            eprintln!("LOCALSYNC_PRELOAD_SNAPSHOT={path:?}: failed to load/verify ({e:#}) — starting without a preload.");
            None
        }
    }
}

fn main() {
    isolate_data_dir_if_set();

    let state = AppState::default();
    let preload_info = preload_snapshot(&state);

    tauri::Builder::default()
        .manage(state)
        .setup(move |app| {
            if let Some(info) = preload_info {
                let handle = app.handle().clone();
                // ponytail: fixed delay to dodge the startup race against the
                // frontend's listen() call (setup() can run before app.js has
                // registered its listener). Upgrade path if this proves
                // flaky in practice: have the frontend ack readiness via an
                // invoke command instead of guessing a delay.
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    let _ = handle.emit("preload-review", info);
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::share_snapshot,
            commands::receive_snapshot,
            commands::run_snapshot,
            commands::stop_session,
        ])
        .run(tauri::generate_context!())
        .expect("error while running LocalSync");
}
