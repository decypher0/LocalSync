//! The dependency-setup wizard's progress: which steps are done, persisted
//! across restarts in `{OS data dir}/localsync/setup-state.json` (same
//! convention as `session_history.rs`), plus the verify/fix orchestration
//! that walks `ls_containers::setup::steps_for(os)` in order.
//!
//! A "done" flag is never trusted blindly: [`verify_from`] re-runs each
//! step's read-only check and resets a step that no longer checks out. Status
//! is reported as a prefix - a step only shows as done when every step before
//! it is done too, so stale flags past the first undone step are kept in the
//! file but never reported.

use std::future::Future;
use std::path::{Path, PathBuf};

use ls_containers::setup::{FixProgress, SetupStep, StepCheck, TargetOs};
use ls_containers::ProvisioningLog;
use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "setup-state.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SetupState {
    /// Steps whose status is "done"; every other step is "not_started".
    #[serde(default)]
    pub done: Vec<SetupStep>,
    /// Set when the app itself enabled WSL; cleared once the person chose
    /// "Restart now" or "Exit and restart later".
    #[serde(default)]
    pub restart_required: bool,
}

impl SetupState {
    pub fn is_done(&self, step: SetupStep) -> bool {
        self.done.contains(&step)
    }
    fn set_done(&mut self, step: SetupStep, done: bool) {
        self.done.retain(|s| *s != step);
        if done {
            self.done.push(step);
        }
    }
    /// The first step (in `os` order) that isn't done - where setup resumes.
    pub fn first_undone(&self, os: TargetOs) -> Option<SetupStep> {
        ls_containers::setup::steps_for(os).into_iter().find(|s| !self.is_done(*s))
    }
    /// Status as reported: done only if every earlier step is done too.
    pub fn reported_done(&self, os: TargetOs, step: SetupStep) -> bool {
        let steps = ls_containers::setup::steps_for(os);
        let idx = steps.iter().position(|s| *s == step);
        let stop = self.first_undone(os).and_then(|f| steps.iter().position(|s| *s == f));
        match (idx, stop) {
            (Some(i), Some(stop)) => i < stop,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }
    pub fn any_progress(&self) -> bool {
        !self.done.is_empty() || self.restart_required
    }
}

pub fn default_dir() -> Result<PathBuf, String> {
    let base = dirs::data_dir().ok_or("could not determine the OS data directory")?;
    Ok(base.join("localsync"))
}

/// Missing or unreadable/corrupt file = no progress (setup simply re-checks
/// everything), never an error.
pub fn load_in(dir: &Path) -> SetupState {
    std::fs::read(dir.join(FILE_NAME))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_in(dir: &Path, state: &SetupState) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let path = dir.join(FILE_NAME);
    let bytes = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
    std::fs::write(&path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))
}

/// The person chose "Restart now" or "Exit and restart later": the restart
/// step is done, persisted before the app restarts/exits so the next launch
/// resumes at the step after it.
pub fn clear_restart_in(dir: &Path) -> Result<(), String> {
    let mut state = load_in(dir);
    state.restart_required = false;
    state.set_done(SetupStep::RestartAfterWsl, true);
    save_in(dir, &state)
}

/// Check/fix one step. Real impl: [`RealOps`]; tests use a fake.
pub trait SetupOps {
    fn check(&self, step: SetupStep) -> impl Future<Output = StepCheck> + Send;
    fn fix(&self, step: SetupStep) -> impl Future<Output = Result<(), String>> + Send;
}

/// The log, and where a fix's per-step progress goes (`setup-fix-progress`).
pub struct RealOps(pub ProvisioningLog, pub Box<dyn Fn(FixProgress) + Send + Sync>);

impl SetupOps for RealOps {
    fn check(&self, step: SetupStep) -> impl Future<Output = StepCheck> + Send {
        ls_containers::setup::check_step(step, &self.0)
    }
    fn fix(&self, step: SetupStep) -> impl Future<Output = Result<(), String>> + Send {
        async move { ls_containers::setup::fix_step(step, &self.0, &*self.1).await.map_err(|e| format!("{e:#}")) }
    }
}

