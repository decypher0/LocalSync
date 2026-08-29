//! Installs the standard `log` facade's global logger once at startup, so
//! every `log::info!`/`log::warn!` call from anywhere in the app — this
//! crate and the ls-snapshot/ls-net crates it depends on, which emit them
//! for each real stage of the Send/Receive flow (bundling, each git
//! command, signing, connecting to signaling, offer/answer, ICE gathering,
//! data channel open, transfer progress, done) — lands in one plain text
//! file, appended, timestamped, meant to be pasted.
//!
//! Same convention as `ls_containers::ProvisioningLog`
//! (`{OS data dir}/localsync/logs/provisioning.log`), for the same reason:
//! a developer hitting a hang or failure on their own machine can paste the
//! log and have it mean something without anyone needing to reproduce their
//! setup. Not literally reused — `ProvisioningLog` lives in `ls-containers`,
//! which the send/receive path doesn't and shouldn't depend on — but this is
//! deliberately a *different* file (`send.log`, not `provisioning.log`)
//! since it's a different concern, installed once here instead of at every
//! call site.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

use log::{Level, Log, Metadata, Record};

struct SendLog {
    path: PathBuf,
    // Not protecting any in-memory state - just serializes concurrent
    // open+append calls so two log lines from different tasks can't
    // interleave mid-line.
    write_lock: Mutex<()>,
}

impl Log for SendLog {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "[{}] [{}] {}: {}\n",
            time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_else(|_| "unknown-time".to_string()),
            record.level(),
            record.target(),
            record.args(),
        );
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let result = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
        if let Err(e) = result {
            eprintln!("send log write failed ({e}): {line}");
        }
    }

    fn flush(&self) {}
}

/// Installs the global logger at `{OS data dir}/localsync/logs/send.log`
/// (e.g. `%APPDATA%\localsync\logs\send.log` on Windows,
/// `~/.local/share/localsync/logs/send.log` on Linux, via the `dirs` crate —
/// same lookup `ProvisioningLog::open_default` uses). Call once, as early in
/// `main()` as possible, so every later stage is captured. If the OS data
/// dir can't be determined, logging is silently disabled (stderr note only)
/// rather than failing app startup over a diagnostics feature.
pub fn install_default() {
    let Some(base) = dirs::data_dir() else {
        eprintln!("send log: could not determine the OS data directory — logging disabled");
        return;
    };
    let dir = base.join("localsync").join("logs");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("send log: failed to create {}: {e}", dir.display());
        return;
    }
    install_at(dir.join("send.log"));
}

/// Same as [`install_default`] but at a caller-chosen path — used by
/// `tests/send_flow_test.rs` to capture real log output into a tempdir
/// instead of touching the real machine's app-data directory.
pub fn install_at(path: PathBuf) {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("send log: failed to create {}: {e}", parent.display());
        }
    }
    let logger = SendLog { path, write_lock: Mutex::new(()) };
    // `set_boxed_logger` errors if a logger is already installed. That's a
    // fine no-op here (the process already has send logging on) rather than
    // something worth panicking over - e.g. two test functions in the same
    // test binary both calling this.
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}
