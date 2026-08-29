//! Thin process-shelling wrappers around `podman` / `podman-compose`. No
//! logic lives here beyond building the right command line — compose-file
//! rewriting is `compose.rs`'s job, this just runs binaries.

use anyhow::{ensure, Result};
use std::path::Path;

/// Sync, cheap check for whether `binary --version` succeeds. Used to decide
/// whether integration tests should run at all, and could back a UI
/// preflight check later.
pub fn binary_available(binary: &str) -> bool {
    std::process::Command::new(binary)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn podman_available() -> bool {
    binary_available("podman")
}

pub fn podman_compose_available() -> bool {
    binary_available("podman-compose")
}

/// True if a podman volume named `name` already exists — i.e. this is a
/// cache hit on the database data volume, not a cold start.
pub async fn volume_exists(name: &str) -> Result<bool> {
    let output = tokio::process::Command::new("podman")
        .args(["volume", "inspect", name])
        .output()
        .await?;
    Ok(output.status.success())
}

/// `podman-compose -p <project> up -d --build`, run with `compose_dir` as
/// cwd so it picks up the rewritten `docker-compose.yml` there. Relies on
/// Podman's own content-addressed layer cache (no `--no-cache`) for "second
/// run is fast" — nothing here reimplements that.
pub async fn compose_up(compose_dir: &Path, project: &str) -> Result<()> {
    let status = tokio::process::Command::new("podman-compose")
        .args(["-p", project, "up", "-d", "--build"])
        .current_dir(compose_dir)
        .status()
        .await?;
    ensure!(status.success(), "podman-compose up failed: {status}");
    Ok(())
}

/// `podman-compose -p <project> down`. Containers/network only — the named
/// DB volume is never touched here, that's the point of it being a separate
/// podman volume rather than part of the compose project's lifecycle.
pub async fn compose_down(compose_dir: &Path, project: &str) -> Result<()> {
    let status = tokio::process::Command::new("podman-compose")
        .args(["-p", project, "down"])
        .current_dir(compose_dir)
        .status()
        .await?;
    ensure!(status.success(), "podman-compose down failed: {status}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_available_is_false_for_nonexistent_binary() {
        assert!(!binary_available("definitely-not-a-real-binary-xyz"));
    }
}
