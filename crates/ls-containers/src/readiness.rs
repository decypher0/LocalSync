//! Is Podman *functionally* ready - not just installed?
//!
//! Podman can look fully installed (binary present, machine exists, `podman
//! info` fine) and still be unable to start a container: after the WSL 3.0.1
//! update, `podman run --memory 1g busybox` failed with `crun: open
//! memory.max for writing: No such file or directory`, and every LocalSync Run
//! with it. A presence/version check never catches that, so the one real test
//! everywhere is to start a container with a memory limit and see it succeed.
//!
//! Two entry points:
//! - [`functional_probe`]: always runs the probe (uncached) - the setup
//!   wizard's check and its Recheck button.
//! - [`ensure_ready_for_run`]: the gate in front of every Podman-dependent
//!   action (`run_snapshot`, `run_existing`, and through them the receiver's
//!   Run and the compose wizard's test run). Starts an existing-but-stopped
//!   podman machine (not system-changing; needed after every reboot on
//!   Windows/macOS), then runs the probe - reusing a *successful* result for
//!   [`PROBE_CACHE_TTL`] so it doesn't add a delay to every click. Never
//!   installs, initializes or enables anything: that only happens in the
//!   setup wizard, with the person's consent.

use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use serde::Serialize;
use tokio::io::AsyncReadExt;

use crate::podman::{podman_available, podman_compose_available};
use crate::{ProvisioningLog, MEMORY_LIMIT_UNSUPPORTED};

/// How long a successful probe is trusted. Failures are never cached, so a
/// Recheck or retry always re-probes.
pub const PROBE_CACHE_TTL: Duration = Duration::from_secs(90);

/// Default upper bound on one probe, image pull included (the first probe on
/// a machine pulls busybox, ~2 MB).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(120);

/// Every error [`ensure_ready_for_run`] returns starts with this, so the UI
/// can recognize "Podman isn't ready" failures and offer to open setup.
pub const NOT_READY_PREFIX: &str = "Podman isn't ready to run containers";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeOutcome {
    pub ok: bool,
    /// One plain-language sentence for the person (never raw output), e.g.
    /// "Podman can't apply memory limits on this computer." Empty when ok.
    pub summary: String,
    /// The probe command's real stderr+stdout (bounded), for "Show details".
    pub details: String,
    /// The failure is the cgroup memory-delegation one (see
    /// `crate::is_memory_limit_unsupported`).
    pub memory_limit_unsupported: bool,
    pub duration_ms: u64,
}

const PROBE_IMAGE: &str = "docker.io/library/busybox";
const DETAILS_MAX: usize = 8 * 1024;

/// Runs the functional probe now, uncached: start a throwaway container with
/// a 1 GB memory limit (`podman run --rm --memory 1g docker.io/library/busybox
/// true`, pulling the image if missing) and report whether it really ran.
pub async fn functional_probe(timeout: Duration) -> ProbeOutcome {
    let started = Instant::now();
    let run = run_probe(timeout).await;
    let memory_limit_unsupported = !run.ok && crate::is_memory_limit_unsupported(&run.details);
    let summary = if run.ok {
        String::new()
    } else if run.not_found {
        "Podman isn't installed.".to_string()
    } else if run.timed_out {
        format!(
            "Podman didn't respond within {} seconds.",
            timeout.as_secs_f64().ceil() as u64
        )
    } else {
        classify(&run.details).to_string()
    };
    ProbeOutcome {
        ok: run.ok,
        summary,
        details: run.details,
        memory_limit_unsupported,
        duration_ms: started.elapsed().as_millis() as u64,
    }
}

/// Plain-language cause for a probe that ran and failed.
fn classify(output: &str) -> &'static str {
    let lower = output.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));
    if crate::is_memory_limit_unsupported(output) {
        "Podman can't apply memory limits on this computer, so LocalSync's sandbox can't start containers."
    } else if has(&[
        "cannot connect to podman",
        "unable to connect to podman",
        "vm does not exist",
    ]) || (lower.contains("machine") && lower.contains("not running"))
    {
        "Podman's virtual machine isn't running."
    } else if has(&[
        "trying to pull",
        "initializing source",
        "pinging container registry",
        "pulling image",
        "manifest unknown",
    ]) {
        "Podman couldn't download its test image (busybox) - check the internet connection."
    } else if has(&["connection refused", "no connection could be made"]) {
        "Podman's virtual machine isn't running."
    } else {
        "Podman couldn't start a test container."
    }
}

