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

use std::time::Duration;

use anyhow::Result;
use serde::Serialize;

use crate::ProvisioningLog;

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

/// Runs the functional probe now, uncached: start a throwaway container with
/// a 1 GB memory limit (`podman run --rm --memory 1g docker.io/library/busybox
/// true`, pulling the image if missing) and report whether it really ran.
pub async fn functional_probe(timeout: Duration) -> ProbeOutcome {
    let _ = timeout;
    unimplemented!("readiness agent")
}

/// The gate before every Podman-dependent action. See the module docs.
/// Logs its steps to `log` with lines containing "provisioning check" (the
/// Run progress UI and an existing test rely on that wording).
pub async fn ensure_ready_for_run(log: &ProvisioningLog) -> Result<()> {
    let _ = log;
    unimplemented!("readiness agent")
}

/// Forget a cached success (after the setup wizard changes something, or a
/// Run fails in a way that suggests Podman broke).
pub fn invalidate_probe_cache() {
    unimplemented!("readiness agent")
}
