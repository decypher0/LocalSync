//! "Fix automatically" for Podman's machine on Windows when it is running but
//! unreachable (`ssh: rejected: connect failed`): inside the WSL distro
//! `user@1000.service` is failed, so there is no podman.socket. Recreating
//! the machine does not help; `wsl --shutdown` + `podman machine start` does.
//!
//! The runner is cross-platform with the command execution injected, so the
//! sequencing, timeouts and the `wsl -l -v` parser are unit-tested on any
//! host; `windows_impl::fix_restart_wsl` only plugs in real processes.
//! It never runs `podman machine rm` or `podman machine init`.

use std::future::Future;
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;

/// The steps, in order, as shown in the wizard. The last one is run by the
/// wizard's re-verify of the test-container step right after this fix.
pub const RECOVERY_STEPS: [&str; 5] = [
    "Stop WSL (wsl --shutdown)",
    "Wait for every WSL distribution to stop",
    "Start Podman's machine",
    "Wait for Podman to answer",
    "Run the test container again",
];

/// Payload of the `setup-fix-progress` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixProgress {
    /// Index into [`RECOVERY_STEPS`].
    pub index: usize,
    pub label: &'static str,
    /// "running" | "done" | "failed"
    pub status: &'static str,
}

/// One finished command: did it exit 0, its raw stdout (for `wsl -l -v`,
/// which is UTF-16LE), and stdout+stderr as text for errors.
#[derive(Debug, Clone, Default)]
pub struct CmdOut {
    pub ok: bool,
    pub stdout: Vec<u8>,
    pub text: String,
}

#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub wsl_stop_timeout: Duration,
    pub podman_info_timeout: Duration,
    pub poll: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            wsl_stop_timeout: Duration::from_secs(30),
            podman_info_timeout: Duration::from_secs(60),
            poll: Duration::from_secs(2),
        }
    }
}

