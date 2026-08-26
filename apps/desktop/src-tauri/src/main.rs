// Desktop-only app (no mobile target), so main.rs owns the whole entry
// point rather than splitting into a lib.rs + mobile_entry_point — that
// split exists in the default Tauri template to support `cargo tauri ios/
// android`, which this project doesn't use.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod state;

use state::AppState;

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

fn main() {
    isolate_data_dir_if_set();

    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::share_snapshot,
            commands::receive_snapshot,
            commands::run_snapshot,
            commands::stop_session,
        ])
        .run(tauri::generate_context!())
        .expect("error while running LocalSync");
}
