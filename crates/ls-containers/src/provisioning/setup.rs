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


/// Consent for the Windows `FunctionalCheck` fix ("Restart WSL and retry").
pub const WSL_RESTART_CONSENT: &str = "This runs `wsl --shutdown`, which stops every running WSL distribution (not just \
    Podman's, so save any work open in them first), then starts Podman's machine again and repeats the test.";

pub fn step_info(step: SetupStep) -> StepInfo {
    info_for(this_os(), step, &on_path)
}

/// `step_info` for any OS, with "which binaries exist" injected so every
/// branch is unit-testable from any host.
fn info_for(os: TargetOs, step: SetupStep, has: &dyn Fn(&str) -> bool) -> StepInfo {
    use SetupStep::*;
    let (title, consent, needs_admin, manual): (&str, Option<String>, bool, String) = match (step, os) {
        (PodmanInstalled, TargetOs::Windows) => (
            "Podman",
            Some("This installs Podman (and Python, if it's missing) using winget, then podman-compose using pip.".into()),
            true,
            "Install Podman from https://podman.io/docs/installation (or run `winget install -e --id RedHat.Podman`), \
             install Python from https://python.org, then run `python -m pip install --user podman-compose` and click Recheck."
                .into(),
        ),
        (PodmanInstalled, TargetOs::Macos) => (
            "Podman",
            Some("This installs Podman and podman-compose using Homebrew.".into()),
            false,
            "Install Homebrew from https://brew.sh, then run `brew install podman podman-compose` in Terminal and click Recheck."
                .into(),
        ),
        (PodmanInstalled, TargetOs::Linux) => {
            let plan = linux_install_plan(has);
            let consent = plan
                .command
                .as_ref()
                .map(|_| format!("This installs Podman and podman-compose using {}.", plan.package_manager.unwrap_or("your package manager")));
            let admin = consent.is_some();
            ("Podman", consent, admin, plan.manual)
        }
        (WslEnabled, _) => (
            "Windows Subsystem for Linux",
            Some("This turns on the Windows Subsystem for Linux, which Podman needs.".into()),
            true,
            "Open PowerShell as administrator, run `wsl --install --no-distribution`, then restart your computer.".into(),
        ),
        (RestartAfterWsl, _) => (
            "Restart",
            None,
            false,
            "Restart your computer so Windows can finish turning on WSL, then open LocalSync again.".into(),
        ),
        (MachineReady, TargetOs::Windows) => (
            "Podman machine",
            Some("This creates and starts Podman's Linux virtual machine, which downloads several hundred MB the first time.".into()),
            false,
            "Run `podman machine init` (first time only), then `podman machine start`, in a terminal. If Podman still \
             can't reach it, run `wsl --shutdown` (this stops all running WSL distributions) and start it again."
                .into(),
        ),
        (MachineReady, _) => (
            "Podman machine",
            Some("This creates and starts Podman's Linux virtual machine, which downloads several hundred MB the first time.".into()),
            false,
            "Run `podman machine init` (first time only), then `podman machine start`, in Terminal.".into(),
        ),
        (FunctionalCheck, os) => (
            "Test container",
            // Windows: the WSL-backed machine can report Running while every
            // container start fails or hangs; a WSL restart clears that.
            (os == TargetOs::Windows).then(|| WSL_RESTART_CONSENT.to_string()),
            false,
            format!(
                "Run `podman run --rm --memory 1g docker.io/library/busybox true` in a terminal; it should finish without \
                 an error. If it fails, the details below show why.{}",
                match os {
                    // The one known cause seen in the field (docs/troubleshooting.md).
                    TargetOs::Windows => " If the error mentions `memory.max`, the WSL 3.0.1 update (kernel 6.18) broke \
                        memory limits for Podman: check `wsl --version`, roll WSL back to a 2.x release from the WSL \
                        GitHub releases page, run `wsl --shutdown`, then Recheck.",
                    TargetOs::Linux => " If the error mentions `memory.max`, your system isn't delegating cgroup memory \
                        control to your user, which rootless Podman needs (cgroup v2 with systemd delegation); see \
                        \"Rootless Podman\" in Podman's troubleshooting guide.",
                    TargetOs::Macos => " If the error mentions the machine, run `podman machine start` and Recheck.",
                }
            ),
        ),
    };
    StepInfo { step, title: title.into(), consent, needs_admin, manual_instructions: manual }
}