/// Runs steps 1-4 of [`RECOVERY_STEPS`] through `run(program, args)`, then
/// marks step 5 running (the caller re-runs the test container).
pub async fn run_recovery<F, Fut>(run: F, progress: &(dyn Fn(FixProgress) + Send + Sync), timing: Timing) -> Result<()>
where
    F: Fn(&'static str, &'static [&'static str]) -> Fut,
    Fut: Future<Output = CmdOut>,
{
    let report = |index: usize, status| progress(FixProgress { index, label: RECOVERY_STEPS[index], status });
    let fail = |index: usize, msg: String| {
        report(index, "failed");
        Err(anyhow::anyhow!(msg))
    };

    report(0, "running");
    let out = run("wsl", &["--shutdown"]).await;
    if !out.ok {
        return fail(0, format!("`wsl --shutdown` failed:\n{}", out.text));
    }
    report(0, "done");

    // `wsl --shutdown` normally returns once everything stopped; this wait is
    // for distros that take a moment (or a service restarting one). On
    // timeout carry on: `podman machine start` still has a good chance, and an
    // unrecognised (localized) state word must not block the fix.
    report(1, "running");
    let deadline = tokio::time::Instant::now() + timing.wsl_stop_timeout;
    loop {
        let out = run("wsl", &["-l", "-v"]).await;
        if all_distros_stopped(&decode_wsl_output(&out.stdout)) || tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(timing.poll).await;
    }
    report(1, "done");

    // Not `ensure_machine_running`: that runs `podman machine init` when no
    // machine is listed, and recreating the machine doesn't fix this.
    report(2, "running");
    let out = run("podman", &["machine", "start"]).await;
    if !out.ok && !out.text.to_lowercase().contains("already running") {
        return fail(2, format!("`podman machine start` failed:\n{}", out.text));
    }
    report(2, "done");

    report(3, "running");
    let deadline = tokio::time::Instant::now() + timing.podman_info_timeout;
    loop {
        let out = run("podman", &["info"]).await;
        if out.ok {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return fail(
                3,
                format!(
                    "Podman still doesn't answer {} seconds after restarting WSL and its machine. `podman info` said:\n{}",
                    timing.podman_info_timeout.as_secs(),
                    out.text
                ),
            );
        }
        tokio::time::sleep(timing.poll).await;
    }
    report(3, "done");

    report(4, "running");
    Ok(())
}

/// `wsl.exe` writes UTF-16LE when captured, sometimes behind a stray UTF-8
/// BOM; with `WSL_UTF8=1` it writes plain UTF-8. So: strip a BOM (UTF-8 or
/// UTF-16LE) if present, then decode as UTF-16LE only if the bytes contain
/// NULs (ASCII-range UTF-16 always does), else as UTF-8.
pub(crate) fn decode_wsl_output(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if bytes.contains(&0) {
        let u16s: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&u16s).trim_start_matches('\u{feff}').to_string()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Decoded `wsl -l -v` -> is every listed distro stopped. Rows look like
/// `* Ubuntu   Running   2` (name, state, version; `*` marks the default);
/// the first line is the header. No rows (no distros, or an error message)
/// counts as stopped.
///
/// ponytail: "stopped" is matched against a short list of translations of
/// WSL's state word; on another language the wait just runs to its timeout.
pub(crate) fn all_distros_stopped(text: &str) -> bool {
    const STOPPED: [&str; 12] = [
        "stopped", "beendet", "arrêté", "arrete", "detenido", "parado", "interrompido", "arrestato", "gestopt",
        "zatrzymano", "остановлен", "已停止",
    ];
    text.lines()
        .skip_while(|l| l.trim().is_empty())
        .skip(1)
        .filter_map(|l| {
            let tokens: Vec<&str> = l.trim().trim_start_matches('*').split_whitespace().collect();
            // name, state words..., version (a number)
            (tokens.len() >= 3 && tokens[tokens.len() - 1].parse::<u8>().is_ok())
                .then(|| tokens[1..tokens.len() - 1].join(" ").to_lowercase())
        })
        .all(|state| STOPPED.iter().any(|s| state == *s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    const ALL_STOPPED: &str = "  NAME                      STATE           VERSION\r\n* Ubuntu                    Stopped         2\r\n  podman-machine-default    Stopped         2\r\n  docker-desktop            Stopped         2\r\n";
    const SOME_RUNNING: &str = "  NAME                      STATE           VERSION\r\n* Ubuntu                    Running         2\r\n  podman-machine-default    Stopped         2\r\n";

    #[test]
    fn wsl_list_parser_reads_utf16le_and_tells_all_stopped_from_some_running() {
        assert!(all_distros_stopped(&decode_wsl_output(&utf16(ALL_STOPPED))));
        assert!(!all_distros_stopped(&decode_wsl_output(&utf16(SOME_RUNNING))));
        // With a UTF-8 BOM, a UTF-16 BOM, or as plain UTF-8 (WSL_UTF8=1).
        assert!(!all_distros_stopped(&decode_wsl_output(&[&[0xEF, 0xBB, 0xBF][..], &utf16(SOME_RUNNING)].concat())));
        assert!(all_distros_stopped(&decode_wsl_output(&utf16(&format!("\u{feff}{ALL_STOPPED}")))));
        assert!(!all_distros_stopped(&decode_wsl_output(SOME_RUNNING.as_bytes())));
        // Localized, and an unknown state word counts as not stopped.
        assert!(all_distros_stopped("  NAME     STATUS    VERSION\n* Ubuntu   Beendet   2\n"));
        assert!(!all_distros_stopped("  NAME     STATUS           VERSION\n* Ubuntu   Wird ausgeführt  2\n"));
        // Nothing installed / an error message: nothing is running.
        assert!(all_distros_stopped(""));
        assert!(all_distros_stopped("Windows Subsystem for Linux has no installed distributions.\n"));
    }

    struct Script {
        calls: Mutex<Vec<String>>,
        /// How many `wsl -l -v` calls still show something running.
        running_lists: Mutex<u32>,
        /// How many `podman info` calls fail before one succeeds.
        info_failures: Mutex<u32>,
        start_ok: bool,
        start_text: &'static str,
    }

    impl Script {
        fn new(running_lists: u32, info_failures: u32) -> Self {
            Script {
                calls: Mutex::default(),
                running_lists: Mutex::new(running_lists),
                info_failures: Mutex::new(info_failures),
                start_ok: true,
                start_text: "Machine \"podman-machine-default\" started successfully",
            }
        }
        fn run(&self, program: &'static str, args: &'static [&'static str]) -> std::future::Ready<CmdOut> {
            let cmd = format!("{program} {}", args.join(" "));
            self.calls.lock().unwrap().push(cmd.clone());
            let countdown = |m: &Mutex<u32>| {
                let mut n = m.lock().unwrap();
                let hit = *n > 0;
                *n = n.saturating_sub(1);
                hit
            };
            std::future::ready(match cmd.as_str() {
                "wsl -l -v" => {
                    let list = if countdown(&self.running_lists) { SOME_RUNNING } else { ALL_STOPPED };
                    CmdOut { ok: true, stdout: utf16(list), text: String::new() }
                }
                "podman machine start" => CmdOut { ok: self.start_ok, text: self.start_text.into(), ..Default::default() },
                "podman info" if countdown(&self.info_failures) => CmdOut {
                    ok: false,
                    text: "Error: unable to connect to Podman socket: failed to connect: ssh: rejected: connect failed (open failed)".into(),
                    ..Default::default()
                },
                _ => CmdOut { ok: true, ..Default::default() },
            })
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    fn fast() -> Timing {
        Timing {
            wsl_stop_timeout: Duration::from_millis(200),
            podman_info_timeout: Duration::from_millis(200),
            poll: Duration::from_millis(5),
        }
    }

    async fn go(s: &Script, timing: Timing) -> (Result<()>, Vec<(usize, &'static str)>) {
        let seen: Mutex<Vec<(usize, &'static str)>> = Mutex::default();
        let r = run_recovery(|p, a| s.run(p, a), &|e: FixProgress| seen.lock().unwrap().push((e.index, e.status)), timing).await;
        (r, seen.into_inner().unwrap())
    }

    #[tokio::test]
    async fn runs_the_steps_in_order_and_reports_each() {
        let s = Script::new(2, 3);
        let (r, seen) = go(&s, fast()).await;
        r.unwrap();
        let calls = s.calls();
        let dedup: Vec<&String> = calls.iter().fold(Vec::new(), |mut v, c| {
            if v.last() != Some(&c) {
                v.push(c)
            }
            v
        });
        assert_eq!(dedup, ["wsl --shutdown", "wsl -l -v", "podman machine start", "podman info"]);
        assert_eq!(calls.iter().filter(|c| *c == "wsl -l -v").count(), 3, "polled until all stopped");
        assert_eq!(calls.iter().filter(|c| *c == "podman info").count(), 4, "polled until it answered");
        assert_eq!(
            seen,
            [(0, "running"), (0, "done"), (1, "running"), (1, "done"), (2, "running"), (2, "done"), (3, "running"), (3, "done"), (4, "running")]
        );
    }

    #[tokio::test]
    async fn never_removes_or_reinitializes_the_machine() {
        for s in [Script::new(0, 0), Script::new(100, 100), Script { start_ok: false, start_text: "boom", ..Script::new(0, 0) }] {
            let _ = go(&s, fast()).await;
            for c in s.calls() {
                assert!(!c.contains("machine rm") && !c.contains("machine init") && !c.contains("machine reset"), "{c}");
            }
        }
    }

    #[tokio::test]
    async fn a_distro_that_never_stops_times_out_and_the_fix_carries_on() {
        let s = Script::new(u32::MAX, 0);
        let (r, seen) = go(&s, fast()).await;
        r.unwrap();
        assert!(s.calls().iter().filter(|c| *c == "wsl -l -v").count() > 2);
        assert!(s.calls().contains(&"podman machine start".to_string()));
        assert!(seen.contains(&(1, "done")));
    }

    #[tokio::test]
    async fn podman_info_that_never_answers_fails_step_4_after_the_timeout() {
        let s = Script::new(0, u32::MAX);
        let started = std::time::Instant::now();
        let (r, seen) = go(&s, fast()).await;
        let err = format!("{:#}", r.unwrap_err());
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert!(err.contains("ssh: rejected"), "{err}");
        assert_eq!(seen.last(), Some(&(3, "failed")));
        assert!(!seen.contains(&(4, "running")));
    }

    #[tokio::test]
    async fn machine_start_already_running_is_fine_but_a_real_failure_stops_the_fix() {
        let s = Script { start_ok: false, start_text: "Error: unable to start \"podman-machine-default\": already running", ..Script::new(0, 0) };
        go(&s, fast()).await.0.unwrap();
        let s = Script { start_ok: false, start_text: "Error: wsl bootstrap failed", ..Script::new(0, 0) };
        let (r, seen) = go(&s, fast()).await;
        assert!(format!("{:#}", r.unwrap_err()).contains("wsl bootstrap failed"));
        assert_eq!(seen.last(), Some(&(2, "failed")));
        assert!(!s.calls().contains(&"podman info".to_string()));
    }
}
