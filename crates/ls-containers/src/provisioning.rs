//! Ensures a working Podman is reachable before any `podman`/`podman-compose`
//! command runs, provisioning one if it isn't. On Linux, Podman runs
//! natively — this is close to a no-op, reusing the checks
//! [`crate::podman::podman_available`]/[`crate::podman::podman_compose_available`]
//! already had since round 1. On Windows and macOS, Podman's containers run
//! inside a VM ("podman machine" — WSL2-backed on Windows, QEMU-backed on
//! macOS) that has to exist and be started first; if the `podman` CLI itself
//! isn't installed, this attempts to install it too (`winget`/`brew`).
//!
//! Every step is written to [`ProvisioningLog`] as it happens — not just the
//! final outcome — so a developer hitting a failure on their own Windows/Mac
//! machine can paste the log and have it mean something without needing
//! anyone to reproduce their machine (see `docs/round5-manual-test-checklist.md`).

use anyhow::{Context, Result};
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;

/// Appends timestamped lines to a plain text file at
/// `{app_data_dir}/logs/provisioning.log` (or a caller-chosen path — tests
/// use a tempdir). Deliberately not a rotating/structured-binary log: this
/// only ever needs to be opened once, read top to bottom, and pasted.
pub struct ProvisioningLog {
    path: PathBuf,
}

impl ProvisioningLog {
    /// `{OS data dir}/localsync/logs/provisioning.log` — e.g.
    /// `%APPDATA%\localsync\logs\provisioning.log` on Windows,
    /// `~/Library/Application Support/localsync/logs/provisioning.log` on
    /// macOS, `~/.local/share/localsync/logs/provisioning.log` on Linux
    /// (via the `dirs` crate, so this doesn't need Tauri's `AppHandle` and
    /// `run_snapshot`'s signature doesn't have to change to thread one
    /// through). Created, including parent dirs, if it doesn't exist yet.
    pub fn open_default() -> Result<Self> {
        let base = dirs::data_dir().context("could not determine the OS data directory")?;
        let dir = base.join("localsync").join("logs");
        std::fs::create_dir_all(&dir)?;
        Ok(Self { path: dir.join("provisioning.log") })
    }

    #[cfg(test)]
    pub fn open_in(dir: &std::path::Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self { path: dir.join("provisioning.log") })
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Appends one line: `[<rfc3339 timestamp>] [<level>] <msg>`. Never
    /// panics on a write failure (a broken log must not break provisioning
    /// itself) — logs the failure to stderr as a last resort instead.
    pub fn log(&self, level: &str, msg: &str) {
        let line = format!(
            "[{}] [{level}] {msg}\n",
            time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_else(|_| "unknown-time".to_string())
        );
        let result = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
        if let Err(e) = result {
            eprintln!("provisioning log write failed ({}): {line}", e);
        }
    }

    pub fn info(&self, msg: &str) {
        self.log("info", msg);
    }
    pub fn error(&self, msg: &str) {
        self.log("error", msg);
    }
}

/// Detected OS + version, logged as the first line of every provisioning
/// attempt so a pasted log always says what it's actually about.
fn os_description() -> String {
    format!("{} ({})", std::env::consts::OS, os_version_detail())
}

#[cfg(target_os = "windows")]
fn os_version_detail() -> String {
    // Filled in by the Windows provisioning implementation.
    windows_impl::detailed_version()
}

#[cfg(target_os = "macos")]
fn os_version_detail() -> String {
    macos_impl::detailed_version()
}

#[cfg(target_os = "linux")]
fn os_version_detail() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string()))
        })
        .unwrap_or_else(|| "unknown Linux".to_string())
}

/// The one entry point `ls_containers::run_snapshot` calls before doing
/// anything else. Replaces round 1's bare
/// `podman_available()`/`podman_compose_available()` `ensure!`s with the
/// same guarantee on Linux (podman must already work, nothing to
/// provision) plus an actual provisioning attempt on Windows/macOS.
pub async fn ensure_podman_ready(log: &ProvisioningLog) -> Result<()> {
    log.info(&format!("provisioning check starting — OS: {}", os_description()));

    #[cfg(target_os = "linux")]
    let result = linux_impl::ensure_ready(log).await;
    #[cfg(target_os = "windows")]
    let result = windows_impl::ensure_ready(log).await;
    #[cfg(target_os = "macos")]
    let result = macos_impl::ensure_ready(log).await;

    match &result {
        Ok(()) => log.info("provisioning check: podman is ready"),
        Err(e) => log.error(&format!("provisioning check failed: {e:#}")),
    }
    result
}

#[cfg(target_os = "linux")]
mod linux_impl {
    use super::ProvisioningLog;
    use anyhow::Result;

    /// Linux has no VM to provision — Podman runs natively. This is exactly
    /// round 1's `run_snapshot` preflight, moved here so Windows/macOS can
    /// share the same call site in `lib.rs` without an `#[cfg]` at every
    /// caller.
    pub async fn ensure_ready(log: &ProvisioningLog) -> Result<()> {
        anyhow::ensure!(
            crate::podman::podman_available(),
            "podman not found on PATH — run scripts/setup-linux-deps.sh"
        );
        log.info("podman found on PATH");
        anyhow::ensure!(
            crate::podman::podman_compose_available(),
            "podman-compose not found on PATH — run scripts/setup-linux-deps.sh"
        );
        log.info("podman-compose found on PATH");
        Ok(())
    }
}

// --- Windows and macOS implementations below this line are what this
// round's platform agents fill in. Keep the same shape: log every step,
// never swallow a command's real error text, and end with podman machine
// actually running and podman-compose reachable. ---

#[cfg(target_os = "windows")]
mod windows_impl;

#[cfg(target_os = "macos")]
mod macos_impl;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_writes_readable_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log = ProvisioningLog::open_in(dir.path()).unwrap();
        log.info("hello");
        log.error("world");
        let contents = std::fs::read_to_string(log.path()).unwrap();
        assert!(contents.contains("[info] hello"));
        assert!(contents.contains("[error] world"));
        assert!(contents.lines().count() == 2);
    }
}
