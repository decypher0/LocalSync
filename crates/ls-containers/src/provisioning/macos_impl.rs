//! macOS Podman provisioning: install Podman via Homebrew if missing, then
//! init/start the Podman machine VM, then verify `podman info` and
//! `podman-compose --version` both work.
//!
//! # THIS FILE IS UNVERIFIED
//!
//! There is no macOS environment anywhere in this build pipeline — no Mac
//! hardware, no macOS VM, no Apple SDK. `#[cfg(target_os = "macos")]` means
//! this module is entirely excluded from compilation on every target this
//! machine can build for, so nothing here has ever been compiled, let alone
//! run. Every command name, flag, and piece of control flow below was
//! written by cross-referencing Podman's and Homebrew's official docs (see
//! the comments next to each command), not by testing it. The first real
//! verification happens on a developer's own Mac — see
//! `docs/round5-manual-test-checklist.md`. Treat a failure there as more
//! credible than anything asserted in this comment.

use super::ProvisioningLog;
use anyhow::{Context, Result};

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
    let output = tokio::process::Command::new(program)
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

/// Step 2/3: `podman --version` (via the same [`crate::podman::podman_available`]
/// check round 1 used); if missing, install via Homebrew.
///
/// Cross-referenced against podman.io's install docs and Homebrew's formula
/// page: `podman` is an official homebrew-core formula (not a third-party
/// tap), so `brew install podman` is the real, current command — podman.io's
/// own docs mention Homebrew as a valid (if community-maintained) install
/// path alongside the signed `.pkg` installer from podman.io, which we don't
/// use here since it can't be scripted non-interactively.
async fn ensure_podman_installed(log: &ProvisioningLog) -> Result<()> {
    if crate::podman::podman_available() {
        log.info("podman found on PATH");
        return Ok(());
    }
    log.info("podman not found on PATH — checking for Homebrew");
    anyhow::ensure!(
        crate::podman::binary_available("brew"),
        "podman is not installed, and Homebrew (`brew`) is not available to install it. \
         Install Homebrew from https://brew.sh and re-run, or install Podman directly \
         from https://podman.io/docs/installation."
    );
    log.info("Homebrew found — attempting `brew install podman`");
    let install = run_logged(log, "brew", &["install", "podman"]).await?;
    anyhow::ensure!(
        install.status.success(),
        "`brew install podman` failed:\n{}",
        String::from_utf8_lossy(&install.stderr).trim()
    );
    anyhow::ensure!(
        crate::podman::podman_available(),
        "`brew install podman` reported success but `podman --version` still fails — \
         check that Homebrew's bin directory (e.g. /opt/homebrew/bin on Apple Silicon, \
         /usr/local/bin on Intel) is on PATH, then re-run."
    );
    log.info("podman installed via Homebrew");
    Ok(())
}

/// Step 4: `podman machine list` — init a machine if none exists, start it
/// if one exists but isn't running.
///
/// The `{{.Name}}\t{{.Running}}` Go-template format is the same
/// `podman machine list` used on every platform (Podman abstracts the
/// WSL2-vs-QEMU/AppleHV backend distinction internally) — `.Running` is a
/// bool field so the template renders literally `true`/`false`, which is
/// parsed here directly rather than pulling in a JSON crate for one field.
///
/// Note on hardware requirements, since the task framing for this file
/// assumed QEMU specifically: as of Podman 5.x the macOS default backend is
/// actually Apple's native `applehv` (Virtualization.framework), with QEMU
/// as a fallback (e.g. when Rosetta is disabled) rather than the only
/// option — cross-referenced against Podman's own machine docs and the
/// Podman Desktop Rosetta docs. Either way the point in the task brief
/// stands: every Mac capable of running current macOS supports hardware
/// virtualization (Hypervisor.framework on Apple Silicon, VT-x on Intel
/// Macs from the last 15+ years), so there's no realistic
/// "hardware doesn't support this" failure mode worth special-casing here —
/// a generic `podman machine init`/`start` failure just surfaces its real
/// stderr text below.
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
            log.info(&format!(
                "podman machine already exists (running: {running})"
            ));
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

