//! The setup commands end to end with the REAL per-OS checks (no fakes): on a
//! machine where Podman works, `setup_verify` must check every step for real,
//! record them all as done, and hand the UI exactly the JSON shape it reads.
//! Runs on Linux (WSL/CI) where the data dir can be redirected with
//! XDG_DATA_HOME; skips where Podman is absent.

use std::process::Command;

use localsync_desktop::setup_commands;
use tauri::Listener;

#[tokio::test]
async fn real_setup_verify_on_a_working_podman_marks_every_step_done_and_matches_the_ui_contract() {
    let ok = |bin: &str| Command::new(bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    if !cfg!(target_os = "linux") || !(ok("podman") && ok("podman-compose")) {
        eprintln!("skipping: needs Linux with podman + podman-compose");
        return;
    }
    let data = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_DATA_HOME", data.path());

    let before = serde_json::to_value(setup_commands::setup_state().unwrap()).unwrap();
    assert_eq!(before["any_progress"], false, "fresh data dir = no progress: {before}");

    let app = tauri::test::mock_app();
    let handle = app.handle().clone();
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let sink = events.clone();
    handle.listen("setup-step", move |e| sink.lock().unwrap().push(serde_json::from_str(e.payload()).unwrap()));

    let view = serde_json::to_value(setup_commands::setup_verify(handle).await.unwrap()).unwrap();

    // The exact keys the UI (apps/desktop/src/setup-wizard.js) reads.
    for key in ["os", "steps", "any_progress", "all_done", "restart_required", "first_undone", "failure"] {
        assert!(view.get(key).is_some(), "missing {key}: {view}");
    }
    assert_eq!(view["os"], "linux");
    let steps: Vec<&str> = view["steps"].as_array().unwrap().iter().map(|s| s["step"].as_str().unwrap()).collect();
    assert_eq!(steps, ["podman_installed", "functional_check"], "Linux has no WSL/restart/machine steps");
    for s in view["steps"].as_array().unwrap() {
        for key in ["title", "status", "consent", "needs_admin", "manual_instructions"] {
            assert!(s.get(key).is_some(), "step missing {key}: {s}");
        }
        assert_eq!(s["status"], "done", "{s}");
    }
    assert_eq!(view["all_done"], true, "{view}");
    assert!(view["failure"].is_null() && view["first_undone"].is_null(), "{view}");

    let events = events.lock().unwrap().clone();
    let seq: Vec<String> = events.iter().map(|e| format!("{}:{}", e["step"].as_str().unwrap(), e["status"].as_str().unwrap())).collect();
    assert_eq!(
        seq,
        ["podman_installed:checking", "podman_installed:done", "functional_check:checking", "functional_check:done"],
        "live checklist events, in order: {events:?}"
    );

    // Persisted: a relaunch reads it back without checking anything.
    let after = serde_json::to_value(setup_commands::setup_state().unwrap()).unwrap();
    assert_eq!(after["all_done"], true, "{after}");
    assert!(data.path().join("localsync/setup-state.json").is_file());
}