/// How `PodmanInstalled` gets fixed on a Linux machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinuxInstallPlan {
    pub package_manager: Option<&'static str>,
    /// argv to run (via pkexec); `None` = manual only.
    pub command: Option<Vec<String>>,
    pub manual: String,
}

/// Pure: picks the package manager (apt-get, dnf, pacman, zypper, in that
/// order) from which binaries exist. Automatic install needs pkexec too
/// (graphical polkit prompt); otherwise the person gets the sudo command.
pub(crate) fn linux_install_plan(has: &dyn Fn(&str) -> bool) -> LinuxInstallPlan {
    const PKGS: &str = "podman podman-compose";
    // (binary, root command run under pkexec, the same thing for a terminal)
    let table: [(&str, String, String); 4] = [
        (
            "apt-get",
            format!("apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y {PKGS}"),
            format!("sudo apt-get update && sudo apt-get install -y {PKGS}"),
        ),
        ("dnf", format!("dnf install -y {PKGS}"), format!("sudo dnf install -y {PKGS}")),
        ("pacman", format!("pacman -S --needed --noconfirm {PKGS}"), format!("sudo pacman -S --needed {PKGS}")),
        ("zypper", format!("zypper --non-interactive install {PKGS}"), format!("sudo zypper install {PKGS}")),
    ];
    let Some((pm, root_cmd, sudo_cmd)) = table.into_iter().find(|(bin, _, _)| has(bin)) else {
        return LinuxInstallPlan {
            package_manager: None,
            command: None,
            manual: "Install the podman and podman-compose packages with your distribution's package manager \
                     (see https://podman.io/docs/installation), then click Recheck."
                .into(),
        };
    };
    let command = has("pkexec").then(|| {
        // apt needs `update && install` under one prompt, hence sh -c.
        if pm == "apt-get" {
            vec!["pkexec".into(), "sh".into(), "-c".into(), root_cmd]
        } else {
            std::iter::once("pkexec").chain(root_cmd.split(' ')).map(String::from).collect()
        }
    });
    LinuxInstallPlan {
        package_manager: Some(pm),
        command,
        manual: format!("Open a terminal and run `{sudo_cmd}`, then click Recheck."),
    }
}

/// Is `bin` an existing file in some PATH directory (no process spawned).
pub(crate) fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

/// `podman machine list --format "{{.Name}}\t{{.Running}}"` -> is the
/// default machine (name marked `*`, else the first) running; `None` = no
/// machine at all.
pub(crate) fn parse_machine_list(stdout: &str) -> Option<bool> {
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    let line = lines.iter().find(|l| l.split('\t').next().is_some_and(|n| n.trim().ends_with('*'))).or(lines.first())?;
    Some(line.split('\t').nth(1).map(str::trim) == Some("true"))
}

/// Windows: pick up PATH entries installers registered since this process
/// started (+ pip's Scripts dir). macOS: Homebrew's bin dir, missing from a
/// Finder-launched app's PATH.
fn prepare_path(log: &ProvisioningLog) {
    #[cfg(windows)]
    super::windows_impl::prepare_path(log);
    #[cfg(target_os = "macos")]
    super::macos_impl::add_homebrew_to_path(log);
    let _ = log;
}

fn not_needed() -> StepCheck {
    StepCheck { ok: true, summary: "This step isn't needed on this computer.".into(), details: String::new() }
}

/// Read-only check of `step`'s real state on this machine. Never changes
/// anything. `FunctionalCheck` runs `crate::readiness::functional_probe`.
/// `RestartAfterWsl` has no check (the caller handles it from the state file).
pub async fn check_step(step: SetupStep, log: &ProvisioningLog) -> StepCheck {
    use SetupStep::*;
    let os = this_os();
    if !steps_for(os).contains(&step) {
        return not_needed();
    }
    prepare_path(log);
    match step {
        PodmanInstalled => check_installed(log).await,
        #[cfg(windows)]
        WslEnabled => super::windows_impl::check_wsl(log).await,
        RestartAfterWsl => StepCheck {
            ok: false,
            summary: "This step is finished by restarting your computer.".into(),
            details: String::new(),
        },
        MachineReady => check_machine(log).await,
        FunctionalCheck => {
            let o = crate::readiness::functional_probe(crate::readiness::PROBE_TIMEOUT).await;
            let summary = match (o.ok, o.summary.is_empty()) {
                (true, _) => "Podman can start containers.".to_string(),
                (false, true) => "Podman couldn't start a test container.".to_string(),
                (false, false) => o.summary,
            };
            StepCheck { ok: o.ok, summary, details: o.details }
        }
        #[allow(unreachable_patterns)]
        _ => not_needed(),
    }
}