/// Step 5a: `podman info` must succeed once the machine is up — this is the
/// real end-to-end check that the CLI can actually reach the VM, not just
/// that both binaries exist on PATH.
async fn verify_podman_info(log: &ProvisioningLog) -> Result<()> {
    let info = run_logged(log, "podman", &["info"]).await?;
    anyhow::ensure!(
        info.status.success(),
        "`podman info` failed — the podman machine may not have finished starting:\n{}",
        String::from_utf8_lossy(&info.stderr).trim()
    );
    Ok(())
}

/// Step 5b: `podman-compose --version`; install via Homebrew if missing.
///
/// Cross-referenced against Homebrew's formula page: `podman-compose` is
/// also an official homebrew-core formula (depends on `podman` and
/// `python@3`), so it's installed the same way as `podman` itself for
/// consistency — the upstream `containers/podman-compose` project's own
/// README documents `pip3 install podman-compose` as an alternative, which
/// is what the error message below points to if Homebrew isn't available
/// (matching the podman-not-installed error path above).
async fn ensure_compose_installed(log: &ProvisioningLog) -> Result<()> {
    if crate::podman::podman_compose_available() {
        log.info("podman-compose found on PATH");
        return Ok(());
    }
    log.info("podman-compose not found on PATH — checking for Homebrew");
    anyhow::ensure!(
        crate::podman::binary_available("brew"),
        "podman-compose is not installed, and Homebrew (`brew`) is not available to \
         install it. Install Homebrew from https://brew.sh and re-run \
         (`brew install podman-compose`), or install it via pip: \
         `pip3 install podman-compose`."
    );
    log.info("Homebrew found — attempting `brew install podman-compose`");
    let install = run_logged(log, "brew", &["install", "podman-compose"]).await?;
    anyhow::ensure!(
        install.status.success(),
        "`brew install podman-compose` failed:\n{}",
        String::from_utf8_lossy(&install.stderr).trim()
    );
    anyhow::ensure!(
        crate::podman::podman_compose_available(),
        "`brew install podman-compose` reported success but `podman-compose --version` \
         still fails — check that Homebrew's bin directory is on PATH."
    );
    log.info("podman-compose installed via Homebrew");
    Ok(())
}

/// The macOS entry point called from `provisioning.rs`'s
/// `ensure_podman_ready`. Order: make sure the `podman` CLI exists (install
/// if not) -> make sure its machine VM exists and is running -> verify the
/// whole chain actually works end to end (`podman info`) -> make sure
/// `podman-compose` exists too. Every step logs before/after so a pasted
/// log means something without needing anyone to reproduce the machine.
pub async fn ensure_ready(log: &ProvisioningLog) -> Result<()> {
    ensure_podman_installed(log).await?;
    ensure_machine_running(log).await?;
    verify_podman_info(log).await?;
    ensure_compose_installed(log).await?;
    Ok(())
}

/// Step 1: `macOS <productVersion> (<codename>)`, e.g. `macOS 14.5 (Sonoma)`.
/// Shells out to `sw_vers` (the standard macOS tool for this — deliberately
/// not hand-rolled parsing of `/System/Library/CoreServices/SystemVersion.plist`
/// or similar) rather than guessing; the codename is looked up locally since
/// `sw_vers` has no flag that reports it.
pub fn detailed_version() -> String {
    let product_name = sw_vers("-productName").unwrap_or_else(|| "macOS".to_string());
    let product_version = sw_vers("-productVersion").unwrap_or_else(|| "unknown".to_string());
    match codename_for(&product_version) {
        Some(codename) => format!("{product_name} {product_version} ({codename})"),
        None => format!("{product_name} {product_version}"),
    }
}

fn sw_vers(flag: &str) -> Option<String> {
    let output = std::process::Command::new("sw_vers").arg(flag).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// Major-version -> marketing-name lookup, current through the macOS
/// releases shipping as of this writing (macOS 26 "Tahoe", released
/// September 2025 — Apple switched from macOS 15/16/... to year-based
/// numbering starting with that release, so `sw_vers -productVersion` on a
/// current Mac reports a `26.x` string, not `16.x`). Returns `None` for
/// anything older/unrecognized rather than guessing; the version number
/// itself is still logged either way.
fn codename_for(product_version: &str) -> Option<&'static str> {
    let major: u32 = product_version.split('.').next()?.parse().ok()?;
    Some(match major {
        26 => "Tahoe",
        15 => "Sequoia",
        14 => "Sonoma",
        13 => "Ventura",
        12 => "Monterey",
        11 => "Big Sur",
        _ => return None,
    })
}
