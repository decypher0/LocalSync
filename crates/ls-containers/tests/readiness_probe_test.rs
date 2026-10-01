//! The functional readiness probe and the Run gate, against fake `podman`
//! binaries (deterministic failures: memory limit, missing binary, connection
//! refused, hang) and against the real installed Podman.
//!
//! Fakes are shell scripts, so those tests are unix-only: on Windows Rust's
//! `Command::new("podman")` only resolves `podman.exe`, never a `.cmd`, so a
//! fake script there would silently not be the one that runs.
//!
//! Every test here holds `ENV_LOCK`: they mutate PATH (and XDG_DATA_HOME,
//! so gate logs land in a tempdir), which is process-wide.

use std::sync::Mutex;

use ls_containers::readiness::{self, functional_probe};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn have(bin: &str) -> bool {
    std::process::Command::new(bin)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

const MEMORY_SENTENCE: &str =
    "Podman can't apply memory limits on this computer, so LocalSync's sandbox can't start containers.";

/// The real installed Podman. On a machine with the WSL 3.0.1 cgroup fault
/// this must fail *and be classified as the memory failure*; where Podman
/// works it must succeed. Skips if podman isn't installed.
#[tokio::test]
async fn real_podman_probe() {
    let _g = lock();
    if !have("podman") {
        eprintln!("podman not installed - skipping");
        return;
    }
    let outcome = functional_probe(readiness::PROBE_TIMEOUT).await;
    eprintln!("REAL PROBE: {outcome:#?}");
    if outcome.ok {
        assert!(outcome.summary.is_empty());
        return;
    }
    assert!(!outcome.details.is_empty());
    if outcome.details.contains("memory.max") {
        assert!(outcome.memory_limit_unsupported, "{outcome:?}");
        assert_eq!(outcome.summary, MEMORY_SENTENCE);
    }
    if have("podman-compose") {
        readiness::invalidate_probe_cache();
        let log = ls_containers::ProvisioningLog::open_default().unwrap();
        let err = format!(
            "{:#}",
            readiness::ensure_ready_for_run(&log).await.unwrap_err()
        );
        eprintln!("REAL GATE ERROR:\n{err}");
        assert!(err.starts_with(readiness::NOT_READY_PREFIX), "{err}");
        assert!(err.contains("Setup"), "{err}");
        if outcome.memory_limit_unsupported {
            assert!(
                err.contains("memory.max") && err.contains(ls_containers::MEMORY_LIMIT_UNSUPPORTED),
                "{err}"
            );
        }
    }
}

#[cfg(unix)]
mod fakes {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    const CRUN: &str = "Error: crun: open `memory.max` for writing: No such file or directory: OCI runtime attempted to invoke a command that was not found";

    /// A tempdir holding fake `podman` (whose non-`--version` body is
    /// `body`) and `podman-compose`, set as the ONLY PATH entry; restores
    /// PATH/XDG_DATA_HOME on drop. Each `podman run` appends a line to
    /// `count`.
    struct Fake {
        dir: tempfile::TempDir,
        old_path: Option<std::ffi::OsString>,
        old_xdg: Option<std::ffi::OsString>,
    }

    impl Fake {
        fn new(body: Option<&str>) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let count = dir.path().join("count");
            if let Some(body) = body {
                write_exe(
                    &dir.path().join("podman"),
                    &format!(
                        "#!/bin/sh\ncase \"$1\" in --version) echo 'podman version 9.9.9'; exit 0;; esac\necho run >> '{}'\n{body}\n",
                        count.display()
                    ),
                );
            }
            write_exe(
                &dir.path().join("podman-compose"),
                "#!/bin/sh\necho 'podman-compose version 1.0'\n",
            );
            let old_path = std::env::var_os("PATH");
            let old_xdg = std::env::var_os("XDG_DATA_HOME");
            std::env::set_var("PATH", dir.path());
            std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
            Fake {
                dir,
                old_path,
                old_xdg,
            }
        }
        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }
        fn runs(&self) -> usize {
            std::fs::read_to_string(self.path("count"))
                .map(|s| s.lines().count())
                .unwrap_or(0)
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            match &self.old_path {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
            match &self.old_xdg {
                Some(p) => std::env::set_var("XDG_DATA_HOME", p),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    fn write_exe(path: &Path, script: &str) {
        std::fs::write(path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn memory_body() -> String {
        format!("echo '{CRUN}' >&2\nexit 125")
    }

    #[tokio::test]
    async fn memory_limit_failure_is_classified() {
        let _g = lock();
        let _f = Fake::new(Some(&memory_body()));
        let o = functional_probe(Duration::from_secs(20)).await;
        assert!(!o.ok);
        assert!(o.memory_limit_unsupported, "{o:?}");
        assert_eq!(o.summary, MEMORY_SENTENCE);
        assert!(o.details.contains(CRUN), "{o:?}");
    }

    #[tokio::test]
    async fn missing_podman() {
        let _g = lock();
        let _f = Fake::new(None);
        let o = functional_probe(Duration::from_secs(20)).await;
        assert!(!o.ok);
        assert_eq!(o.summary, "Podman isn't installed.");
        assert!(!o.memory_limit_unsupported);
    }

    #[tokio::test]
    async fn connection_refused_means_vm_not_running() {
        let _g = lock();
        let msg = "Cannot connect to Podman. Please verify your connection to the Linux system using `podman system connection list`: dial tcp 127.0.0.1:50211: connect: connection refused";
        let _f = Fake::new(Some(&format!("echo '{msg}' >&2\nexit 125")));
        let o = functional_probe(Duration::from_secs(20)).await;
        assert!(!o.ok);
        assert_eq!(o.summary, "Podman's virtual machine isn't running.");
        assert!(o.details.contains("connection refused"), "{o:?}");
    }

    #[tokio::test]
    async fn hang_times_out_and_child_is_killed() {
        let _g = lock();
        let f = Fake::new(Some("echo $$ > \"${0%/*}/pid\"\nexec /bin/sleep 30"));
        let started = std::time::Instant::now();
        let o = functional_probe(Duration::from_secs(1)).await;
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "timeout not honored: {:?}",
            started.elapsed()
        );
        assert!(!o.ok);
        assert_eq!(o.summary, "Podman didn't respond within 1 seconds.");
        let pid = std::fs::read_to_string(f.path("pid"))
            .unwrap()
            .trim()
            .to_string();
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        // Gone, or at worst a zombie awaiting reaping - never still sleeping.
        assert!(
            stat.is_empty() || stat.contains(") Z "),
            "child {pid} still alive: {stat}"
        );
    }

    #[tokio::test]
    async fn gate_failure_has_prefix_and_real_output_and_is_not_cached() {
        let _g = lock();
        readiness::invalidate_probe_cache();
        let f = Fake::new(Some(&memory_body()));
        let log = ls_containers::ProvisioningLog::open_default().unwrap();
        let err = format!(
            "{:#}",
            readiness::ensure_ready_for_run(&log).await.unwrap_err()
        );
        assert!(
            err.starts_with(&format!(
                "{}: {MEMORY_SENTENCE}",
                readiness::NOT_READY_PREFIX
            )),
            "{err}"
        );
        assert!(err.contains("Open Setup"), "{err}");
        assert!(err.contains(CRUN), "{err}");
        assert!(
            err.contains(ls_containers::MEMORY_LIMIT_UNSUPPORTED),
            "{err}"
        );
        assert!(std::fs::read_to_string(log.path())
            .unwrap()
            .contains("provisioning check"));
        assert_eq!(f.runs(), 1);
        readiness::ensure_ready_for_run(&log).await.unwrap_err();
        assert_eq!(f.runs(), 2, "a failure must not be cached");
    }

    #[tokio::test]
    async fn gate_caches_success_until_invalidated() {
        let _g = lock();
        readiness::invalidate_probe_cache();
        let f = Fake::new(Some("exit 0"));
        let log = ls_containers::ProvisioningLog::open_default().unwrap();
        readiness::ensure_ready_for_run(&log).await.unwrap();
        readiness::ensure_ready_for_run(&log).await.unwrap();
        assert_eq!(f.runs(), 1, "second call within TTL must not re-probe");
        readiness::invalidate_probe_cache();
        readiness::ensure_ready_for_run(&log).await.unwrap();
        assert_eq!(f.runs(), 2);
        readiness::invalidate_probe_cache();
    }

    #[tokio::test]
    async fn gate_reports_missing_compose() {
        let _g = lock();
        let f = Fake::new(Some("exit 0"));
        std::fs::remove_file(f.path("podman-compose")).unwrap();
        let log = ls_containers::ProvisioningLog::open_default().unwrap();
        let err = format!(
            "{:#}",
            readiness::ensure_ready_for_run(&log).await.unwrap_err()
        );
        assert!(
            err.starts_with(readiness::NOT_READY_PREFIX) && err.contains("podman-compose"),
            "{err}"
        );
        assert_eq!(f.runs(), 0);
    }
}
