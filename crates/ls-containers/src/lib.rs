//! Turns a verified snapshot into running (sandboxed, ephemeral) containers.
//!
//! No custom compose parser, no custom image-layer cache: `podman-compose`
//! does the actual bring-up (shelled out to via `tokio::process`), and
//! Podman's own build/pull caches image layers by content hash for free.
//! The one thing this crate manages itself is the database data volume
//! (MySQL, Postgres, ...), named deterministically from
//! `manifest.db_seed_hash` so a second snapshot of the same project/seed
//! reuses the already-seeded volume instead of re-running init scripts.

mod compose;
mod podman;
mod provisioning;

pub use provisioning::{ensure_podman_ready, ProvisioningLog};

use anyhow::{Context, Result};
use ls_security::VerifiedSnapshot;
use std::path::{Path, PathBuf};

/// A live `podman-compose` project brought up from a verified snapshot.
pub struct RunningSession {
    pub project_name: String,
    /// Passed to `podman-compose -p`, so `stop_session` tears down only
    /// this run's containers/network.
    pub compose_project_name: String,
    /// (service_name, "host:container") pairs read from the (rewritten)
    /// `docker-compose.yml`, so the caller/UI knows what localhost port to
    /// hit.
    pub service_ports: Vec<(String, String)>,
    /// Whether the database data volume already existed before this run (a
    /// cache hit on `db_seed_hash`) vs. a cold start that had to re-seed.
    pub db_cache_hit: bool,
    /// Directory actually holding `docker-compose.yml` (rewritten) - the
    /// unpacked snapshot's own root for the older, pre-round-17 single-
    /// folder path, or one level down (`<unpacked root>/<folder label>`)
    /// for a round-17+ wizard send with exactly one folder, since
    /// `create_snapshot_multi` nests every folder's files under its own
    /// label unconditionally (see `run_snapshot`'s own comment on
    /// `compose_root`). Not part of the public contract data-wise, but
    /// `stop_session` needs it as the cwd for `podman-compose down` to find
    /// the same project.
    compose_dir: PathBuf,
}

/// Unpack `verified`'s payload into a fresh subdirectory of `work_dir`,
/// enforce `ls_security::default_policy()` on every service in its
/// `docker-compose.yml` (never trust the snapshot's own compose settings for
/// this), point any database service at the deterministic seed-hash-keyed
/// volume, and bring the project up via `podman-compose`.
pub async fn run_snapshot(verified: &VerifiedSnapshot, work_dir: &Path) -> Result<RunningSession> {
    // On Linux this is close to the old bare availability checks; on
    // Windows/macOS it actually attempts to provision Podman (installing it
    // and/or starting its VM) rather than just failing. Every step is
    // logged to ProvisioningLog::open_default()'s file regardless of
    // platform. Falls back to a stderr-only log if the OS data dir can't be
    // determined, rather than blocking the run over a logging problem.
    //
    // Kept for the rest of this function (not just this preflight check) so
    // `podman::compose_up` below can stream its own real, live output into
    // the same file - previously only the (near-instant, on Linux) preflight
    // checks ever wrote here, so a live-tailing UI had nothing to show
    // during the actual slow part of a run (round 12: root cause of the
    // "Show details" panel going silent almost immediately in real use).
    let log = match ProvisioningLog::open_default() {
        Ok(log) => {
            provisioning::ensure_podman_ready(&log).await?;
            Some(log)
        }
        Err(e) => {
            eprintln!("provisioning log unavailable ({e:#}), continuing without one");
            anyhow::ensure!(
                podman::podman_available() && podman::podman_compose_available(),
                "podman/podman-compose not found on PATH — run scripts/setup-linux-deps.sh"
            );
            None
        }
    };

    let manifest = &verified.snapshot().manifest;

    let subdir = format!(
        "{}-{}",
        sanitize(&manifest.project_name),
        sanitize(&manifest.git_commit)
    );
    let compose_dir = work_dir.join(subdir);
    unpack_payload(&verified.snapshot().payload, &compose_dir).await?;

    // Round 30: a real bug found in real cross-machine testing, root-caused
    // by directly reproducing it rather than guessed at - a project sent
    // through the Send wizard (`share_snapshot_wizard` ->
    // `ls_snapshot::create_snapshot_multi`, the only Send path since round
    // 20) fails here even when it has a perfectly real `docker-compose.yml`,
    // because `bundle::merge_folder_payloads` nests *every* folder's files
    // under its own `manifest.folders[i].name` label - unconditionally,
    // with no special case for exactly one folder - while this function
    // always looked for `docker-compose.yml` at the payload's own top
    // level. `manifest.folders` is empty only for the older, pre-round-17
    // `create_snapshot` single-folder path (see its own doc comment),
    // which never nests anything and keeps working unchanged below. A
    // genuinely multi-folder send (2+) has no single compose file to run
    // at all by design - round 17's own doc comment on
    // `create_snapshot_multi` is explicit that running several raw folders
    // together was deliberately out of scope - so that case still falls
    // through to the same "no docker-compose.yml" path as a genuinely
    // uncontainerized project, just with a real, actionable message now
    // instead of a bare internal-looking string.
    let compose_root = match manifest.folders.as_slice() {
        [only_folder] => compose_dir.join(&only_folder.name),
        _ => compose_dir.clone(),
    };

    let compose_path = compose_root.join("docker-compose.yml");
    let original_yaml = tokio::fs::read_to_string(&compose_path).await.context(
        "This project doesn't have a docker-compose.yml, so LocalSync doesn't know how to build \
         or run it yet. Add one to the project defining how to build and run it, then send again \
         (auto-generating one from the project's own framework is a real, separate future \
         capability - not something LocalSync does today; see docs/auto-containerization.md).",
    )?;

    let database_services = compose::database_service_names(manifest);
    let db_volume = compose::db_volume_name(&manifest.db_seed_hash);
    let policy = ls_security::default_policy();
    let rewritten = compose::apply_policy(&original_yaml, &policy, &database_services, &db_volume)?;
    tokio::fs::write(&compose_path, &rewritten).await?;

    let service_ports = compose::parse_service_ports(&rewritten)?;

    // Cache-hit check has to happen before `up` creates/touches the volume.
    let db_cache_hit = if database_services.is_empty() {
        false
    } else {
        podman::volume_exists(&db_volume).await.unwrap_or(false)
    };

    let compose_project_name = compose_project_name(&manifest.project_name, &manifest.git_commit);
    // podman-compose resolves docker-compose.yml (and any relative `build:`
    // context inside it) from its own current directory, not from an
    // explicit -f flag - compose_root, not the unpacked payload's own root,
    // is what must be passed here (and to compose_down, via
    // RunningSession.compose_dir below) whenever a single folder's own
    // files live nested one level down.
    podman::compose_up(&compose_root, &compose_project_name, log.as_ref()).await?;

    Ok(RunningSession {
        project_name: manifest.project_name.clone(),
        compose_project_name,
        service_ports,
        db_cache_hit,
        // compose_root, not the unpacked payload's own root - stop_session
        // must `cd` to the exact same directory compose_up just did, or
        // podman-compose down would look for docker-compose.yml (and this
        // run's own project state) in the wrong place.
        compose_dir: compose_root,
    })
}

