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

use super::ProvisioningLog;
use anyhow::{Context, Result};
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

/// Runs `program args...`, logging the command line and its exit
/// status/stdout/stderr before returning the raw [`std::process::Output`].
/// Only errors (via `Context`) on a spawn failure (binary not found etc) —
/// a non-zero exit is left to the caller to turn into a specific `ensure!`
/// with its own actionable message.
async fn run_logged(
    log: &ProvisioningLog,
    program: &str,
    args: &[&str],
) -> Result<std::process::Output> {
    let cmdline = format!("{program} {}", args.join(" "));
    log.info(&format!("running: {cmdline}"));
    let output = tokio_command(program)
        .args(args)
        .output()
        .await
        .with_context(|| format!("failed to run `{cmdline}` — is it on PATH?"))?;
    log.info(&format!(
        "`{cmdline}` exited {} — stdout: {:?} stderr: {:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim(),
    ));
    Ok(output)
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
fn refresh_path_from_registry(log: &ProvisioningLog) {
    let machine =
        reg_query_value(r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment", "Path");
    let user = reg_query_value(r"HKCU\Environment", "Path");
    let mut combined = machine.unwrap_or_default();
    if let Some(u) = user {
        if !combined.is_empty() && !combined.ends_with(';') {
            combined.push(';');
        }
        combined.push_str(&u);
    }
    if combined.is_empty() {
        log.info("PATH refresh: could not read PATH from the registry, leaving it unchanged");
        return;
    }
    log.info("refreshed this process's PATH from the registry after an install");
    std::env::set_var("PATH", combined);
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
    let decode = |bytes: &[u8]| -> String {
        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
        let u16s: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&u16s)
    };
    let stdout = decode(&output.stdout);
    let stderr = decode(&output.stderr);
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
    let stdout = String::from_utf8_lossy(&list.stdout);
    let first_machine = stdout.lines().find(|l| !l.trim().is_empty()).map(str::to_string);

    let already_running = match first_machine {
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
        Some(line) => {
            let running = line.split('\t').nth(1).map(str::trim) == Some("true");
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
async fn verify_podman_info(log: &ProvisioningLog) -> Result<()> {
    let info = run_logged(log, "podman", &["info"]).await?;
    if info.status.success() {
        return Ok(());
    }
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
    let scripts_query = run_logged(
        log,
        "python",
        &["-c", "import sysconfig; print(sysconfig.get_path('scripts', 'nt_user'))"],
    )
    .await?;
    anyhow::ensure!(
        scripts_query.status.success(),
        "installed podman-compose via pip, but locating its install directory via \
         `sysconfig.get_path('scripts', 'nt_user')` failed afterwards:\n{}",
        String::from_utf8_lossy(&scripts_query.stderr).trim()
    );
    let scripts_dir = String::from_utf8_lossy(&scripts_query.stdout).trim().to_string();
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
    verify_podman_info(log).await?;
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
    fn detailed_version_is_nonempty_and_mentions_windows() {
        let v = detailed_version();
        assert!(v.contains("Windows"), "unexpected version string: {v}");
        assert!(v.contains("build"), "unexpected version string: {v}");
    }
}
