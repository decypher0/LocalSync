//! `compat::check_update_compat` against a real project and (where present)
//! real Podman: after a change to a build file, a version whose app no
//! longer starts is reported not compatible, a fixed one compatible, and a
//! version that already passed isn't booted again.
//!
//! The Podman part skips with a printed reason where podman/podman-compose
//! are absent. Run in WSL/Linux.

use std::path::{Path, PathBuf};
use std::process::Command;

use localsync_desktop::commands::FolderPlanDto;
use localsync_desktop::compat::{self, CompatReport};
use localsync_desktop::project_session::{DeviceMarker, FolderCommit, ProjectSession};
use localsync_desktop::state::AppState;
use ls_composegen::{BuildTool, ComposeSpec, DbEnvPreset, Runtime};
use tauri::Manager;

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed");
}

const GOOD_APP: &str = "import http.server\nprint('compat app up', flush=True)\nhttp.server.ThreadingHTTPServer(('0.0.0.0', PORT), http.server.SimpleHTTPRequestHandler).serve_forever()\n";

/// A committed Python project, and a session whose device already received
/// that commit.
fn project(name: &str, port: u16) -> (tempfile::TempDir, PathBuf, ProjectSession) {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join(name);
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("app.py"), GOOD_APP.replace("PORT", &port.to_string())).unwrap();
    std::fs::write(dir.join("requirements.txt"), "# no dependencies\n").unwrap();
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "initial"]);

    let spec = ComposeSpec {
        runtime: Runtime::Python,
        runtime_version: "3.12".into(),
        build_tool: BuildTool::Pip,
        run_command: "python app.py".into(),
        port,
        artifact_path: None,
        java_packaging: Default::default(),
        tomcat_version: None,
        database: None,
        db_env_preset: DbEnvPreset::Standard,
        extras: vec![],
        env: vec![],
    };
    let path = dir.display().to_string();
    let folder = FolderPlanDto { path: path.clone(), dump: None, compose: Some(spec), externals: vec![] };
    let mut session = ProjectSession::new(format!("s-{name}"), name.into(), vec![folder], "2026-01-01T00:00:00Z".into());
    let commit = ls_snapshot::head_commit(&dir).unwrap();
    let tree = ls_snapshot::folder_tree(&dir, &commit).ok();
    session.record_send(
        "dev-a",
        "Alice",
        DeviceMarker { snapshot_id: format!("{name}@{commit}"), commits: vec![FolderCommit { path, commit, dump: None, tree, externals: Default::default() }], sent_at: "2026-01-01T00:00:00Z".into() },
    );
    (base, dir, session)
}

async fn check(session: &ProjectSession) -> CompatReport {
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    let handle = app.handle().clone();
    handle.state::<AppState>().project_sessions.lock().unwrap().insert(session.id.clone(), session.clone());
    compat::check_update_compat(handle.clone(), handle.state::<AppState>(), session.id.clone()).await.unwrap()
}

#[tokio::test]
async fn a_change_to_app_code_only_runs_the_quick_checks() {
    let (_base, dir, session) = project("compat-quick", 18120);
    std::fs::write(dir.join("app.py"), "print('edited')\n").unwrap();
    let r = check(&session).await;
    assert!(r.ok, "{r:?}");
    assert!(r.test.is_none(), "no build/config file changed: no test run: {r:?}");
    assert!(r.problems.is_empty() && r.notes.is_empty(), "{r:?}");
}

#[tokio::test]
async fn a_broken_build_is_not_compatible_and_a_fixed_one_is() {
    let ok = |bin: &str| Command::new(bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    if !(ok("podman") && ok("podman-compose")) {
        eprintln!("podman/podman-compose not on PATH - skipping (this test boots real containers)");
        return;
    }
    let port = 18122;
    let (_base, dir, session) = project("compat-py", port);

    // A build-file change, plus a syntax error in the app.
    std::fs::write(dir.join("requirements.txt"), "# no dependencies\n# touched\n").unwrap();
    std::fs::write(dir.join("app.py"), "def broken(:\n").unwrap();
    let bad = check(&session).await;
    eprintln!("--- broken ---\n{bad:#?}");
    assert!(!bad.ok, "{bad:?}");
    let t = bad.test.as_ref().expect("a build file changed: the test must run");
    assert!(!t.ok && !t.cached);
    let all = format!("{:?}\n{}", t.error, t.output_tail);
    assert!(all.contains("SyntaxError"), "the real reason should be in the report: {all}");
    assert_eq!(bad.problems.len(), 1, "{:?}", bad.problems);

    std::fs::write(dir.join("app.py"), GOOD_APP.replace("PORT", &port.to_string())).unwrap();
    let good = check(&session).await;
    eprintln!("--- fixed ---\n{good:#?}");
    assert!(good.ok, "{good:?}");
    let t = good.test.as_ref().unwrap();
    assert!(t.ok && !t.cached, "{t:?}");
    assert_ne!(good.tree_key, bad.tree_key);

    // The same version again: the pass for this tree counts, no second boot.
    let again = check(&session).await;
    assert!(again.ok && again.test.as_ref().unwrap().cached, "{again:?}");
}
