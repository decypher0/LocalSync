//! Thin process-shelling wrappers around `podman` / `podman-compose`. No
//! logic lives here beyond building the right command line — compose-file
//! rewriting is `compose.rs`'s job, this just runs binaries.

use anyhow::{bail, Context, Result};
use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
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

    let tail = Arc::new(Mutex::new(Tail::default()));
    let stdout_task = tokio::spawn(stream_lines_to_log(stdout, log.map(ToOwned::to_owned), tail.clone()));
    let stderr_task = tokio::spawn(stream_lines_to_log(stderr, log.map(ToOwned::to_owned), tail.clone()));

    let status = child.wait().await.context("waiting on subprocess")?;
    // Let both readers finish draining before checking status - same
    // ordering git_bytes uses, so no output is lost even if the process
    // exits right as it's still flushing its last lines.
    let _ = stdout_task.await;
    let _ = stderr_task.await;

    if !status.success() {
        // "exit status: 1" alone says nothing about *why* - a failed
        // `npm ci`, a Python traceback, "port already allocated" are only in
        // the output, which otherwise goes to the log file and nowhere else.
        // Carry the end of it in the error itself.
        let tail = tail.lock().map(|t| t.render()).unwrap_or_default();
        if tail.is_empty() {
            bail!("subprocess exited with {status}");
        }
        bail!("subprocess exited with {status}. Last output:\n{tail}");
    }
    Ok(())
}

/// Bounded tail of a subprocess's combined stdout+stderr: the last
/// `TAIL_MAX_LINES` lines, at most `TAIL_MAX_BYTES` in total.
#[derive(Default)]
struct Tail {
    lines: VecDeque<String>,
    bytes: usize,
}

const TAIL_MAX_LINES: usize = 60;
const TAIL_MAX_BYTES: usize = 8 * 1024;
const TAIL_MAX_LINE: usize = 1024;

impl Tail {
    fn push(&mut self, line: &str) {
        let mut end = line.len().min(TAIL_MAX_LINE);
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        self.bytes += end + 1;
        self.lines.push_back(line[..end].to_string());
        while self.lines.len() > TAIL_MAX_LINES || self.bytes > TAIL_MAX_BYTES {
            let Some(old) = self.lines.pop_front() else { break };
            self.bytes -= old.len() + 1;
        }
    }

    fn render(&self) -> String {
        self.lines.iter().map(String::as_str).collect::<Vec<_>>().join("\n")
    }
}

async fn stream_lines_to_log(pipe: impl tokio::io::AsyncRead + Unpin, log: Option<ProvisioningLog>, tail: Arc<Mutex<Tail>>) {
    let mut lines = BufReader::new(pipe).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(log) = &log {
            log.info(&line);
        }
        if let Ok(mut t) = tail.lock() {
            t.push(&line);
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

/// What a compose service's container is doing right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceState {
    Running,
    /// Stopped (crashed or finished) with this exit code.
    Exited(i32),
    /// No container for this service (not created yet, or already removed).
    Missing,
    /// A container exists but never started (e.g. its published port was taken).
    NotStarted,
}

/// Compose-labeled container of `service` in compose project `project`:
/// (container name, podman state string, exit code). Goes through `podman ps`
/// labels (set by podman-compose itself) rather than guessing container names.
async fn service_container(project: &str, service: &str) -> Result<Option<(String, String, i32)>> {
    let output = command("podman")
        .args(["ps", "-a", "--filter"])
        .arg(format!("label=io.podman.compose.project={project}"))
        .arg("--filter")
        .arg(format!("label=com.docker.compose.service={service}"))
        .args(["--format", "{{.Names}}|{{.State}}|{{.ExitCode}}"])
        .output()
        .await
        .context("running podman ps")?;
    ensure_success(&output, "podman ps")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().find(|l| !l.trim().is_empty()).map(|line| {
        let mut parts = line.trim().splitn(3, '|');
        let name = parts.next().unwrap_or_default().to_string();
        let state = parts.next().unwrap_or_default().to_lowercase();
        let code = parts.next().and_then(|c| c.parse().ok()).unwrap_or(0);
        (name, state, code)
    }))
}