/// Payload of the `setup-step` event.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StepEvent {
    pub step: SetupStep,
    /// "checking" | "done" | "failed" | "pending"
    pub status: &'static str,
    pub summary: String,
    pub details: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Failure {
    pub step: SetupStep,
    pub summary: String,
    pub details: String,
}

fn event(step: SetupStep, status: &'static str, summary: &str, details: &str) -> StepEvent {
    StepEvent { step, status, summary: summary.to_string(), details: details.to_string() }
}

/// Re-verifies steps in order starting at `start` (the first step when
/// `None`), persisting after every change. Stops at the first step that
/// doesn't check out (reset to not_started, returned as the failure) or at a
/// restart that is still required. Never fixes anything.
pub async fn verify_from(
    dir: &Path,
    os: TargetOs,
    start: Option<SetupStep>,
    ops: &impl SetupOps,
    mut emit: impl FnMut(StepEvent),
) -> Result<(SetupState, Option<Failure>), String> {
    let mut state = load_in(dir);
    let steps = ls_containers::setup::steps_for(os);
    let from = start.and_then(|s| steps.iter().position(|x| *x == s)).unwrap_or(0);
    for &step in &steps[from..] {
        if step == SetupStep::RestartAfterWsl {
            if state.restart_required {
                state.set_done(step, false);
                save_in(dir, &state)?;
                emit(event(step, "pending", "Restart your computer to finish turning on WSL.", ""));
                return Ok((state, None));
            }
            state.set_done(step, true);
            save_in(dir, &state)?;
            emit(event(step, "done", "No restart needed.", ""));
            continue;
        }
        if step == SetupStep::WslEnabled && state.restart_required {
            // The app enabled WSL and the restart that activates it hasn't
            // happened yet - checking now would report it as off.
            state.set_done(step, true);
            save_in(dir, &state)?;
            emit(event(step, "done", "WSL was turned on; a restart finishes it.", ""));
            continue;
        }
        emit(event(step, "checking", "", ""));
        let check = ops.check(step).await;
        state.set_done(step, check.ok);
        save_in(dir, &state)?;
        if check.ok {
            emit(event(step, "done", &check.summary, &check.details));
        } else {
            emit(event(step, "failed", &check.summary, &check.details));
            return Ok((state, Some(Failure { step, summary: check.summary, details: check.details })));
        }
    }
    Ok((state, None))
}

/// Runs `step`'s fix (only with the person's explicit consent), then
/// continues verifying from that step.
pub async fn fix_then_verify(
    dir: &Path,
    os: TargetOs,
    step: SetupStep,
    confirmed: bool,
    ops: &impl SetupOps,
    emit: impl FnMut(StepEvent),
) -> Result<(SetupState, Option<Failure>), String> {
    if !confirmed {
        return Err("This change needs your OK first - nothing was changed.".to_string());
    }
    ops.fix(step).await?;
    let mut start = Some(step);
    if step == SetupStep::WslEnabled {
        // Enabling WSL always prompts a restart; save that before anything
        // else so a crash here still resumes at the restart step.
        let mut state = load_in(dir);
        state.restart_required = true;
        state.set_done(SetupStep::WslEnabled, true);
        state.set_done(SetupStep::RestartAfterWsl, false);
        save_in(dir, &state)?;
        start = Some(SetupStep::RestartAfterWsl);
    }
    verify_from(dir, os, start, ops, emit).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_state_file_round_trips_and_missing_or_corrupt_means_no_progress() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_in(dir.path()), SetupState::default());
        let state = SetupState { done: vec![SetupStep::PodmanInstalled], restart_required: true };
        save_in(dir.path(), &state).unwrap();
        assert_eq!(load_in(dir.path()), state);
        std::fs::write(dir.path().join(FILE_NAME), b"{not json").unwrap();
        assert!(!load_in(dir.path()).any_progress());
    }
}