/// `bin --version` -> (works, raw output or spawn error).
async fn version_of(log: &ProvisioningLog, bin: &str) -> (bool, String) {
    match super::run_logged(log, bin, &["--version"]).await {
        Ok(o) => (o.status.success(), super::output_text(&o)),
        Err(e) => (false, format!("{e:#}")),
    }
}

async fn check_installed(log: &ProvisioningLog) -> StepCheck {
    let (podman, podman_out) = version_of(log, "podman").await;
    let (compose, compose_out) = version_of(log, "podman-compose").await;
    let summary = match (podman, compose) {
        (true, true) => "Podman and podman-compose are installed.",
        (false, false) => "Podman and podman-compose aren't installed yet.",
        (false, true) => "Podman isn't installed yet.",
        (true, false) => "podman-compose isn't installed yet.",
    };
    StepCheck {
        ok: podman && compose,
        summary: summary.into(),
        details: format!("$ podman --version\n{podman_out}\n\n$ podman-compose --version\n{compose_out}"),
    }
}

async fn check_machine(log: &ProvisioningLog) -> StepCheck {
    let out = super::run_logged(log, "podman", &["machine", "list", "--noheading", "--format", "{{.Name}}\t{{.Running}}"]).await;
    let (ok, summary, details) = match out {
        Err(e) => (false, "Podman isn't installed yet, so its machine can't be checked.", format!("{e:#}")),
        Ok(o) if !o.status.success() => (false, "Podman couldn't list its machines.", super::output_text(&o)),
        Ok(o) => {
            let text = super::output_text(&o);
            match parse_machine_list(&String::from_utf8_lossy(&o.stdout)) {
                None => (false, "Podman's machine hasn't been created yet.", text),
                Some(false) => (false, "Podman's machine exists but isn't running.", text),
                Some(true) => (true, "Podman's machine is running.", text),
            }
        }
    };
    StepCheck { ok, summary: summary.into(), details }
}

/// Performs `step`'s system-changing fix. The caller must have shown
/// `step_info(step).consent` and got the person's explicit OK first. Logs
/// every command and its real output to `log`; errors carry the real output.
pub async fn fix_step(step: SetupStep, log: &ProvisioningLog) -> Result<()> {
    log.info(&format!("setup: fixing {step:?} on {:?}", this_os()));
    prepare_path(log);
    let result = os_fix(step, log).await;
    match &result {
        Ok(()) => {
            log.info(&format!("setup: {step:?} fixed"));
            crate::readiness::invalidate_probe_cache();
        }
        Err(e) => log.error(&format!("setup: fixing {step:?} failed: {e:#}")),
    }
    result
}

fn no_fix(step: SetupStep) -> Result<()> {
    anyhow::bail!("{step:?} has no automatic fix on this computer")
}

#[cfg(windows)]
async fn os_fix(step: SetupStep, log: &ProvisioningLog) -> Result<()> {
    use super::windows_impl as w;
    match step {
        SetupStep::PodmanInstalled => w::fix_podman_installed(log).await,
        SetupStep::WslEnabled => w::fix_wsl(log).await,
        SetupStep::MachineReady => w::fix_machine(log).await,
        SetupStep::FunctionalCheck => w::fix_restart_wsl(log).await,
        _ => no_fix(step),
    }
}

#[cfg(target_os = "macos")]
async fn os_fix(step: SetupStep, log: &ProvisioningLog) -> Result<()> {
    use super::macos_impl as m;
    match step {
        SetupStep::PodmanInstalled => m::fix_podman_installed(log).await,
        SetupStep::MachineReady => m::fix_machine(log).await,
        _ => no_fix(step),
    }
}

#[cfg(target_os = "linux")]
async fn os_fix(step: SetupStep, log: &ProvisioningLog) -> Result<()> {
    match step {
        SetupStep::PodmanInstalled => super::linux_impl::fix_podman_installed(log).await,
        _ => no_fix(step),
    }
}