fn ensure_success(output: &std::process::Output, what: &str) -> Result<()> {
    if !output.status.success() {
        bail!("{what} failed ({}): {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

pub async fn service_state(project: &str, service: &str) -> Result<ServiceState> {
    Ok(match service_container(project, service).await? {
        None => ServiceState::Missing,
        Some((_, state, code)) => match state.as_str() {
            "exited" | "stopped" | "dead" => ServiceState::Exited(code),
            "created" | "configured" => ServiceState::NotStarted,
            // Anything else (running, paused, stopping, ...) is live.
            _ => ServiceState::Running,
        },
    })
}

/// The last `tail` lines the service's container wrote (stdout and stderr
/// merged, bounded to `TAIL_MAX_BYTES`) - "" if it has no container.
pub async fn service_logs(project: &str, service: &str, tail: usize) -> Result<String> {
    let Some((name, _, _)) = service_container(project, service).await? else { return Ok(String::new()) };
    let output = command("podman")
        .args(["logs", "--tail"])
        .arg(tail.to_string())
        .arg(&name)
        .output()
        .await
        .context("running podman logs")?;
    ensure_success(&output, "podman logs")?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    let mut tail = Tail::default();
    collapse_repeats(&text).iter().for_each(|l| tail.push(l));
    Ok(tail.render())
}

/// A crash-looping app (`restart: on-failure`) prints the same error dozens of
/// times; show it once with a count so the real reason isn't buried.
fn collapse_repeats(text: &str) -> Vec<String> {
    let mut out: Vec<(String, usize)> = Vec::new();
    for line in text.lines() {
        match out.last_mut() {
            Some((prev, n)) if prev == line => *n += 1,
            _ => out.push((line.to_string(), 1)),
        }
    }
    out.into_iter().map(|(l, n)| if n > 1 { format!("{l}  (repeated {n} times)") } else { l }).collect()
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

    /// The point of the tail: a failing subprocess's error names *why* it
    /// failed (its last output, stderr included), not just its exit status.
    #[tokio::test]
    async fn run_streaming_error_carries_the_real_output_of_a_failing_subprocess() {
        let mut cmd = command("sh");
        cmd.args(["-c", "echo building step 1; echo npm ERR! missing package left-pad >&2; exit 1"]);
        let err = format!("{:#}", run_streaming(cmd, None).await.unwrap_err());
        assert!(err.contains("exit status: 1"), "{err}");
        assert!(err.contains("building step 1"), "stdout missing from error: {err}");
        assert!(err.contains("npm ERR! missing package left-pad"), "stderr missing from error: {err}");
    }

    /// The error's output is bounded (lines and bytes) and keeps the *end*,
    /// where the failure is - and a very long single line can't blow it up.
    #[tokio::test]
    async fn run_streaming_error_tail_is_bounded_and_keeps_the_last_lines() {
        let mut cmd = command("sh");
        cmd.args([
            "-c",
            "i=1; while [ $i -le 500 ]; do echo line-$i; i=$((i+1)); done; \
             head -c 50000 /dev/zero | tr '\\0' x; echo; echo the-real-error; exit 2",
        ]);
        let err = format!("{:#}", run_streaming(cmd, None).await.unwrap_err());
        assert!(err.contains("the-real-error"), "{err}");
        assert!(!err.contains("line-1\n") && !err.contains("line-200\n"), "old lines should be dropped");
        assert!(err.len() < TAIL_MAX_BYTES + 512, "error text is not bounded: {} bytes", err.len());
    }

    #[test]
    fn collapse_repeats_folds_consecutive_identical_lines_only() {
        let got = collapse_repeats("a
b
b
b
a
");
        assert_eq!(got, vec!["a", "b  (repeated 3 times)", "a"]);
    }

    #[test]
    fn tail_keeps_at_most_the_last_sixty_lines() {
        let mut t = Tail::default();
        (0..200).for_each(|i| t.push(&format!("l{i}")));
        let lines: Vec<_> = t.render().lines().map(String::from).collect();
        assert_eq!(lines.len(), TAIL_MAX_LINES);
        assert_eq!(lines.last().unwrap(), "l199");
        assert_eq!(lines[0], "l140");
    }

    /// A subprocess that produces lots of output but succeeds is unaffected
    /// by the tail (no error, no truncation of the log).
    #[tokio::test]
    async fn run_streaming_success_with_heavy_output_is_still_ok_and_fully_logged() {
        let dir = tempfile::tempdir().unwrap();
        let log = ProvisioningLog::open_in(dir.path()).unwrap();
        let mut cmd = command("sh");
        cmd.args(["-c", "i=1; while [ $i -le 300 ]; do echo out-$i; i=$((i+1)); done"]);
        run_streaming(cmd, Some(&log)).await.expect("success must stay Ok");
        let contents = std::fs::read_to_string(log.path()).unwrap();
        assert!(contents.contains("out-1\n") && contents.contains("out-300"), "log lost lines");
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
