//! The dependency-setup wizard's resume/verify/fix/restart flow, driven with a
//! fake `SetupOps` and a tempdir state file (no real system changes).

use std::collections::HashSet;
use std::sync::Mutex;

use localsync_desktop::setup_commands;
use localsync_desktop::setup_state::{self, SetupOps, SetupState};
use ls_containers::setup::{steps_for, SetupStep, StepCheck, TargetOs};
use tauri::Listener;

use SetupStep::*;

#[derive(Default)]
struct Fake {
    passing: Mutex<HashSet<SetupStep>>,
    checked: Mutex<Vec<SetupStep>>,
    fixed: Mutex<Vec<SetupStep>>,
}

impl Fake {
    fn passing(steps: &[SetupStep]) -> Self {
        let f = Fake::default();
        f.passing.lock().unwrap().extend(steps.iter().copied());
        f
    }
    fn checked(&self) -> Vec<SetupStep> {
        self.checked.lock().unwrap().clone()
    }
    fn fixed(&self) -> Vec<SetupStep> {
        self.fixed.lock().unwrap().clone()
    }
}

impl SetupOps for Fake {
    async fn check(&self, step: SetupStep) -> StepCheck {
        self.checked.lock().unwrap().push(step);
        let ok = self.passing.lock().unwrap().contains(&step);
        StepCheck { ok, summary: format!("{step:?} ok={ok}"), details: String::new() }
    }
    async fn fix(&self, step: SetupStep) -> Result<(), String> {
        self.fixed.lock().unwrap().push(step);
        self.passing.lock().unwrap().insert(step);
        Ok(())
    }
}

async fn verify(dir: &std::path::Path, os: TargetOs, ops: &Fake) -> (SetupState, Option<setup_state::Failure>) {
    setup_state::verify_from(dir, os, None, ops, |_| {}).await.unwrap()
}

#[tokio::test]
async fn relaunch_mid_setup_reverifies_done_steps_and_resumes_at_the_next() {
    let dir = tempfile::tempdir().unwrap();
    // A process that died after finishing steps 1-2 (macOS: Podman, machine).
    setup_state::save_in(dir.path(), &SetupState { done: vec![PodmanInstalled, MachineReady], restart_required: false })
        .unwrap();
    let ops = Fake::passing(&[PodmanInstalled, MachineReady]);
    let (state, failure) = verify(dir.path(), TargetOs::Macos, &ops).await;
    assert_eq!(ops.checked(), vec![PodmanInstalled, MachineReady, FunctionalCheck]);
    assert!(ops.fixed().is_empty(), "verify never fixes");
    assert_eq!(state.first_undone(TargetOs::Macos), Some(FunctionalCheck));
    assert_eq!(failure.unwrap().step, FunctionalCheck);
    assert_eq!(setup_state::load_in(dir.path()), state, "persisted");
}

#[tokio::test]
async fn a_stale_done_that_no_longer_checks_out_is_reset() {
    let dir = tempfile::tempdir().unwrap();
    setup_state::save_in(dir.path(), &SetupState { done: vec![PodmanInstalled, MachineReady], restart_required: false })
        .unwrap();
    let ops = Fake::passing(&[PodmanInstalled]); // the machine was removed since
    let (state, failure) = verify(dir.path(), TargetOs::Macos, &ops).await;
    assert_eq!(ops.checked(), vec![PodmanInstalled, MachineReady], "stops at the failure");
    assert!(!state.is_done(MachineReady));
    assert_eq!(state.first_undone(TargetOs::Macos), Some(MachineReady));
    assert_eq!(failure.unwrap().step, MachineReady);
    assert!(!setup_state::load_in(dir.path()).is_done(MachineReady));
}

#[tokio::test]
async fn a_failed_step_retried_after_a_fix_continues_without_refixing_earlier_steps() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Fake::passing(&[PodmanInstalled, FunctionalCheck]);
    let (_, failure) = verify(dir.path(), TargetOs::Macos, &ops).await;
    assert_eq!(failure.unwrap().step, MachineReady);

    let (state, failure) =
        setup_state::fix_then_verify(dir.path(), TargetOs::Macos, MachineReady, true, &ops, |_| {}).await.unwrap();
    assert_eq!(ops.fixed(), vec![MachineReady], "only the failing step is fixed");
    assert!(failure.is_none());
    assert_eq!(state.first_undone(TargetOs::Macos), None, "all done");
    // Continued from MachineReady, not from the start.
    assert_eq!(ops.checked(), vec![PodmanInstalled, MachineReady, MachineReady, FunctionalCheck]);
}

