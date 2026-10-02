//! Tauri commands for the dependency-setup wizard. State/orchestration lives
//! in `setup_state.rs`; this is the IPC surface and event streaming.
//!
//! Events: `setup-step` ([`StepEvent`]) while verifying, `setup-progress`
//! ([`SetupProgress`]) with the provisioning log's lines while a fix runs.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ls_containers::setup::{self, SetupStep, TargetOs};
use ls_containers::ProvisioningLog;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::setup_state::{self, Failure, RealOps, SetupOps, SetupState, StepEvent};

#[derive(Debug, Clone, Serialize)]
pub struct StepView {
    pub step: SetupStep,
    pub title: String,
    /// "not_started" | "done"
    pub status: &'static str,
    pub consent: Option<String>,
    pub needs_admin: bool,
    pub manual_instructions: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupView {
    pub os: TargetOs,
    pub steps: Vec<StepView>,
    pub any_progress: bool,
    pub all_done: bool,
    pub restart_required: bool,
}

/// `SetupView`'s fields plus the outcome of a verify.
#[derive(Debug, Clone, Serialize)]
pub struct SetupVerifyView {
    #[serde(flatten)]
    pub view: SetupView,
    pub first_undone: Option<SetupStep>,
    pub failure: Option<Failure>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupProgress {
    pub step: SetupStep,
    pub line: String,
}

pub fn view(os: TargetOs, state: &SetupState) -> SetupView {
    let steps = setup::steps_for(os)
        .into_iter()
        .map(|step| {
            let info = setup::step_info(step);
            StepView {
                step,
                title: info.title,
                status: if state.reported_done(os, step) { "done" } else { "not_started" },
                consent: info.consent,
                needs_admin: info.needs_admin,
                manual_instructions: info.manual_instructions,
            }
        })
        .collect();
    SetupView {
        os,
        steps,
        any_progress: state.any_progress(),
        all_done: state.first_undone(os).is_none(),
        restart_required: state.restart_required,
    }
}

fn verify_view(os: TargetOs, state: &SetupState, failure: Option<Failure>) -> SetupVerifyView {
    SetupVerifyView { view: view(os, state), first_undone: state.first_undone(os), failure }
}

fn emitter<R: tauri::Runtime>(app: &AppHandle<R>) -> impl FnMut(StepEvent) + '_ {
    move |e| {
        let _ = app.emit("setup-step", e);
    }
}

/// [`setup_state::verify_from`] from the first step, emitting `setup-step`
/// events. Split out of the command so tests can drive it with a fake.
pub async fn verify_emitting<R: tauri::Runtime>(
    app: &AppHandle<R>,
    dir: &Path,
    os: TargetOs,
    ops: &impl SetupOps,
) -> Result<(SetupState, Option<Failure>), String> {
    setup_state::verify_from(dir, os, None, ops, emitter(app)).await
}

/// Instant, file-only: what the wizard shows on open, before any check.
#[tauri::command]
pub fn setup_state() -> Result<SetupView, String> {
    Ok(view(setup::this_os(), &setup_state::load_in(&setup_state::default_dir()?)))
}

#[tauri::command]
pub async fn setup_verify<R: tauri::Runtime>(app: AppHandle<R>) -> Result<SetupVerifyView, String> {
    let os = setup::this_os();
    let ops = RealOps(ProvisioningLog::open_default().map_err(|e| e.to_string())?);
    let (state, failure) = verify_emitting(&app, &setup_state::default_dir()?, os, &ops).await?;
    Ok(verify_view(os, &state, failure))
}

#[tauri::command]
pub async fn setup_fix<R: tauri::Runtime>(
    app: AppHandle<R>,
    step: SetupStep,
    confirmed: bool,
) -> Result<SetupVerifyView, String> {
    if !confirmed {
        return Err("This change needs your OK first - nothing was changed.".to_string());
    }
    let os = setup::this_os();
    let dir = setup_state::default_dir()?;
    let log = ProvisioningLog::open_default().map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    // Offset taken now, before the fix starts, so none of its lines are missed.
    let offset = std::fs::metadata(log.path()).map(|m| m.len()).unwrap_or(0);
    let tail = tauri::async_runtime::spawn(tail_log(app.clone(), log.path().to_path_buf(), offset, step, stop.clone()));
    let ops = RealOps(log);
    let result = setup_state::fix_then_verify(&dir, os, step, confirmed, &ops, emitter(&app)).await;
    stop.store(true, Ordering::Relaxed);
    let _ = tail.await;
    let (state, failure) = result?;
    Ok(verify_view(os, &state, failure))
}

/// "Restart now" (`now`) or "Exit and restart later": either way the restart
/// step is recorded as done first, so the next launch resumes after it.
#[tauri::command]
pub fn setup_restart<R: tauri::Runtime>(app: AppHandle<R>, now: bool) -> Result<(), String> {
    setup_state::clear_restart_in(&setup_state::default_dir()?)?;
    if now {
        setup::restart_computer().map_err(|e| format!("{e:#}"))
    } else {
        app.exit(0);
        Ok(())
    }
}

/// Streams lines appended to the provisioning log as `setup-progress` until
/// `stop`, then flushes what's left. Same polling approach as
/// `commands::tail_provisioning_log` (private there, and it emits a
/// different event).
async fn tail_log<R: tauri::Runtime>(
    app: AppHandle<R>,
    path: PathBuf,
    mut offset: u64,
    step: SetupStep,
    stop: Arc<AtomicBool>,
) {
    loop {
        let last = stop.load(Ordering::Relaxed);
        if let Ok(contents) = tokio::fs::read(&path).await {
            if let Some(new_bytes) = contents.get(offset as usize..) {
                if let Some(nl) = new_bytes.iter().rposition(|&b| b == b'\n') {
                    for line in String::from_utf8_lossy(&new_bytes[..=nl]).lines() {
                        let _ = app.emit("setup-progress", SetupProgress { step, line: line.to_string() });
                    }
                    offset += (nl + 1) as u64;
                }
            }
        }
        if last {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}
