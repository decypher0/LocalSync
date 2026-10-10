//! "Set up automatically" on the folder step: puts a folder that isn't in git
//! yet (a GitHub "Download ZIP" folder, say) into git with one commit, since
//! every snapshot is built from a commit. Only adds a `.git` folder: the
//! person's own files are never written, so excludes go to `.git/info/exclude`
//! rather than a new `.gitignore`.

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

/// `git add -A` on a big folder writes every file into git's object store:
/// far slower than the read-only calls ls-snapshot's 30s is sized for.
const INIT_TIMEOUT: Duration = Duration::from_secs(300);

/// Build output, dependencies and editor folders - only used when the folder
/// has no `.gitignore` of its own.
const EXCLUDES: &[&str] = &["node_modules/", "target/", "build/", "dist/", ".venv/", "venv/", "__pycache__/", ".idea/", ".vscode/", "*.log"];

pub const READ_ONLY: &str =
    "LocalSync can't write to this folder, so it can't set up git here. Copy the folder somewhere you can edit (such as Documents) and add the copy instead.";

#[tauri::command]
pub async fn init_project_repo(folder_path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || init_repo(Path::new(&folder_path)))
        .await
        .map_err(|e| e.to_string())?
}

/// `git init` (unless the folder is already in a repo), `git add -A`, and one
/// commit under a fixed identity, so it works with no git identity configured.
pub fn init_repo(folder: &Path) -> Result<(), String> {
    let git = |args: &[&str]| -> Result<String, String> {
        ls_snapshot::run_git(folder, args, &[("GIT_TERMINAL_PROMPT", OsStr::new("0"))], INIT_TIMEOUT)
            .map(|out| String::from_utf8_lossy(&out).trim().to_string())
            .map_err(|e| plain_error(&format!("{e:#}")))
    };
    if !folder.is_dir() {
        return Err(format!("{} isn't a folder.", folder.display()));
    }
    // Same test inspect() uses for "is a repo".
    if !folder.ancestors().any(|d| d.join(".git").exists()) {
        git(&["init", "-q"])?;
    }
    if !folder.join(".gitignore").exists() {
        let exclude = git(&["rev-parse", "--path-format=absolute", "--git-path", "info/exclude"])?;
        let exclude = Path::new(&exclude);
        let existing = std::fs::read_to_string(exclude).unwrap_or_default();
        let missing: Vec<&str> = EXCLUDES.iter().copied().filter(|e| !existing.lines().any(|l| l.trim() == *e)).collect();
        if !missing.is_empty() {
            let write = || -> std::io::Result<()> {
                if let Some(dir) = exclude.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let sep = if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" };
                std::fs::write(exclude, format!("{existing}{sep}# Added by LocalSync\n{}\n", missing.join("\n")))
            };
            write().map_err(|e| plain_error(&e.to_string()))?;
        }
    }
    // Only this folder: if it sits inside a larger repository with no commits
    // yet, a bare `add -A` would stage that whole repository's tree.
    git(&["add", "-A", "--", "."])?;
    git(&[
        "-c", "user.name=LocalSync", "-c", "user.email=localsync@localhost",
        "commit", "-q", "--no-verify", "--no-gpg-sign", "--allow-empty", "-m", "Project files (set up by LocalSync)",
    ])?;
    Ok(())
}

