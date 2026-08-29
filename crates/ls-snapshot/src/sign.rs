use anyhow::{Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::PathBuf;

use crate::types::Manifest;

fn home_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    let var = "USERPROFILE";
    #[cfg(not(windows))]
    let var = "HOME";
    std::env::var_os(var)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("could not determine home directory (${var} unset)"))
}

fn identity_path() -> Result<PathBuf> {
    Ok(home_dir()?.join(".localsync").join("identity.key"))
}

/// Loads the sender's persistent ed25519 identity from `~/.localsync/identity.key`,
/// generating and persisting a new one on first run. Uses `create_new` so two
/// concurrent first-run callers can't corrupt each other's key file; whichever
/// loses the race just reads back what the winner wrote.
pub fn load_or_create_identity() -> Result<SigningKey> {
    let path = identity_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            let key = SigningKey::generate(&mut rand::rngs::OsRng);
            file.write_all(key.as_bytes())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            log::info!("signing: created new sender identity at {}", path.display());
            Ok(key)
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let arr: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("identity key file {} is corrupt (expected 32 bytes)", path.display()))?;
            log::info!("signing: loaded existing sender identity from {}", path.display());
            Ok(SigningKey::from_bytes(&arr))
        }
        Err(e) => Err(e).with_context(|| format!("creating {}", path.display())),
    }
}

/// Signs `sha256(manifest_json) || sha256(payload)` (64 bytes) — the exact
/// scheme the receiver-side verifier expects. Do not change without
/// coordinating with ls-security.
pub fn sign_manifest(key: &SigningKey, manifest: &Manifest, payload: &[u8]) -> Result<[u8; 64]> {
    let manifest_bytes = serde_json::to_vec(manifest)?;
    let mut msg = [0u8; 64];
    msg[..32].copy_from_slice(&Sha256::digest(&manifest_bytes));
    msg[32..].copy_from_slice(&Sha256::digest(payload));
    let sig: Signature = key.sign(&msg);
    Ok(sig.to_bytes())
}

/// Verifies a `Snapshot`'s signature against `manifest.sender_pubkey`. Not
/// part of the receiver's trust path (ls-security owns that) — used here for
/// our own round-trip tests.
#[cfg_attr(not(test), allow(dead_code))]
pub fn verify_signature(manifest: &Manifest, payload: &[u8], signature: &[u8; 64]) -> Result<bool> {
    let manifest_bytes = serde_json::to_vec(manifest)?;
    let mut msg = [0u8; 64];
    msg[..32].copy_from_slice(&Sha256::digest(&manifest_bytes));
    msg[32..].copy_from_slice(&Sha256::digest(payload));
    let vk = VerifyingKey::from_bytes(&manifest.sender_pubkey)?;
    let sig = Signature::from_bytes(signature);
    Ok(vk.verify(&msg, &sig).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ServiceDef;

    fn dummy_manifest(pubkey: [u8; 32]) -> Manifest {
        Manifest {
            project_name: "demo".into(),
            git_commit: "abc123".into(),
            git_parent_commit: None,
            dependency_lock_hash: "deadbeef".into(),
            db_seed_hash: "deadbeef".into(),
            services: vec![ServiceDef {
                name: "web".into(),
                image_or_build: "image:nginx".into(),
                ports: vec!["8080:80".into()],
                depends_on: vec![],
            }],
            sender_pubkey: pubkey,
            created_at: time::OffsetDateTime::now_utc(),
        }
    }

    #[test]
    fn sign_and_verify_round_trip() {
        let key = SigningKey::generate(&mut rand::rngs::OsRng);
        let manifest = dummy_manifest(key.verifying_key().to_bytes());
        let payload = b"pretend tar.gz bytes".to_vec();

        let sig = sign_manifest(&key, &manifest, &payload).unwrap();
        assert!(verify_signature(&manifest, &payload, &sig).unwrap());

        // Tampered payload must fail verification.
        let mut bad_payload = payload.clone();
        bad_payload.push(0xff);
        assert!(!verify_signature(&manifest, &bad_payload, &sig).unwrap());
    }

    #[test]
    fn load_or_create_identity_persists_across_calls() {
        // Exercises the real ~/.localsync/identity.key path (create-if-missing,
        // reuse-if-present). Idempotent and harmless to run repeatedly.
        let a = load_or_create_identity().unwrap();
        let b = load_or_create_identity().unwrap();
        assert_eq!(a.to_bytes(), b.to_bytes());
    }
}