struct ProbeRun {
    ok: bool,
    timed_out: bool,
    not_found: bool,
    /// Bounded stderr + stdout.
    details: String,
}

async fn run_probe(timeout: Duration) -> ProbeRun {
    let fail = |timed_out, not_found, details| ProbeRun {
        ok: false,
        timed_out,
        not_found,
        details,
    };
    let mut cmd = command("podman");
    cmd.args([
        "run",
        "--rm",
        "--memory",
        "1g",
        "--pull=missing",
        PROBE_IMAGE,
        "true",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return fail(false, true, format!("podman not found on PATH: {e}"))
        }
        Err(e) => return fail(false, false, format!("failed to start podman: {e}")),
    };
    // ponytail: buffers the whole output, then keeps the tail; probe output is a few KB.
    let drain = |mut r: Box<dyn tokio::io::AsyncRead + Unpin + Send>| {
        tokio::spawn(async move {
            let mut b = Vec::new();
            let _ = r.read_to_end(&mut b).await;
            b
        })
    };
    let err_task = drain(Box::new(child.stderr.take().expect("piped")));
    let out_task = drain(Box::new(child.stdout.take().expect("piped")));

    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(s)) => Some(s),
        Ok(Err(e)) => return fail(false, false, format!("waiting on podman failed: {e}")),
        Err(_) => {
            let _ = child.kill().await;
            None
        }
    };
    // A killed child's grandchildren could keep the pipes open; don't hang on them.
    let grab = |t: tokio::task::JoinHandle<Vec<u8>>| async move {
        match tokio::time::timeout(Duration::from_secs(2), t).await {
            Ok(Ok(b)) => String::from_utf8_lossy(&b).into_owned(),
            _ => String::new(),
        }
    };
    let (stderr, stdout) = (grab(err_task).await, grab(out_task).await);
    let mut details = [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if details.len() > DETAILS_MAX {
        let mut start = details.len() - DETAILS_MAX;
        while !details.is_char_boundary(start) {
            start += 1;
        }
        details = details[start..].to_string();
    }
    match status {
        Some(s) if s.success() => ProbeRun {
            ok: true,
            timed_out: false,
            not_found: false,
            details,
        },
        Some(s) if details.is_empty() => fail(
            false,
            false,
            format!("podman exited with {s} and printed nothing"),
        ),
        Some(_) => fail(false, false, details),
        None => {
            let note = format!("podman run did not finish within {timeout:?} and was stopped");
            fail(
                true,
                false,
                if details.is_empty() {
                    note
                } else {
                    format!("{details}\n{note}")
                },
            )
        }
    }
}

/// When the probe last succeeded. Failures are never stored.
static PROBE_OK_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// The gate before every Podman-dependent action. See the module docs.
/// Logs its steps to `log` with lines containing "provisioning check" (the
/// Run progress UI and an existing test rely on that wording).
pub async fn ensure_ready_for_run(log: &ProvisioningLog) -> Result<()> {
    gate(Some(log)).await
}

/// [`ensure_ready_for_run`] for a caller that couldn't open a log file:
/// same checks, logged to stderr.
pub(crate) async fn gate(log: Option<&ProvisioningLog>) -> Result<()> {
    let say = Say(log);
    say.call(
        "info",
        &format!("provisioning check starting - OS: {}", std::env::consts::OS),
    );
    let result = gate_inner(say).await;
    match &result {
        Ok(()) => say.call("info", "provisioning check: podman is ready"),
        Err(e) => say.call("error", &format!("provisioning check failed: {e:#}")),
    }
    result
}

async fn gate_inner(say: Say<'_>) -> Result<()> {
    if !podman_available() {
        bail!(
            "{NOT_READY_PREFIX}: Podman isn't installed.\nOpen Setup in LocalSync to install it."
        );
    }
    say.call("info", "provisioning check: podman found on PATH");
    if !podman_compose_available() {
        bail!("{NOT_READY_PREFIX}: podman-compose isn't installed.\nOpen Setup in LocalSync to install it.");
    }
    say.call("info", "provisioning check: podman-compose found on PATH");

    if cfg!(any(target_os = "windows", target_os = "macos")) {
        start_stopped_machine(say).await;
    }

    if let Some(at) = *PROBE_OK_AT.lock().unwrap() {
        if at.elapsed() < PROBE_CACHE_TTL {
            say.call(
                "info",
                "provisioning check: a test container ran recently, skipping the probe",
            );
            return Ok(());
        }
    }
    say.call(
        "info",
        "provisioning check: starting a memory-limited test container",
    );
    let outcome = functional_probe(PROBE_TIMEOUT).await;
    if outcome.ok {
        *PROBE_OK_AT.lock().unwrap() = Some(Instant::now());
        say.call(
            "info",
            &format!(
                "provisioning check: test container ran in {} ms",
                outcome.duration_ms
            ),
        );
        return Ok(());
    }
    let guidance = if outcome.memory_limit_unsupported {
        format!("{MEMORY_LIMIT_UNSUPPORTED}\n")
    } else {
        String::new()
    };
    bail!(
        "{NOT_READY_PREFIX}: {}\nOpen Setup in LocalSync to fix it.\n{guidance}Details:\n{}",
        outcome.summary,
        outcome.details
    )
}

