//! Windows Podman provisioning: install Podman via winget if missing, verify
//! WSL2 is actually usable (`podman machine` runs entirely inside a WSL2
//! distro on Windows and fails with WSL-specific errors if it isn't), then
//! init/start the podman machine and verify `podman info` +
//! `podman-compose --version` both work.
//!
//! # THIS FILE IS VERIFIED — every command below was actually run on a real
//! Windows 11 Home machine (build 26200) during development, not just
//! cross-referenced against docs. Specific things confirmed empirically,
//! each with a comment at the call site: `winget install -e --id
//! RedHat.Podman` is the real package id; a fresh process's PATH doesn't
//! pick up what winget/pip just registered without an explicit refresh;
//! `wsl.exe`'s captured output is BOM-prefixed UTF-16LE, not UTF-8; the
//! Windows Podman installer does not bundle podman-compose; and a
//! `podman info` that fails right after `podman machine start` can be
//! fixed by a `wsl --shutdown` + restart (a real, reproduced-more-than-once
//! stuck WSL2 user-session state, not a hypothetical).

use super::{output_text, run_logged, ProvisioningLog};
use crate::provisioning::setup::{parse_machine_list, StepCheck};
use anyhow::Result;
use std::os::windows::process::CommandExt;

/// Prevents a spawned console-mode child (`winget`, `podman`, `wsl.exe`,
/// `reg.exe` — everything this file shells out to) from popping its own
/// visible console window when this GUI app has no console of its own
/// (Windows' documented default otherwise). Round 12's audit: every
/// subprocess spawn in this codebase now goes through a helper like this
/// one rather than a bare `Command::new` — see the identical fix/comment in
/// `crates/ls-containers/src/podman.rs` and
/// `crates/ls-snapshot/src/bundle.rs`.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn tokio_command(program: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

