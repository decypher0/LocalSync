//! Exercises the logic behind main.rs's `LOCALSYNC_PRELOAD_SNAPSHOT` preload
//! path (the same calls `preload_snapshot` in `src/main.rs` makes:
//! `create_snapshot` + `save_to_file` (what the `make_snapshot` example
//! does), then `load_from_file` + `ls_security::verify` + `diff_summary`)
//! without a Tauri window or display — same spirit as `pipeline_test.rs`'s
//! exercise of the `receive_snapshot` path, just for the preload shortcut
//! instead. This is the proof that the data reaching the frontend via the
//! "preload-review" event (the `IncomingSnapshotInfo` shape) is correct;
//! actually looking at the resulting screen is a manual, separate step.

use std::path::Path;
use std::process::Command;

fn sh(dir: &Path, cmd: &str, args: &[&str]) {
    let status = Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {cmd} {args:?}: {e}"));
    assert!(status.success(), "{cmd} {args:?} failed in {}", dir.display());
}

/// `sample-project/` isn't its own git repo (it's tracked as part of the
/// LocalSync monorepo), and `create_snapshot` needs `project_dir` to be a
/// repo root of its own — copy it into a fresh tempdir and git-init it
/// there, same requirement `pipeline_test.rs`'s `make_git_project_from`
/// documents (and what `scripts/demo-review-screen.sh` does for real).
fn make_git_project_from(source: &Path) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let project_dir = dir.path().join("sample-project");
    std::fs::create_dir(&project_dir).unwrap();
    sh(
        Path::new("."),
        "cp",
        &["-r", &format!("{}/.", source.display()), &project_dir.display().to_string()],
    );
    sh(&project_dir, "git", &["init", "-q"]);
    sh(&project_dir, "git", &["add", "-A"]);
    sh(
        &project_dir,
        "git",
        &[
            "-c", "user.name=Test", "-c", "user.email=test@example.com",
            "commit", "-q", "-m", "preload snapshot",
        ],
    );
    (dir, project_dir)
}

#[test]
fn preload_path_produces_a_correct_review() {
    let sample_project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../sample-project");
    assert!(sample_project.is_dir(), "expected {} to exist", sample_project.display());

    // Isolated identity.key, same reasoning as pipeline_test.rs: HOME is
    // process-global and this file has exactly one test, so it's safe here.
    let home = tempfile::tempdir().unwrap();
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::set_var(home_var, home.path());

    let (tempdir, project_dir) = make_git_project_from(&sample_project);

    // --- what the make_snapshot example does: create_snapshot + save_to_file ---
    let snapshot = ls_snapshot::create_snapshot(&project_dir, None)
        .expect("create_snapshot should succeed against a git-initialized sample-project copy");
    let snapshot_path = tempdir.path().join("preload-snapshot.json");
    ls_snapshot::save_to_file(&snapshot, &snapshot_path).expect("save_to_file should write the snapshot");

    // --- what main.rs's preload_snapshot does: load_from_file + verify + diff_summary ---
    let loaded = ls_snapshot::load_from_file(&snapshot_path).expect("load_from_file should read the snapshot back");
    let verified = ls_security::verify(loaded, &[]).expect("TOFU verify should accept a freshly-signed snapshot");
    let diff = ls_security::diff_summary(&verified).expect("diff_summary should read diff_stat.json from the payload");
    let manifest = verified.snapshot().manifest.clone();
    let snapshot_id = format!("{}@{}", manifest.project_name, manifest.git_commit);

    // --- assert the IncomingSnapshotInfo-equivalent data is non-empty and correct ---
    assert_eq!(manifest.project_name, "sample-project");
    assert_eq!(snapshot_id, format!("sample-project@{}", manifest.git_commit));
    assert!(!manifest.git_commit.is_empty());
    assert!(manifest.git_parent_commit.is_none(), "no parent was passed to create_snapshot");
    assert!(!manifest.services.is_empty(), "sample-project's docker-compose.yml should yield services");
    assert!(manifest.services.iter().any(|s| s.name == "app"));

    assert!(!diff.entries.is_empty(), "an initial snapshot should report every file as added");
    assert!(diff.entries.iter().any(|e| e.path == "docker-compose.yml"));
    assert!(diff.entries.iter().all(|e| e.change_type == "added"));
    assert_eq!(diff.total_insertions, diff.entries.iter().map(|e| e.insertions).sum::<u32>());
    assert!(diff.total_insertions > 0);
}