/// Starts an existing-but-stopped podman machine (needed after every reboot
/// on Windows/macOS). Never creates one. A failure is only logged: the probe
/// that follows reports the real problem.
async fn start_stopped_machine(say: Say<'_>) {
    let Ok(listed) = command("podman")
        .args(["machine", "list", "--format", "{{.Name}}\t{{.Running}}"])
        .stdin(Stdio::null())
        .output()
        .await
    else {
        return;
    };
    let text = String::from_utf8_lossy(&listed.stdout).into_owned();
    let machines: Vec<(&str, bool)> = text
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(name, running)| (name.trim(), running.trim() == "true"))
        .collect();
    if machines.is_empty() || machines.iter().any(|(_, running)| *running) {
        return;
    }
    // The default machine's name carries a trailing '*'.
    let name = machines
        .iter()
        .find(|(n, _)| n.ends_with('*'))
        .unwrap_or(&machines[0])
        .0
        .trim_end_matches('*');
    say.call(
        "info",
        &format!("provisioning check: starting podman machine {name}"),
    );
    match command("podman")
        .args(["machine", "start", name])
        .stdin(Stdio::null())
        .output()
        .await
    {
        Ok(o) if o.status.success() => {
            say.call("info", "provisioning check: podman machine started")
        }
        Ok(o) => say.call(
            "error",
            &format!(
                "provisioning check: podman machine start failed: {}{}",
                String::from_utf8_lossy(&o.stderr),
                String::from_utf8_lossy(&o.stdout)
            ),
        ),
        Err(e) => say.call(
            "error",
            &format!("provisioning check: podman machine start failed: {e}"),
        ),
    }
}

/// Where the gate's lines go: the log file, or stderr without one. A plain
/// struct (not a `dyn Fn`) so the gate's future stays `Send`.
#[derive(Clone, Copy)]
struct Say<'a>(Option<&'a ProvisioningLog>);

impl Say<'_> {
    fn call(self, level: &str, msg: &str) {
        match self.0 {
            Some(l) => l.log(level, msg),
            None => eprintln!("[{level}] {msg}"),
        }
    }
}

/// Forget a cached success (after the setup wizard changes something, or a
/// Run fails in a way that suggests Podman broke).
pub fn invalidate_probe_cache() {
    *PROBE_OK_AT.lock().unwrap() = None;
}

/// Same as `podman::command` (private there): hides the console window a
/// GUI parent would otherwise flash on Windows.
#[cfg_attr(not(windows), allow(unused_mut))]
fn command(program: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd
}

#[cfg(test)]
mod tests {
    use super::classify;

    /// Tauri commands and `run_snapshot` need the gate's future to be Send.
    #[allow(dead_code)]
    fn gate_future_is_send(log: &crate::ProvisioningLog) {
        fn assert_send<T: Send>(_: T) {}
        assert_send(super::ensure_ready_for_run(log));
        assert_send(super::gate(None));
    }

    #[test]
    fn classifies_causes() {
        let mem = "Error: crun: open `memory.max` for writing: No such file or directory: OCI runtime attempted to invoke a command that was not found";
        assert!(classify(mem).contains("memory limits"));
        assert!(
            classify("Cannot connect to Podman. Please verify your connection")
                .contains("virtual machine")
        );
        assert!(
            classify("Error: dial unix /run/podman.sock: connect: connection refused")
                .contains("virtual machine")
        );
        assert!(classify(
            "Trying to pull docker.io/library/busybox:latest...\nError: initializing source docker://busybox: pinging container registry registry-1.docker.io: dial tcp: lookup registry-1.docker.io: no such host"
        )
        .contains("download"));
        assert_eq!(
            classify("Error: something odd"),
            "Podman couldn't start a test container."
        );
    }
}