/// Restart the computer now (Windows only; after the person chose "Restart
/// now").
pub fn restart_computer() -> Result<()> {
    #[cfg(windows)]
    return super::windows_impl::restart_computer();
    #[cfg(not(windows))]
    anyhow::bail!("Restarting from LocalSync is only supported on Windows.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use SetupStep::*;

    const ALL: [SetupStep; 5] = [PodmanInstalled, WslEnabled, RestartAfterWsl, MachineReady, FunctionalCheck];
    const OSES: [TargetOs; 3] = [TargetOs::Windows, TargetOs::Macos, TargetOs::Linux];

    #[test]
    fn steps_per_os() {
        assert_eq!(steps_for(TargetOs::Windows), vec![PodmanInstalled, WslEnabled, RestartAfterWsl, MachineReady, FunctionalCheck]);
        assert_eq!(steps_for(TargetOs::Macos), vec![PodmanInstalled, MachineReady, FunctionalCheck]);
        assert_eq!(steps_for(TargetOs::Linux), vec![PodmanInstalled, FunctionalCheck]);
    }

    #[test]
    fn step_info_complete_and_consent_none_exactly_where_expected() {
        let apt_pkexec = |b: &str| b == "apt-get" || b == "pkexec";
        let nothing = |_: &str| false;
        for os in OSES {
            for step in ALL {
                for (has, has_pm) in [(&apt_pkexec as &dyn Fn(&str) -> bool, true), (&nothing, false)] {
                    let i = info_for(os, step, has);
                    assert_eq!(i.step, step);
                    assert!(!i.title.is_empty() && !i.manual_instructions.is_empty(), "{os:?} {step:?}");
                    let expect_none = step == RestartAfterWsl
                        || (step == FunctionalCheck && os != TargetOs::Windows)
                        || (os == TargetOs::Linux && step == PodmanInstalled && !has_pm);
                    assert_eq!(i.consent.is_none(), expect_none, "{os:?} {step:?} has_pm={has_pm}");
                    if let Some(c) = &i.consent {
                        assert!(c.ends_with('.') && c.matches(". ").count() == 0, "one sentence: {c}");
                    }
                }
            }
        }
        assert!(info_for(TargetOs::Windows, WslEnabled, &nothing).needs_admin);
        assert!(info_for(TargetOs::Windows, PodmanInstalled, &nothing).needs_admin);
        assert!(!info_for(TargetOs::Macos, PodmanInstalled, &nothing).needs_admin);
        assert!(info_for(TargetOs::Linux, PodmanInstalled, &apt_pkexec).needs_admin);
        assert!(!info_for(TargetOs::Linux, PodmanInstalled, &nothing).needs_admin);
        // "Restart WSL and retry": Windows only, warns that it stops every distro.
        let win = info_for(TargetOs::Windows, FunctionalCheck, &nothing);
        assert_eq!(win.consent.as_deref(), Some(WSL_RESTART_CONSENT));
        assert!(!win.needs_admin);
        assert!(WSL_RESTART_CONSENT.contains("wsl --shutdown") && WSL_RESTART_CONSENT.contains("every running WSL distribution"));
        assert!(!WSL_RESTART_CONSENT.contains("  "), "{WSL_RESTART_CONSENT}");
    }

    fn plan(bins: &[&str]) -> LinuxInstallPlan {
        let bins: Vec<String> = bins.iter().map(|s| s.to_string()).collect();
        linux_install_plan(&move |b: &str| bins.iter().any(|x| x == b))
    }

    fn argv(v: &[&str]) -> Option<Vec<String>> {
        Some(v.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn linux_package_manager_detection() {
        let p = plan(&["apt-get", "pkexec", "dnf"]);
        assert_eq!(p.package_manager, Some("apt-get"));
        assert_eq!(
            p.command,
            argv(&["pkexec", "sh", "-c", "apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y podman podman-compose"])
        );
        assert!(p.manual.contains("`sudo apt-get update && sudo apt-get install -y podman podman-compose`"));

        let p = plan(&["dnf", "pkexec"]);
        assert_eq!(p.command, argv(&["pkexec", "dnf", "install", "-y", "podman", "podman-compose"]));
        assert!(p.manual.contains("`sudo dnf install -y podman podman-compose`"));

        let p = plan(&["pacman", "pkexec"]);
        assert_eq!(p.command, argv(&["pkexec", "pacman", "-S", "--needed", "--noconfirm", "podman", "podman-compose"]));
        assert!(p.manual.contains("`sudo pacman -S --needed podman podman-compose`"));

        let p = plan(&["zypper", "pkexec"]);
        assert_eq!(p.command, argv(&["pkexec", "zypper", "--non-interactive", "install", "podman", "podman-compose"]));
        assert!(p.manual.contains("`sudo zypper install podman podman-compose`"));

        // Package manager but no pkexec: manual sudo command only.
        let p = plan(&["dnf"]);
        assert_eq!((p.package_manager, p.command), (Some("dnf"), None));
        assert!(p.manual.contains("`sudo dnf install -y podman podman-compose`"));

        // Nothing known.
        let p = plan(&["pkexec"]);
        assert_eq!((p.package_manager, p.command.clone()), (None, None));
        assert!(p.manual.contains("podman-compose"));
    }

    #[test]
    fn machine_list_parsing() {
        assert_eq!(parse_machine_list(""), None);
        assert_eq!(parse_machine_list("podman-machine-default*\ttrue\n"), Some(true));
        assert_eq!(parse_machine_list("podman-machine-default\tfalse\n"), Some(false));
        // The default (`*`) machine decides, not the first line.
        assert_eq!(parse_machine_list("other\ttrue\nmain*\tfalse\n"), Some(false));
    }

    /// Tauri commands need `Send` futures.
    #[allow(dead_code)]
    fn futures_are_send(log: &'static ProvisioningLog) {
        fn assert_send<T: Send>(_: T) {}
        assert_send(check_step(PodmanInstalled, log));
        assert_send(fix_step(PodmanInstalled, log));
    }

    #[tokio::test]
    async fn fixes_without_automation_error() {
        let dir = tempfile::tempdir().unwrap();
        let log = ProvisioningLog::open_in(dir.path()).unwrap();
        #[cfg(not(windows))] // on Windows this is the real `wsl --shutdown` fix
        assert!(fix_step(FunctionalCheck, &log).await.is_err());
        assert!(fix_step(RestartAfterWsl, &log).await.is_err());
        #[cfg(not(windows))]
        assert!(restart_computer().is_err());
    }

    /// Real checks against this machine's Podman (not run by default:
    /// needs podman + podman-compose installed and, on Windows/macOS, a
    /// running machine). `cargo test -p ls-containers real_setup_checks -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn real_setup_checks() {
        let dir = tempfile::tempdir().unwrap();
        let log = ProvisioningLog::open_in(dir.path()).unwrap();
        for step in steps_for(this_os()) {
            if matches!(step, RestartAfterWsl | FunctionalCheck) {
                continue;
            }
            let c = check_step(step, &log).await;
            println!("{step:?}: {c:?}");
            assert!(c.ok, "{step:?}: {c:?}");
        }
        #[cfg(target_os = "linux")]
        println!("linux plan: {:?}", linux_install_plan(&on_path));
    }

    /// FunctionalCheck against the real readiness probe.
    #[tokio::test]
    #[ignore]
    async fn real_functional_check() {
        let dir = tempfile::tempdir().unwrap();
        let log = ProvisioningLog::open_in(dir.path()).unwrap();
        let c = check_step(FunctionalCheck, &log).await;
        println!("{c:?}");
        // Either outcome is real; a failure must explain itself.
        assert!(c.ok || (!c.summary.is_empty() && !c.details.is_empty()), "{c:?}");
    }

    /// `MachineReady`'s fix on a machine whose podman machine already exists
    /// and runs: must be a no-op that succeeds.
    #[tokio::test]
    #[ignore]
    async fn real_machine_fix_is_noop_when_running() {
        let dir = tempfile::tempdir().unwrap();
        let log = ProvisioningLog::open_in(dir.path()).unwrap();
        let r = fix_step(MachineReady, &log).await;
        println!("{}", std::fs::read_to_string(log.path()).unwrap());
        assert!(r.is_ok(), "{r:?}");
    }
}

#[cfg(test)]
mod manual_text_tests {
    use super::*;

    /// The container check's do-it-yourself text names the known cause per
    /// OS, reads as normal sentences (no stray runs of spaces from line
    /// continuations), and is never empty.
    #[test]
    fn functional_check_manual_instructions_name_the_known_cause_per_os() {
        let none = |_: &str| false;
        let win = info_for(TargetOs::Windows, SetupStep::FunctionalCheck, &none).manual_instructions;
        assert!(win.contains("WSL 3.0.1") && win.contains("2.x") && win.contains("Recheck"), "{win}");
        let linux = info_for(TargetOs::Linux, SetupStep::FunctionalCheck, &none).manual_instructions;
        assert!(linux.contains("memory.max") && linux.contains("Rootless Podman"), "{linux}");
        let mac = info_for(TargetOs::Macos, SetupStep::FunctionalCheck, &none).manual_instructions;
        assert!(mac.contains("podman machine start"), "{mac}");
        for text in [&win, &linux, &mac] {
            assert!(!text.contains("  "), "double spaces: {text}");
        }
    }
}