/// Git's own error, or plain words for the cases a person can act on.
fn plain_error(raw: &str) -> String {
    if raw == ls_snapshot::GIT_MISSING {
        return raw.to_string();
    }
    let lower = raw.to_lowercase();
    if ["permission denied", "read-only file system", "access is denied", "operation not permitted"].iter().any(|s| lower.contains(s)) {
        return READ_ONLY.to_string();
    }
    format!("Couldn't set up git for this folder: {raw}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    fn tracked(dir: &Path) -> Vec<String> {
        git_out(dir, &["ls-files"]).lines().map(str::to_string).collect()
    }

    /// A downloaded Flask app with the folders a real one picks up.
    fn project(dir: &Path) {
        std::fs::write(dir.join("app.py"), "from flask import Flask\n").unwrap();
        std::fs::write(dir.join("requirements.txt"), "flask\n").unwrap();
        std::fs::write(dir.join("debug.log"), "x").unwrap();
        for d in ["venv/lib", "__pycache__", "node_modules/x", "static"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
            std::fs::write(dir.join(d).join("f"), "x").unwrap();
        }
    }

    fn top_level(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn a_plain_folder_gets_a_repo_one_commit_and_excludes_in_git_info() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        project(p);
        let before = top_level(p);
        init_repo(p).unwrap();

        let mut after = top_level(p);
        after.retain(|n| n != ".git");
        assert_eq!(after, before, "only .git is added - no .gitignore or anything else written into the folder");
        assert_eq!(tracked(p), vec!["app.py", "requirements.txt", "static/f"], "build/dependency folders and logs aren't committed");
        let exclude = std::fs::read_to_string(p.join(".git/info/exclude")).unwrap();
        for e in EXCLUDES {
            assert!(exclude.lines().any(|l| l == *e), "{e} missing from {exclude}");
        }
        assert_eq!(git_out(p, &["rev-list", "--count", "HEAD"]).trim(), "1");
        assert_eq!(git_out(p, &["log", "-1", "--format=%an <%ae>"]).trim(), "LocalSync <localsync@localhost>");
        assert!(ls_snapshot::head_commit(p).is_ok());
    }

    #[test]
    fn an_existing_gitignore_is_respected_and_not_touched() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        project(p);
        let ignore = "venv/\n*.log\n";
        std::fs::write(p.join(".gitignore"), ignore).unwrap();
        init_repo(p).unwrap();

        assert_eq!(std::fs::read_to_string(p.join(".gitignore")).unwrap(), ignore);
        // The project's own rules only: node_modules and __pycache__ aren't in them.
        assert_eq!(tracked(p), vec![".gitignore", "__pycache__/f", "app.py", "node_modules/x/f", "requirements.txt", "static/f"]);
        assert!(!std::fs::read_to_string(p.join(".git/info/exclude")).unwrap_or_default().contains("node_modules"));
    }

    #[test]
    fn a_repo_with_no_commits_is_committed_not_re_initialized() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        project(p);
        git_out(p, &["init", "-q"]);
        git_out(p, &["config", "localsync.marker", "kept"]);
        init_repo(p).unwrap();
        assert_eq!(git_out(p, &["config", "localsync.marker"]).trim(), "kept", "the repo's own config survives: not re-initialized");
        assert_eq!(tracked(p), vec!["app.py", "requirements.txt", "static/f"]);
        assert_eq!(git_out(p, &["rev-list", "--count", "HEAD"]).trim(), "1");
    }

    /// Runs [`works_with_no_git_identity_inner`] in a child copy of this test
    /// binary whose git sees no user config at all (env vars are
    /// process-wide, so setting them here would leak into parallel tests).
    #[test]
    fn works_with_no_git_identity() {
        let home = tempfile::tempdir().unwrap();
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "init_repo::tests::works_with_no_git_identity_inner", "--ignored", "--nocapture"])
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL")
            .env_remove("EMAIL")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success() && text.contains("1 passed"), "{text}\n{}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    #[ignore = "run by works_with_no_git_identity with an empty git config"]
    fn works_with_no_git_identity_inner() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        let configured = Command::new("git").args(["config", "user.email"]).current_dir(p).output().unwrap();
        assert!(!configured.status.success(), "the child must see no git identity");
        project(p);
        init_repo(p).unwrap();
        assert_eq!(git_out(p, &["rev-list", "--count", "HEAD"]).trim(), "1");
    }

    #[test]
    fn inside_a_larger_repo_with_no_commits_only_this_folder_is_committed() {
        let outer = tempfile::tempdir().unwrap();
        git_out(outer.path(), &["init", "-q"]);
        std::fs::write(outer.path().join("someone-elses-file.txt"), "not part of the project").unwrap();
        let p = outer.path().join("project");
        std::fs::create_dir(&p).unwrap();
        project(&p);
        init_repo(&p).unwrap();
        let committed = git_out(outer.path(), &["ls-files"]);
        assert!(!committed.contains("someone-elses-file.txt"), "nothing outside the chosen folder: {committed}");
        assert!(committed.contains("project/app.py"), "{committed}");
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_folder_gets_a_plain_message() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::write(p.join("app.py"), "x").unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o555)).unwrap();
        let writable = std::fs::write(p.join("probe"), "").is_ok(); // root ignores modes
        let result = init_repo(p);
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !writable {
            assert_eq!(result, Err(READ_ONLY.to_string()));
        }
    }
}