#[tokio::test]
async fn fix_without_consent_is_refused_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Fake::default();
    let err = setup_state::fix_then_verify(dir.path(), TargetOs::Linux, PodmanInstalled, false, &ops, |_| {}).await;
    assert!(err.is_err());
    assert!(ops.fixed().is_empty());
    assert!(!setup_state::load_in(dir.path()).any_progress());
}

#[tokio::test]
async fn enabling_wsl_requires_a_restart_and_clearing_it_resumes_at_machine_ready() {
    let dir = tempfile::tempdir().unwrap();
    let os = TargetOs::Windows;
    let ops = Fake::passing(&[PodmanInstalled]);
    let (_, failure) = verify(dir.path(), os, &ops).await;
    assert_eq!(failure.unwrap().step, WslEnabled);

    let mut events = Vec::new();
    let (state, failure) =
        setup_state::fix_then_verify(dir.path(), os, WslEnabled, true, &ops, |e| events.push(e)).await.unwrap();
    assert!(state.restart_required);
    assert!(failure.is_none(), "a pending restart is not a failure");
    assert_eq!(state.first_undone(os), Some(RestartAfterWsl));
    assert_eq!(events.last().map(|e| (e.step, e.status)), Some((RestartAfterWsl, "pending")));

    // Killed before choosing: relaunch still stops at the restart (WSL not
    // re-checked - it isn't active until the restart).
    let checks_before = ops.checked().len();
    let (state, _) = verify(dir.path(), os, &ops).await;
    assert_eq!(state.first_undone(os), Some(RestartAfterWsl));
    assert_eq!(ops.checked().len(), checks_before + 1, "only Podman re-checked");

    // "Exit and restart later" (pure part): clear + persist.
    setup_state::clear_restart_in(dir.path()).unwrap();
    let reloaded = setup_state::load_in(dir.path());
    assert!(!reloaded.restart_required);
    assert_eq!(reloaded.first_undone(os), Some(MachineReady));
}

#[tokio::test]
async fn wsl_already_enabled_needs_no_restart() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Fake::passing(&[PodmanInstalled, WslEnabled]);
    let (state, failure) = verify(dir.path(), TargetOs::Windows, &ops).await;
    assert!(!state.restart_required);
    assert!(state.is_done(RestartAfterWsl));
    assert_eq!(failure.unwrap().step, MachineReady);
    assert!(ops.fixed().is_empty());
}

#[test]
fn corrupt_file_is_no_progress_and_state_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("setup-state.json"), b"\x00garbage").unwrap();
    assert!(!setup_state::load_in(dir.path()).any_progress());
    let s = SetupState { done: vec![PodmanInstalled, WslEnabled], restart_required: true };
    setup_state::save_in(dir.path(), &s).unwrap();
    assert_eq!(setup_state::load_in(dir.path()), s);
}

#[test]
fn linux_has_no_wsl_restart_or_machine_steps() {
    assert_eq!(steps_for(TargetOs::Linux), vec![PodmanInstalled, FunctionalCheck]);
}

#[test]
fn stale_flags_past_the_first_undone_step_are_not_reported_done() {
    let s = SetupState { done: vec![PodmanInstalled, FunctionalCheck], restart_required: false };
    assert!(s.reported_done(TargetOs::Macos, PodmanInstalled));
    assert!(!s.reported_done(TargetOs::Macos, FunctionalCheck));
}

#[tokio::test]
async fn verify_emits_checking_then_done_or_failed_in_order() {
    let app = tauri::test::mock_app();
    let handle = app.handle().clone();
    let seen = std::sync::Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let sink = seen.clone();
    handle.listen("setup-step", move |e| sink.lock().unwrap().push(serde_json::from_str(e.payload()).unwrap()));

    let dir = tempfile::tempdir().unwrap();
    let ops = Fake::passing(&[PodmanInstalled]);
    setup_commands::verify_emitting(&handle, dir.path(), TargetOs::Macos, &ops).await.unwrap();

    let got: Vec<(String, String)> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|v| (v["step"].as_str().unwrap().to_string(), v["status"].as_str().unwrap().to_string()))
        .collect();
    let want = [
        ("podman_installed", "checking"),
        ("podman_installed", "done"),
        ("machine_ready", "checking"),
        ("machine_ready", "failed"),
    ];
    assert_eq!(got, want.map(|(a, b)| (a.to_string(), b.to_string())));
}
