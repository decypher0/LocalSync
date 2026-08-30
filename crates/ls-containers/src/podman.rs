//! Thin process-shelling wrappers around `podman` / `podman-compose`. No
//! logic lives here beyond building the right command line — compose-file
//! rewriting is `compose.rs`'s job, this just runs binaries.

use anyhow::{ensure, Context, Result};
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::ProvisioningLog;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Prevents a spawned console-mode child (podman/podman-compose are both
/// this) from popping its own visible console window when the parent is a
/// GUI app with no console of its own (Windows' default behavior otherwise
/// for exactly this case — verified against Microsoft's own documented
/// `CREATE_NO_WINDOW` process creation flag). No-op on every other
/// platform (`#[cfg(windows)]`) — this is the one central place round 12's
/// audit routes every real (non-test) subprocess spawn in this crate
/// through, rather than repeating the flag at each call site.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg_attr(not(windows), allow(unused_mut))]
fn command(program: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

#[cfg_attr(not(windows), allow(unused_mut))]
fn sync_command(program: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Sync, cheap check for whether `binary --version` succeeds. Used to decide
/// whether integration tests should run at all, and could back a UI
/// preflight check later.
pub fn binary_available(binary: &str) -> bool {
    sync_command(binary)
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
    let output = command("podman")
        .args(["volume", "inspect", name])
        .output()
        .await?;
    Ok(output.status.success())
}

/// Runs `program args...` with `cwd`, capturing stdout/stderr *live* — each
/// line is appended to `log` (if given) as it arrives, not just returned
/// after the process exits — so a slow step (an image pull, a Maven build)
/// shows real progress rather than going silent until it's done. This is
/// the actual data source behind the Run flow's "Show details" panel
/// (round 9) for the part of a Run that panel previously showed nothing
/// useful for: `ensure_podman_ready`'s own pre-flight checks (a handful of
/// near-instant lines on Linux) were the only thing being logged before
/// this — the slow part, `podman-compose up` actually building/pulling
/// images, wrote its output nowhere but this function's return value,
/// which nothing was tailing.
///
/// Reading stdout and stderr concurrently (not sequentially, not only after
/// `wait()`) for the same reason `crates/ls-snapshot/src/bundle.rs`'s
/// `git_bytes` does: a child that fills the OS pipe buffer on one stream
/// while nobody's draining it stalls, and `podman-compose`'s build/pull
/// output can be large enough to hit that in practice, not just in theory.
async fn run_streaming(mut cmd: tokio::process::Command, log: Option<&ProvisioningLog>) -> Result<()> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().context("failed to spawn subprocess")?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    let stdout_task = tokio::spawn(stream_lines_to_log(stdout, log.map(ToOwned::to_owned)));
    let stderr_task = tokio::spawn(stream_lines_to_log(stderr, log.map(ToOwned::to_owned)));

    let status = child.wait().await.context("waiting on subprocess")?;
    // Let both readers finish draining before checking status - same
    // ordering git_bytes uses, so no output is lost even if the process
    // exits right as it's still flushing its last lines.
    let _ = stdout_task.await;
    let _ = stderr_task.await;

    ensure!(status.success(), "subprocess exited with {status}");
    Ok(())
}

async fn stream_lines_to_log(pipe: impl tokio::io::AsyncRead + Unpin, log: Option<ProvisioningLog>) {
    let mut lines = BufReader::new(pipe).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(log) = &log {
            log.info(&line);
        }
    }
}

/// `podman-compose -p <project> up -d --build`, run with `compose_dir` as
/// cwd so it picks up the rewritten `docker-compose.yml` there. Relies on
/// Podman's own content-addressed layer cache (no `--no-cache`) for "second
/// run is fast" — nothing here reimplements that.
pub async fn compose_up(compose_dir: &Path, project: &str, log: Option<&ProvisioningLog>) -> Result<()> {
    let mut cmd = command("podman-compose");
    cmd.args(["-p", project, "up", "-d", "--build"]).current_dir(compose_dir);
    run_streaming(cmd, log).await.context("podman-compose up failed")
}

/// `podman-compose -p <project> down`. Containers/network only — the named
/// DB volume is never touched here, that's the point of it being a separate
/// podman volume rather than part of the compose project's lifecycle.
pub async fn compose_down(compose_dir: &Path, project: &str, log: Option<&ProvisioningLog>) -> Result<()> {
    let mut cmd = command("podman-compose");
    cmd.args(["-p", project, "down"]).current_dir(compose_dir);
    run_streaming(cmd, log).await.context("podman-compose down failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_available_is_false_for_nonexistent_binary() {
        assert!(!binary_available("definitely-not-a-real-binary-xyz"));
    }

    /// Fast, focused proof of round 12's actual fix, independent of the
    /// slow full-`run_snapshot`-through-real-podman-compose path
    /// `run_progress_test.rs` (in the desktop app's own test suite) also
    /// covers: `run_streaming` genuinely captures a real subprocess's
    /// stdout *and* stderr live and writes every line to the log — not
    /// discarded to an inherited/separate console, not silently dropped,
    /// not only stdout. This is the actual mechanism behind both this
    /// round's fixes (no visible console window *and* the details panel
    /// having real content to show) - a subprocess whose output isn't
    /// captured at all couldn't feed either one.
    #[tokio::test]
    async fn run_streaming_captures_both_stdout_and_stderr_lines_live() {
        let dir = tempfile::tempdir().unwrap();
        let log = ProvisioningLog::open_in(dir.path()).unwrap();

        let mut cmd = command("sh");
        cmd.args(["-c", "echo from-stdout-1; echo from-stderr-1 >&2; echo from-stdout-2"]);
        run_streaming(cmd, Some(&log))
            .await
            .expect("a real, successful subprocess should not error");

        let contents = std::fs::read_to_string(log.path()).unwrap();
        assert!(contents.contains("from-stdout-1"), "stdout line 1 missing from log: {contents:?}");
        assert!(contents.contains("from-stdout-2"), "stdout line 2 missing from log: {contents:?}");
        assert!(contents.contains("from-stderr-1"), "stderr line missing from log: {contents:?}");
    }

    /// A failing subprocess is a real error, not silently swallowed -
    /// matters because `run_streaming` restructured how the exit status is
    /// checked (after concurrently draining both pipes, not via the
    /// simpler-but-pipe-buffer-unsafe `Command::status()`/`::output()`).
    #[tokio::test]
    async fn run_streaming_surfaces_a_nonzero_exit_as_an_error() {
        let mut cmd = command("sh");
        cmd.args(["-c", "exit 7"]);
        let result = run_streaming(cmd, None).await;
        assert!(result.is_err(), "a nonzero exit status must be a real Err, not silently ok");
    }
}
