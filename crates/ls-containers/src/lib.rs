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
    /// Directory holding the unpacked snapshot + rewritten compose file.
    /// Not part of the public contract data-wise, but `stop_session` needs
    /// it as the cwd for `podman-compose down` to find the same project.
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
    match ProvisioningLog::open_default() {
        Ok(log) => provisioning::ensure_podman_ready(&log).await?,
        Err(e) => {
            eprintln!("provisioning log unavailable ({e:#}), continuing without one");
            anyhow::ensure!(
                podman::podman_available() && podman::podman_compose_available(),
                "podman/podman-compose not found on PATH"
            );
        }
    }

    let manifest = &verified.snapshot().manifest;

    let subdir = format!(
        "{}-{}",
        sanitize(&manifest.project_name),
        sanitize(&manifest.git_commit)
    );
    let compose_dir = work_dir.join(subdir);
    unpack_payload(&verified.snapshot().payload, &compose_dir).await?;

    let compose_path = compose_dir.join("docker-compose.yml");
    let original_yaml = tokio::fs::read_to_string(&compose_path)
        .await
        .context("snapshot payload has no docker-compose.yml")?;

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
    podman::compose_up(&compose_dir, &compose_project_name).await?;

    Ok(RunningSession {
        project_name: manifest.project_name.clone(),
        compose_project_name,
        service_ports,
        db_cache_hit,
        compose_dir,
    })
}

/// Tear down this run's containers/network. The named DB volume is
/// intentionally left alone — it's the cache.
pub async fn stop_session(session: &RunningSession) -> Result<()> {
    podman::compose_down(&session.compose_dir, &session.compose_project_name).await
}

async fn unpack_payload(payload: &[u8], dest: &Path) -> Result<()> {
    let payload = payload.to_vec();
    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<()> {
        std::fs::create_dir_all(&dest)?;
        let gz = flate2::read::GzDecoder::new(payload.as_slice());
        tar::Archive::new(gz).unpack(&dest)?;
        Ok(())
    })
    .await
    .context("unpack task panicked")??;
    Ok(())
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

        let mut encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
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
}