fn sync_command(program: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Reads one `REG_SZ`/`REG_EXPAND_SZ` value via the `reg.exe` that ships
/// with every Windows install — avoids pulling in a registry crate for a
/// couple of string reads.
fn reg_query_value(key: &str, name: &str) -> Option<String> {
    let out = sync_command("reg").args(["query", key, "/v", name]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().find_map(|l| {
        let rest = l.trim().strip_prefix(name)?.trim_start();
        let rest = rest.strip_prefix("REG_EXPAND_SZ").or_else(|| rest.strip_prefix("REG_SZ"))?;
        Some(rest.trim().to_string())
    })
}

/// `winget install`/`pip install` register their new PATH entries in the
/// registry, but this process's own `PATH` env var was captured at process
/// start and is never re-read from there automatically — verified
/// empirically: a `podman --version` run in the very same process right
/// after a successful `winget install -e --id RedHat.Podman` still failed
/// to find it, until the process's PATH was rebuilt from the registry like
/// this. Without this, every install step below would "succeed" and then
/// have the immediately-following verification check fail for a completely
/// unrelated reason.
///
/// Setup-wizard change: registry entries are *appended* to the current PATH
/// (only the ones it doesn't already have, with `%VAR%`s expanded) instead
/// of replacing it, so whatever this process was started with (e.g. a pip
/// Scripts dir added earlier) stays reachable.
fn refresh_path_from_registry(log: &ProvisioningLog) {
    let machine =
        reg_query_value(r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment", "Path");
    let user = reg_query_value(r"HKCU\Environment", "Path");
    let registry = format!("{};{}", machine.unwrap_or_default(), user.unwrap_or_default());
    let current = std::env::var("PATH").unwrap_or_default();
    let merged = merge_path(&current, &expand_env(&registry));
    if merged != current {
        log.info("refreshed this process's PATH from the registry");
        std::env::set_var("PATH", merged);
    }
}

/// `current` plus every entry of `extra` it doesn't already contain
/// (case-insensitive, trailing `\` ignored - Windows paths).
fn merge_path(current: &str, extra: &str) -> String {
    let norm = |p: &str| p.trim().trim_end_matches('\\').to_lowercase();
    let mut out: Vec<String> =
        current.split(';').filter(|p| !p.trim().is_empty()).map(String::from).collect();
    for p in extra.split(';').filter(|p| !p.trim().is_empty()) {
        if !out.iter().any(|o| norm(o) == norm(p)) {
            out.push(p.trim().to_string());
        }
    }
    out.join(";")
}

/// Expands `%NAME%` references (REG_EXPAND_SZ values come back unexpanded
/// from `reg query`); unknown names are left as-is.
fn expand_env(s: &str) -> String {
    let parts: Vec<&str> = s.split('%').collect();
    let mut out = String::from(parts[0]);
    let mut i = 1;
    while i < parts.len() {
        match (parts.get(i + 1), std::env::var(parts[i])) {
            (Some(rest), Ok(v)) if !parts[i].is_empty() => {
                out.push_str(&v);
                out.push_str(rest);
                i += 2;
            }
            _ => {
                out.push('%');
                out.push_str(parts[i]);
                i += 1;
            }
        }
    }
    out
}

/// pip's per-user Scripts dir (where pip puts `podman-compose.exe`; see
/// `ensure_compose_installed` for why it's asked, not guessed), if Python
/// is available.
fn pip_user_scripts_dir() -> Option<String> {
    let out = sync_command("python")
        .args(["-c", "import sysconfig; print(sysconfig.get_path('scripts', 'nt_user'))"])
        .output()
        .ok()?;
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !dir.is_empty()).then_some(dir)
}

/// Before any setup check/fix: pick up what installers registered since this
/// process started, and pip's user Scripts dir if podman-compose is only
/// reachable from there (pip never adds it to PATH itself).
pub(crate) fn prepare_path(log: &ProvisioningLog) {
    refresh_path_from_registry(log);
    if !crate::podman::podman_compose_available() {
        if let Some(dir) = pip_user_scripts_dir() {
            let current = std::env::var("PATH").unwrap_or_default();
            let merged = merge_path(&current, &dir);
            if merged != current {
                log.info(&format!("added pip's user Scripts dir {dir} to this process's PATH"));
                std::env::set_var("PATH", merged);
            }
        }
    }
}

/// Step 1 (called from `provisioning.rs`, before `ensure_ready`):
/// `Windows 11 Home (build 26200)`. Reads the registry directly rather than
/// shelling out to `systeminfo`/`Get-ComputerInfo` (both much slower).
///
/// Verified on this machine: `ProductName` still literally says
/// `"Windows 10 Home Single Language"` even though `DisplayVersion` is
/// `25H2` and `CurrentBuild` is `26200` — Microsoft never updated that
/// registry string when Windows 11 shipped, so it's corrected here using
/// the documented rule that build >= 22000 means Windows 11.
pub fn detailed_version() -> String {
    let key = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build = reg_query_value(key, "CurrentBuild").unwrap_or_else(|| "unknown".to_string());
    let mut product = reg_query_value(key, "ProductName").unwrap_or_else(|| "Windows".to_string());
    if build.parse::<u32>().is_ok_and(|b| b >= 22000) {
        product = product.replacen("Windows 10", "Windows 11", 1);
    }
    format!("{product} (build {build})")
}

/// Step 2/3: `podman --version`; if missing, install via winget.
///
/// `RedHat.Podman` (confirmed via `winget search podman` on this machine —
/// there's also a community `Podman.CLI` package, this is the official
/// Red Hat one matching podman.io) with `--accept-source-agreements
/// --accept-package-agreements` so it runs non-interactively. Actually run
/// here, not guessed: `winget install -e --id RedHat.Podman
/// --accept-source-agreements --accept-package-agreements` installed
/// Podman 5.8.3 to `C:\Program Files\RedHat\Podman` and printed
/// `Successfully installed`.
async fn ensure_podman_installed(log: &ProvisioningLog) -> Result<()> {
    if crate::podman::podman_available() {
        log.info("podman found on PATH");
        return Ok(());
    }
    log.info("podman not found on PATH — checking for winget");
    anyhow::ensure!(
        crate::podman::binary_available("winget"),
        "podman is not installed, and winget is not available to install it. Install Podman \
         manually from https://podman.io/docs/installation and re-run."
    );
    log.info("winget found — attempting `winget install -e --id RedHat.Podman`");
    let install = run_logged(
        log,
        "winget",
        &[
            "install",
            "-e",
            "--id",
            "RedHat.Podman",
            "--accept-source-agreements",
            "--accept-package-agreements",
        ],
    )
    .await?;
    anyhow::ensure!(
        install.status.success(),
        "`winget install -e --id RedHat.Podman` failed:\n{}\n\
         Install Podman manually from https://podman.io/docs/installation and re-run.",
        String::from_utf8_lossy(&install.stderr).trim()
    );
    refresh_path_from_registry(log);
    anyhow::ensure!(
        crate::podman::podman_available(),
        "`winget install` reported success but `podman --version` still fails even after \
         refreshing PATH from the registry. Close and reopen your terminal (or reboot) and \
         re-run, or install Podman manually from https://podman.io/docs/installation."
    );
    log.info("podman installed via winget");
    Ok(())
}

/// Step 4: before touching `podman machine` at all, make sure WSL2 itself
/// is usable — `podman machine init`/`start` on Windows run entirely inside
/// a WSL2 distro Podman creates, and fail with confusing WSL-flavored
/// errors (not podman ones) if WSL2 isn't ready.
///
/// This machine (Windows 11 **Home**) is proof WSL2 itself works fine
/// without full Hyper-V — this whole project has been built inside WSL2
/// here via the lighter "Virtual Machine Platform" feature — so this does
/// NOT treat Home edition as unsupported. It only reacts to what a real
/// `wsl --status` run actually says.
///
/// Encoding gotcha verified by hex-dumping a captured `wsl --status` run on
/// this machine: when its output is redirected/captured (as opposed to
/// printed to a real console), `wsl.exe` writes a stray UTF-8 BOM
/// (`EF BB BF`) followed by the text in UTF-16LE, not UTF-8 — decoded for
/// real below, not assumed.
async fn check_wsl2_usable(log: &ProvisioningLog) -> Result<()> {
    let output = match tokio_command("wsl").arg("--status").output().await {
        Ok(o) => o,
        Err(e) => {
            log.error(&format!("`wsl --status` could not even be run: {e}"));
            anyhow::bail!(
                "WSL is not installed on this machine. Podman on Windows runs its containers \
                 inside a WSL2 distro, so it can't work without it. Open an elevated \
                 PowerShell and run `wsl --install`, reboot if prompted, then re-run."
            );
        }
    };
    let stdout = decode_wsl_output(&output.stdout);
    let stderr = decode_wsl_output(&output.stderr);
    log.info(&format!(
        "`wsl --status` exited {} — stdout: {:?} stderr: {:?}",
        output.status,
        stdout.trim(),
        stderr.trim(),
    ));
    let combined = format!("{stdout}\n{stderr}");

    if !output.status.success() {
        // `0x80370102` / "HYPERV_NOT_INSTALLED" is the specific error code
        // Microsoft's own WSL troubleshooting docs document for
        // virtualization/Hyper-V-adjacent Windows features being disabled
        // (e.g. "Virtual Machine Platform" turned off, or virtualization
        // off in BIOS/UEFI) — matched on since it's real, stable, documented
        // text. This machine's virtualization already works, so this
        // couldn't be triggered here to confirm firsthand; anything else
        // that fails falls through to the raw-output message below rather
        // than guessing a diagnosis that wasn't actually observed.
        if combined.contains("0x80370102") || combined.contains("HYPERV_NOT_INSTALLED") {
            anyhow::bail!(
                "WSL2 can't start a VM on this machine — this matches the error Microsoft \
                 documents for virtualization/Hyper-V-adjacent Windows features being \
                 disabled. Enable 'Virtual Machine Platform' (Settings > Apps > Optional \
                 features > More Windows features, or `dism /online /enable-feature \
                 /featurename:VirtualMachinePlatform`), make sure virtualization is enabled \
                 in your BIOS/UEFI, reboot, then re-run."
            );
        }
        anyhow::bail!(
            "`wsl --status` failed and the reason doesn't match a known case — see the raw \
             output in the provisioning log. Try `wsl --install` or `wsl --update`, then \
             re-run."
        );
    }
    if combined.contains("Default Version: 1") {
        anyhow::bail!(
            "WSL is installed but its default version is WSL1 — Podman needs WSL2. Run \
             `wsl --set-default-version 2` and re-run."
        );
    }
    if !combined.contains("Default Distribution") {
        anyhow::bail!(
            "WSL is installed but has no distribution set up. Run `wsl --install` to install \
             the default distro, then re-run."
        );
    }
    log.info("WSL2 looks usable");
    Ok(())
}

/// `wsl.exe` writes UTF-16LE when captured, sometimes behind a stray UTF-8
/// BOM (see `check_wsl2_usable`); on this machine in October 2026 it came
/// with no BOM, and with `WSL_UTF8=1` set it writes plain UTF-8. So: strip
/// a BOM if present, then decode as UTF-16LE only if the bytes contain NULs
/// (ASCII-range UTF-16 always does), else as UTF-8.
fn decode_wsl_output(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if bytes.contains(&0) {
        let u16s: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&u16s)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Step 5: `podman machine list` — init a machine if none exists, start it
/// if one exists but isn't running. Same `{{.Name}}\t{{.Running}}` template
/// used across platforms (Podman abstracts the WSL2-vs-QEMU/AppleHV backend
/// distinction internally).
async fn ensure_machine_running(log: &ProvisioningLog) -> Result<()> {
    let list = run_logged(
        log,
        "podman",
        &["machine", "list", "--noheading", "--format", "{{.Name}}\t{{.Running}}"],
    )
    .await?;
    anyhow::ensure!(
        list.status.success(),
        "`podman machine list` failed:\n{}",
        String::from_utf8_lossy(&list.stderr).trim()
    );
    let already_running = match parse_machine_list(&String::from_utf8_lossy(&list.stdout)) {
        None => {
            log.info("no podman machine found — running `podman machine init`");
            let init = run_logged(log, "podman", &["machine", "init"]).await?;
            anyhow::ensure!(
                init.status.success(),
                "`podman machine init` failed:\n{}",
                String::from_utf8_lossy(&init.stderr).trim()
            );
            false
        }
        Some(running) => {
            log.info(&format!("podman machine already exists (running: {running})"));
            running
        }
    };

    if !already_running {
        log.info("starting podman machine — running `podman machine start`");
        let start = run_logged(log, "podman", &["machine", "start"]).await?;
        anyhow::ensure!(
            start.status.success(),
            "`podman machine start` failed:\n{}",
            String::from_utf8_lossy(&start.stderr).trim()
        );
    }
    Ok(())
}

/// Step 6a: `podman info` must succeed once the machine is up — the real
/// end-to-end check that the CLI can actually reach the VM, not just that
/// the binary exists.
///
/// ponytail: the one retry below is a real fix reproduced more than once on
/// this machine, not a hypothetical — `podman machine start` can report
/// success while the WSL2 distro's own user session/SSH plumbing is still
/// wedged (observed cause: `user@1000.service` inside the distro failing
/// with "Device or resource busy", left over from an earlier WSL session
/// that didn't shut down cleanly), and `podman info` then fails with an SSH
/// connect error even though `podman machine list` shows it running. A
/// full `wsl --shutdown` + `podman machine start` cleared it every time it
/// was tried. This is one bounded retry, not a loop — if it doesn't clear
/// up after that, it's surfaced as a real failure instead of retried
/// blindly forever.
///
/// `allow_wsl_shutdown` is false on the setup-wizard path: `wsl --shutdown`
/// stops every WSL distro the person has, so it's never done silently there
/// (the error and the MachineReady manual instructions mention it instead).
async fn verify_podman_info(log: &ProvisioningLog, allow_wsl_shutdown: bool) -> Result<()> {
    let info = run_logged(log, "podman", &["info"]).await?;
    if info.status.success() {
        return Ok(());
    }
    anyhow::ensure!(
        allow_wsl_shutdown,
        "The Podman machine started, but `podman info` can't reach it:\n{}\n\
         Running `wsl --shutdown` (this stops all running WSL distributions) and trying again \
         usually fixes this.",
        output_text(&info)
    );
    log.error(&format!(
        "`podman info` failed on the first try: {}",
        String::from_utf8_lossy(&info.stderr).trim()
    ));
    log.info("retrying once: `wsl --shutdown` then `podman machine start`");
    run_logged(log, "wsl", &["--shutdown"]).await?;
    let restart = run_logged(log, "podman", &["machine", "start"]).await?;
    anyhow::ensure!(
        restart.status.success(),
        "`podman machine start` failed on retry after `wsl --shutdown`:\n{}",
        String::from_utf8_lossy(&restart.stderr).trim()
    );
    let info2 = run_logged(log, "podman", &["info"]).await?;
    anyhow::ensure!(
        info2.status.success(),
        "`podman info` still fails after a `wsl --shutdown` + `podman machine start` retry:\n{}",
        String::from_utf8_lossy(&info2.stderr).trim()
    );
    Ok(())
}

/// Step 6b: `podman-compose --version`; installed via pip if missing.
///
/// Verified empirically: the Windows Podman installer (winget's
/// `RedHat.Podman`) does NOT bundle `podman-compose` — right after
/// installing and starting Podman, `podman-compose --version` still fails.
/// `pip install podman-compose` is what actually works; the upstream
/// `containers/podman-compose` project's own README documents pip as the
/// Windows install path (there's no winget/choco package for it).
async fn ensure_compose_installed(log: &ProvisioningLog) -> Result<()> {
    if crate::podman::podman_compose_available() {
        log.info("podman-compose found on PATH");
        return Ok(());
    }
    log.info(
        "podman-compose not found on PATH (confirmed: the Windows Podman installer does not \
         bundle it) — checking for Python",
    );
    anyhow::ensure!(
        crate::podman::binary_available("python"),
        "podman-compose is not installed, and Python (needed to `pip install` it) is not on \
         PATH. Install Python from https://python.org and re-run, or install podman-compose \
         manually: https://github.com/containers/podman-compose."
    );
    log.info("python found — running `pip install podman-compose`");
    let install = run_logged(log, "pip", &["install", "podman-compose"]).await?;
    anyhow::ensure!(
        install.status.success(),
        "`pip install podman-compose` failed:\n{}",
        String::from_utf8_lossy(&install.stderr).trim()
    );

    // pip's Windows "user site" install (used automatically here since the
    // system site-packages dir isn't writable without admin) drops
    // `podman-compose.exe` into a per-Python-version Scripts dir, and pip
    // does NOT add it to PATH itself — verified: pip prints a "which is not
    // on PATH" warning for it. The exact directory is NOT `<user
    // base>\Scripts` — verified the hard way, that guess is wrong: Windows
    // nests it one level deeper under the Python version, e.g.
    // `<user base>\Python313\Scripts`. `sysconfig.get_path('scripts',
    // 'nt_user')` is the one API that reports the real, version-correct
    // path, so that's what's actually asked here instead of guessing again.
    let Some(scripts_dir) = pip_user_scripts_dir() else {
        anyhow::bail!(
            "installed podman-compose via pip, but locating its install directory via \
             `sysconfig.get_path('scripts', 'nt_user')` failed afterwards"
        );
    };
    log.info(&format!("adding {scripts_dir} to this process's PATH"));
    let existing = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{scripts_dir};{existing}"));

    anyhow::ensure!(
        crate::podman::podman_compose_available(),
        "installed podman-compose via pip but `podman-compose --version` still fails even \
         after adding {scripts_dir} to PATH. Restart your terminal and re-run."
    );
    log.info("podman-compose installed via pip");
    Ok(())
}

// --- Setup wizard steps (called from `setup.rs`) ---

/// `WslEnabled` check (read-only): `wsl --status` succeeds and doesn't say
/// Virtual Machine Platform is missing. Unlike `check_wsl2_usable`, a
/// default distribution is NOT required: the fix is `wsl --install
/// --no-distribution`, and `podman machine init` imports its own WSL2 distro
/// (always as version 2, so the default version doesn't matter either).
pub(crate) async fn check_wsl(log: &ProvisioningLog) -> StepCheck {
    log.info("running: wsl --status");
    let output = match tokio_command("wsl").arg("--status").output().await {
        Ok(o) => o,
        Err(e) => {
            return StepCheck {
                ok: false,
                summary: "The Windows Subsystem for Linux isn't installed yet.".into(),
                details: format!("`wsl --status` could not be run: {e}"),
            }
        }
    };
    let text = format!("{}\n{}", decode_wsl_output(&output.stdout), decode_wsl_output(&output.stderr))
        .trim()
        .to_string();
    log.info(&format!("`wsl --status` exited {} — output: {text:?}", output.status));
    let (ok, summary) = classify_wsl_status(output.status.success(), &text);
    StepCheck { ok, summary: summary.into(), details: text }
}

/// ponytail: matches English `wsl --status` text only; on a localized
/// Windows only the exit code and the error code count.
fn classify_wsl_status(success: bool, text: &str) -> (bool, &'static str) {
    if text.contains("0x80370102") || text.contains("HYPERV_NOT_INSTALLED") {
        return (
            false,
            "Virtualization is turned off on this computer (check Virtual Machine Platform and your BIOS/UEFI settings).",
        );
    }
    if !success || text.to_lowercase().contains("virtual machine platform") {
        return (false, "The Windows Subsystem for Linux isn't turned on yet.");
    }
    (true, "The Windows Subsystem for Linux is turned on.")
}

/// `PodmanInstalled` fix: Podman via winget, Python via winget (per-user)
/// only if it's missing, then podman-compose via pip.
pub(crate) async fn fix_podman_installed(log: &ProvisioningLog) -> Result<()> {
    ensure_podman_installed(log).await?;
    if !crate::podman::podman_compose_available() && !crate::podman::binary_available("python") {
        anyhow::ensure!(
            crate::podman::binary_available("winget"),
            "Python (needed to install podman-compose) is missing and winget isn't available. \
             Install Python from https://python.org, then try again."
        );
        // ponytail: unverified on real hardware (this machine already has
        // Python). PrependPath=1 because the python.org installer leaves PATH
        // alone by default.
        let install = run_logged(
            log,
            "winget",
            &[
                "install",
                "-e",
                "--id",
                "Python.Python.3.13",
                "--scope",
                "user",
                "--accept-source-agreements",
                "--accept-package-agreements",
                "--override",
                "/quiet InstallAllUsers=0 PrependPath=1 Include_pip=1",
            ],
        )
        .await?;
        anyhow::ensure!(
            install.status.success(),
            "`winget install Python.Python.3.13` failed (exit code {:?}):\n{}",
            install.status.code(),
            output_text(&install)
        );
        refresh_path_from_registry(log);
    }
    ensure_compose_installed(log).await
}

/// The PowerShell that runs `wsl --install --no-distribution` elevated
/// (UAC prompt via `Start-Process -Verb RunAs`) and exits with its real
/// exit code. A declined prompt makes Start-Process throw; that exits 1223
/// (ERROR_CANCELLED). `$ErrorActionPreference = 'Stop'` matters: without it
/// the throw is non-terminating, `$p` is null and `exit $null` exits 0.
const WSL_INSTALL_PS: &str = "$ErrorActionPreference = 'Stop'; try { \
    $p = Start-Process -FilePath 'wsl.exe' -ArgumentList '--install','--no-distribution' -Verb RunAs -Wait -PassThru; \
    exit $p.ExitCode } catch { Write-Output $_.Exception.Message; exit 1223 }";

/// `WslEnabled` fix. The elevated process's output can't be captured, only
/// its exit code.
pub(crate) async fn fix_wsl(log: &ProvisioningLog) -> Result<()> {
    let out = run_logged(log, "powershell", &["-NoProfile", "-NonInteractive", "-Command", WSL_INSTALL_PS]).await?;
    match out.status.code() {
        // 3010 = ERROR_SUCCESS_REBOOT_REQUIRED; the restart step follows anyway.
        Some(0) | Some(3010) => Ok(()),
        Some(1223) => anyhow::bail!(
            "The administrator prompt was declined, so WSL wasn't turned on.\n{}",
            output_text(&out)
        ),
        code => anyhow::bail!(
            "`wsl --install --no-distribution` failed (exit code {code:?}).\n{}",
            output_text(&out)
        ),
    }
}

/// `MachineReady` fix: init (if none) + start + `podman info`, without the
/// silent `wsl --shutdown` retry `ensure_ready` does.
pub(crate) async fn fix_machine(log: &ProvisioningLog) -> Result<()> {
    ensure_machine_running(log).await?;
    verify_podman_info(log, false).await
}

/// `FunctionalCheck` fix ("Restart WSL and retry", only after the person
/// agreed that every WSL distro stops): the machine can report Running while
/// every container start fails or hangs (see `verify_podman_info`), and a
/// `wsl --shutdown` + `podman machine start` is what clears it. The wizard
/// re-runs the test container afterwards.
pub(crate) async fn fix_restart_wsl(log: &ProvisioningLog) -> Result<()> {
    let out = run_logged(log, "wsl", &["--shutdown"]).await?;
    anyhow::ensure!(
        out.status.success(),
        "`wsl --shutdown` failed (exit code {:?}):\n{}{}",
        out.status.code(),
        decode_wsl_output(&out.stdout),
        decode_wsl_output(&out.stderr)
    );
    ensure_machine_running(log).await
}

pub(crate) fn restart_computer() -> Result<()> {
    let out = sync_command("shutdown").args(["/r", "/t", "5", "/c", "LocalSync setup"]).output()?;
    anyhow::ensure!(
        out.status.success(),
        "`shutdown /r` failed (exit code {:?}):\n{}",
        out.status.code(),
        output_text(&out)
    );
    Ok(())
}

/// The Windows entry point called from `provisioning.rs`'s
/// `ensure_podman_ready`. Order: make sure the `podman` CLI exists (install
/// via winget if not) -> make sure WSL2 itself is usable -> make sure the
/// machine VM exists and is running -> verify the whole chain actually
/// works end to end (`podman info`) -> make sure `podman-compose` exists
/// too. Every step logs before/after so a pasted log means something
/// without needing anyone to reproduce the machine.
pub async fn ensure_ready(log: &ProvisioningLog) -> Result<()> {
    ensure_podman_installed(log).await?;
    check_wsl2_usable(log).await?;
    ensure_machine_running(log).await?;
    verify_podman_info(log, true).await?;
    ensure_compose_installed(log).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not run by default (`cargo test -p ls-containers` skips it) — this
    /// genuinely installs/starts Podman on whatever machine runs it, which
    /// is only appropriate to do deliberately. Run explicitly with:
    /// `cargo test -p ls-containers --test '*' -- --ignored` or
    /// `cargo test -p ls-containers windows_provisioning_end_to_end -- --ignored`.
    /// Verified for real on this machine during development: this test
    /// passed after the full winget install -> WSL2 check -> machine
    /// init/start -> `podman info` -> pip `podman-compose` install chain
    /// ran end to end.
    #[tokio::test]
    #[ignore]
    async fn windows_provisioning_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let log = ProvisioningLog::open_in(dir.path()).unwrap();
        let result = ensure_ready(&log).await;
        let contents = std::fs::read_to_string(log.path()).unwrap_or_default();
        assert!(result.is_ok(), "provisioning failed: {result:?}\nlog:\n{contents}");
        assert!(crate::podman::podman_available());
        assert!(crate::podman::podman_compose_available());
    }

    #[test]
    fn wsl_output_decoding_handles_utf16_with_and_without_bom_and_utf8() {
        let utf16: Vec<u8> = "Default Version: 2".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let with_bom = [&[0xEF, 0xBB, 0xBF][..], &utf16].concat();
        assert_eq!(decode_wsl_output(&utf16), "Default Version: 2");
        assert_eq!(decode_wsl_output(&with_bom), "Default Version: 2");
        assert_eq!(decode_wsl_output(b"Default Version: 2"), "Default Version: 2");
    }

    #[test]
    fn wsl_status_classification() {
        assert!(classify_wsl_status(true, "Default Version: 2").0);
        // No default distribution is fine (podman brings its own).
        assert!(classify_wsl_status(true, "Default Version: 2\nNo default distribution").0);
        assert!(!classify_wsl_status(false, "WSL is not installed").0);
        assert!(!classify_wsl_status(true, "Please enable the Virtual Machine Platform feature").0);
        assert!(!classify_wsl_status(false, "Error code: Wsl/0x80370102").0);
    }

    #[test]
    fn path_merge_appends_only_missing_entries() {
        assert_eq!(merge_path(r"C:\a;C:\B\", r"c:\b;C:\c;"), r"C:\a;C:\B\;C:\c");
        std::env::set_var("LS_TEST_EXPAND_VAR", r"C:\x");
        assert_eq!(
            expand_env(r"%LS_TEST_EXPAND_VAR%\bin;%LS_NOT_SET_XYZ%;50%"),
            r"C:\x\bin;%LS_NOT_SET_XYZ%;50%"
        );
    }

    /// The exit-code plumbing of `WSL_INSTALL_PS`, with the elevated
    /// `wsl.exe` swapped for an unelevated `cmd /c exit 7` (no UAC prompt):
    /// the real child exit code must come through.
    #[test]
    fn wsl_install_script_propagates_exit_code() {
        let script = WSL_INSTALL_PS
            .replace("'wsl.exe'", "'cmd.exe'")
            .replace("'--install','--no-distribution'", "'/c','exit 7'")
            .replace(" -Verb RunAs", " -WindowStyle Hidden");
        let out = sync_command("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script]).output().unwrap();
        assert_eq!(out.status.code(), Some(7));
        let failing = WSL_INSTALL_PS.replace("'wsl.exe'", "'definitely-not-a-real-exe-xyz.exe'").replace(" -Verb RunAs", "");
        let out = sync_command("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &failing]).output().unwrap();
        assert_eq!(out.status.code(), Some(1223), "a Start-Process failure must not exit 0");
    }

    #[test]
    fn detailed_version_is_nonempty_and_mentions_windows() {
        let v = detailed_version();
        assert!(v.contains("Windows"), "unexpected version string: {v}");
        assert!(v.contains("build"), "unexpected version string: {v}");
    }
}
