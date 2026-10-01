//! The dependency-setup steps the app's setup wizard walks through, per OS:
//! a read-only check for each, and the system-changing fix the person
//! consents to. Ordering and persistence (which steps are done) live in the
//! desktop crate's setup state file; this module only knows how to *check*
//! and *fix* one step on this machine.
//!
//! Sequences (see [`steps_for`]):
//! - Windows: PodmanInstalled -> WslEnabled -> RestartAfterWsl ->
//!   MachineReady -> FunctionalCheck
//! - macOS:   PodmanInstalled -> MachineReady -> FunctionalCheck
//! - Linux:   PodmanInstalled -> FunctionalCheck (no VM on Linux)
//!
//! `PodmanInstalled` covers both binaries LocalSync needs: `podman` and
//! `podman-compose`.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::ProvisioningLog;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupStep {
    PodmanInstalled,
    /// Windows only.
    WslEnabled,
    /// Windows only: always shown after enabling WSL, unconditionally (no
    /// attempt to detect whether a restart is strictly needed). It has no
    /// check of its own: it is done once the person chose "Restart now" or
    /// "Exit and restart later".
    RestartAfterWsl,
    /// Windows/macOS only: `podman machine init` (if none) + `start`.
    MachineReady,
    FunctionalCheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetOs {
    Windows,
    Macos,
    Linux,
}

pub fn this_os() -> TargetOs {
    if cfg!(target_os = "windows") {
        TargetOs::Windows
    } else if cfg!(target_os = "macos") {
        TargetOs::Macos
    } else {
        TargetOs::Linux
    }
}

/// The ordered steps for `os`.
pub fn steps_for(os: TargetOs) -> Vec<SetupStep> {
    use SetupStep::*;
    match os {
        TargetOs::Windows => vec![PodmanInstalled, WslEnabled, RestartAfterWsl, MachineReady, FunctionalCheck],
        TargetOs::Macos => vec![PodmanInstalled, MachineReady, FunctionalCheck],
        TargetOs::Linux => vec![PodmanInstalled, FunctionalCheck],
    }
}

/// Result of checking one step's real current state (read-only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepCheck {
    pub ok: bool,
    /// One plain-language sentence, e.g. "Podman is installed." /
    /// "Podman isn't installed yet."
    pub summary: String,
    /// Raw command output behind "Show details" (may be empty).
    pub details: String,
}

/// What the wizard shows for a step, and how its fix behaves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepInfo {
    pub step: SetupStep,
    /// Checklist label, e.g. "Podman".
    pub title: String,
    /// The one-sentence consent text shown before `fix_step` runs, e.g. "This
    /// installs Podman and podman-compose using winget." `None` when the step
    /// has no automatic fix (`FunctionalCheck`, `RestartAfterWsl`, or Linux
    /// without a supported package manager).
    pub consent: Option<String>,
    /// The fix needs administrator rights (an OS elevation prompt will
    /// appear).
    pub needs_admin: bool,
    /// Plain-language "do it yourself" instructions for the manual path.
    pub manual_instructions: String,
}

pub fn step_info(step: SetupStep) -> StepInfo {
    let _ = step;
    unimplemented!("per-OS agent")
}

/// Read-only check of `step`'s real state on this machine. Never changes
/// anything. `FunctionalCheck` runs `crate::readiness::functional_probe`.
/// `RestartAfterWsl` has no check (the caller handles it from the state file).
pub async fn check_step(step: SetupStep, log: &ProvisioningLog) -> StepCheck {
    let _ = (step, log);
    unimplemented!("per-OS agent")
}

/// Performs `step`'s system-changing fix. The caller must have shown
/// `step_info(step).consent` and got the person's explicit OK first. Logs
/// every command and its real output to `log`; errors carry the real output.
pub async fn fix_step(step: SetupStep, log: &ProvisioningLog) -> Result<()> {
    let _ = (step, log);
    unimplemented!("per-OS agent")
}

/// Restart the computer now (Windows only; after the person chose "Restart
/// now").
pub fn restart_computer() -> Result<()> {
    unimplemented!("per-OS agent")
}