/// Tear down this run's containers/network. The named DB volume is
/// intentionally left alone — it's the cache.
pub async fn stop_session(session: &RunningSession) -> Result<()> {
    // Best-effort logging only - a missing/unopenable log must never block
    // actually tearing the session down (same non-fatal fallback style
    // `run_snapshot` already uses above).
    let log = ProvisioningLog::open_default().ok();
    podman::compose_down(&session.compose_dir, &session.compose_project_name, log.as_ref()).await
}

async fn unpack_payload(payload: &[u8], dest: &Path) -> Result<()> {
    let payload = payload.to_vec();
    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<()> {
        std::fs::create_dir_all(&dest)?;
        // Round 36: matches ls-snapshot's own switch from gzip to zstd.
        let zstd = zstd::stream::read::Decoder::new(payload.as_slice())
            .context("initializing zstd decoder for snapshot payload")?;
        tar::Archive::new(zstd)
            .unpack(&dest)
            .map_err(|e| anyhow::anyhow!(describe_unpack_error(&e, &dest)))?;
        Ok(())
    })
    .await
    .context("unpack task panicked")??;
    Ok(())
}

/// `tar` reports every extraction failure as only "failed to unpack `<path>`"
/// - the real reason (disk full, permission denied, a corrupt/cut-off
/// stream) is nested one or two errors down, and shown alone that message
/// gives the person nothing to act on. This walks the whole chain, reports
/// the root cause, and adds a hint for the causes that have an obvious fix.
fn describe_unpack_error(e: &(dyn std::error::Error + 'static), dest: &Path) -> String {
    let mut root: &(dyn std::error::Error + 'static) = e;
    let mut kind = None;
    let mut cur = Some(e);
    while let Some(err) = cur {
        root = err;
        cur = match err.downcast_ref::<std::io::Error>() {
            // io::Error::source() skips the error it wraps (it returns that
            // error's own source), so descend via get_ref() to visit every
            // layer instead of stepping over one.
            Some(io) => {
                kind = Some(io.kind());
                io.get_ref().map(|inner| inner as &(dyn std::error::Error + 'static)).or_else(|| err.source())
            }
            None => err.source(),
        };
    }
    let hint = match kind {
        Some(std::io::ErrorKind::StorageFull) => {
            " - the disk is full. On some Linux systems /tmp is RAM-backed and small: set the              work directory to a folder on a disk with room for the project and its database dump."
        }
        Some(std::io::ErrorKind::PermissionDenied) => {
            " - LocalSync isn't allowed to write there, or a file from an earlier run belongs              to another user. Choose a different work directory, or delete that folder."
        }
        Some(std::io::ErrorKind::UnexpectedEof) | Some(std::io::ErrorKind::InvalidData) => {
            " - the received project data looks corrupted or cut off. Ask the sender to send it again."
        }
        _ => "",
    };
    // tar's own outer message is what names the file that failed; the root
    // cause says why. Both, unless they're the same text.
    let (outer, root) = (e.to_string(), root.to_string());
    if outer == root {
        format!("couldn't unpack the project into {}: {root}{hint}", dest.display())
    } else {
        format!("couldn't unpack the project into {}: {outer}: {root}{hint}", dest.display())
    }
}

/// Filesystem-path- and compose-project-name-safe: manifest fields are
/// signed but still attacker-influenceable content, not something to trust
/// blindly in a path. Collapses anything that isn't alnum/-/_ to '-', which
/// also kills path traversal ("../../etc" -> "-------etc").
fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "x".to_string()
    } else {
        cleaned
    }
}

fn compose_project_name(project_name: &str, git_commit: &str) -> String {
    let short_commit = &git_commit[..git_commit.len().min(12)];
    format!(
        "localsync-{}-{}",
        sanitize(project_name),
        sanitize(short_commit)
    )
    .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_path_traversal() {
        assert_eq!(sanitize("../../etc/passwd"), "------etc-passwd");
        assert_eq!(sanitize("my-project_1"), "my-project_1");
        assert_eq!(sanitize(""), "x");
    }

    #[test]
    fn compose_project_name_is_lowercase_and_truncates_commit() {
        let name = compose_project_name("Demo App", "deadbeefCAFE1234567890");
        assert_eq!(name, "localsync-demo-app-deadbeefcafe");
    }

    // --- Integration path: needs a real podman + podman-compose. Skips
    // (with a printed reason) rather than failing when they're not
    // installed, per the task: don't block on Podman being present, but
    // keep the path written and ready to run the moment it is.
    #[tokio::test]
    async fn run_and_stop_a_real_snapshot() {
        if !podman::podman_available() || !podman::podman_compose_available() {
            eprintln!(
                "skipping run_and_stop_a_real_snapshot: podman/podman-compose not found on PATH"
            );
            return;
        }

        let verified = build_verified_snapshot();
        let work_dir = tempfile::tempdir().unwrap();

        let session = run_snapshot(&verified, work_dir.path())
            .await
            .expect("run_snapshot should bring the project up");

        assert_eq!(session.project_name, "demo");
        assert!(!session.db_cache_hit, "first run should be a cold start");
        assert!(session
            .service_ports
            .iter()
            .any(|(svc, port)| svc == "web" && port == "8080:80"));

        stop_session(&session)
            .await
            .expect("stop_session should tear the project down");
    }

    fn build_verified_snapshot() -> VerifiedSnapshot {
        use ed25519_dalek::{Signer, SigningKey};
        use ls_snapshot::{Manifest, ServiceDef, Snapshot};
        use rand::rngs::OsRng;
        use sha2::{Digest, Sha256};
        use std::io::Write;

        let compose_yaml = r#"
services:
  web:
    image: docker.io/library/nginx:alpine
    ports:
      - "8080:80"
"#;

        let mut builder = tar::Builder::new(Vec::new());
        let data = compose_yaml.as_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_cksum();
        builder
            .append_data(&mut header, "docker-compose.yml", data)
            .unwrap();
        let tar_bytes = builder.into_inner().unwrap();

        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        encoder.write_all(&tar_bytes).unwrap();
        let payload = encoder.finish().unwrap();

        let signing_key = SigningKey::generate(&mut OsRng);
        let manifest = Manifest {
            project_name: "demo".into(),
            git_commit: "deadbeef".into(),
            git_parent_commit: None,
            dependency_lock_hash: "0".repeat(64),
            db_seed_hash: "0".repeat(64),
            services: vec![ServiceDef {
                name: "web".into(),
                image_or_build: "image:nginx:alpine".into(),
                ports: vec!["8080:80".into()],
                depends_on: vec![],
            }],
            sender_pubkey: signing_key.verifying_key().to_bytes(),
            created_at: time::OffsetDateTime::now_utc(),
            folders: vec![],
            database_dumps: vec![],
        };

        let manifest_digest = Sha256::digest(serde_json::to_vec(&manifest).unwrap());
        let payload_digest = Sha256::digest(&payload);
        let mut message = [0u8; 64];
        message[..32].copy_from_slice(&manifest_digest);
        message[32..].copy_from_slice(&payload_digest);
        let signature = signing_key.sign(&message).to_bytes();

        let snapshot = Snapshot {
            manifest,
            signature,
            payload,
        };
        ls_security::verify(snapshot, &[]).expect("test snapshot should verify")
    }

    /// Unpacking a payload that carries a real SQL dump, built by the real
    /// `create_snapshot_multi` (not a hand-rolled tar), byte-exact - and a
    /// second unpack into the same directory, which is what a retried Run
    /// does (the snapshot is put back after a failed Run, then unpacked into
    /// the same `<project>-<commit>` dir again). Dump size is
    /// `LS_DUMP_TEST_MB` (default small) so size-dependent problems can be
    /// reproduced without editing the test.
    #[tokio::test]
    async fn unpack_payload_extracts_a_real_sql_dump_exactly_and_can_rerun_into_the_same_dir() {
        use sha2::{Digest, Sha256};
        use std::io::Write;
        use std::process::Command;

        let mb: u64 = std::env::var("LS_DUMP_TEST_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(16);
        let base = tempfile::tempdir().unwrap();
        let project = base.path().join("xusom-admin");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("docker-compose.yml"), "services: {}
").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["add", "-A"],
            vec!["-c", "user.name=T", "-c", "user.email=t@example.com", "commit", "-q", "-m", "init"],
        ] {
            assert!(Command::new("git").args(&args).current_dir(&project).status().unwrap().success());
        }

        let dump_path = base.path().join("xusom.sql");
        {
            let mut f = std::io::BufWriter::new(std::fs::File::create(&dump_path).unwrap());
            let (mut written, mut i) = (0u64, 0u64);
            while written < mb * 1024 * 1024 {
                let line = format!("INSERT INTO `users` VALUES ({i}, 'user{i}@example.com', 'row {}');
", i.wrapping_mul(2654435761) % 1_000_003);
                f.write_all(line.as_bytes()).unwrap();
                written += line.len() as u64;
                i += 1;
            }
        }
        let expected = Sha256::digest(std::fs::read(&dump_path).unwrap());

        let snapshot = ls_snapshot::create_snapshot_multi(
            &[ls_snapshot::FolderSpec { path: project.clone(), parent_commit: None }],
            &[ls_snapshot::PendingDump {
                folder_index: 0,
                schema: "xusom".to_string(),
                source: ls_snapshot::DumpSource::FilePath(dump_path.clone()),
                engine: "mysql".to_string(),
            }],
        )
        .expect("create_snapshot_multi");

        let dest = base.path().join("work").join("xusom-admin-abc");
        for attempt in 1..=2 {
            unpack_payload(&snapshot.payload, &dest)
                .await
                .unwrap_or_else(|e| panic!("unpack attempt {attempt} failed: {e:#}"));
            let unpacked = dest.join("db-dumps/xusom-admin/xusom.sql");
            assert_eq!(
                Sha256::digest(std::fs::read(&unpacked).unwrap()),
                expected,
                "unpacked dump differs from the original after attempt {attempt}"
            );
        }
    }

    #[test]
    fn describe_unpack_error_reports_the_root_cause_and_a_hint_not_just_tars_wrapper() {
        // tar nests the real io error inside its own wrappers; any nesting
        // reproduces the shape.
        let leaf = std::io::Error::new(std::io::ErrorKind::StorageFull, "No space left on device (os error 28)");
        let wrapped = std::io::Error::other(std::io::Error::other(leaf));
        let msg = describe_unpack_error(&wrapped, Path::new("/tmp/localsync-work/p-abc"));
        assert!(msg.contains("/tmp/localsync-work/p-abc"), "{msg}");
        assert!(msg.contains("No space left on device"), "root cause missing: {msg}");
        assert!(msg.contains("disk is full"), "hint missing: {msg}");
    }

    #[tokio::test]
    async fn unpack_failure_surfaces_the_underlying_cause() {
        // A directory squatting where the dump file has to go makes tar's
        // extraction fail for a real, non-space reason. The message must
        // say why - not just name the file.
        let dest = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dest.path().join("db-dumps/xusom-admin/xusom.sql/blocker")).unwrap();
        let mut tb = tar::Builder::new(zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap());
        let data = b"INSERT INTO t VALUES (1);";
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        tb.append_data(&mut header, "db-dumps/xusom-admin/xusom.sql", &data[..]).unwrap();
        let payload = tb.into_inner().unwrap().finish().unwrap();

        let err = unpack_payload(&payload, dest.path()).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("xusom.sql"), "{msg}");
        assert!(
            msg.matches(':').count() >= 2 && !msg.trim_end().ends_with("xusom.sql"),
            "message names the file but not why it failed: {msg}"
        );
    }
}
