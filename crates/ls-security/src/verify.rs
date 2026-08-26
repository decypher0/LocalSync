//! Signature verification — the only door into a [`VerifiedSnapshot`].
//!
//! `VerifiedSnapshot` wraps a `Snapshot` behind a private field, so the only
//! way anything downstream (diff rendering, `ls-containers`) can obtain one
//! is by going through [`verify`]. There is no escape hatch: no `pub`
//! constructor, no `Default`, nothing.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use ls_snapshot::Snapshot;
use sha2::{Digest, Sha256};

/// A `Snapshot` that has passed signature verification. The inner field is
/// private and only constructible from within this crate (see [`verify`]),
/// so holding one of these is a compile-time proof the signature checked out.
pub struct VerifiedSnapshot(Snapshot);

impl VerifiedSnapshot {
    pub fn snapshot(&self) -> &Snapshot {
        &self.0
    }
}

/// Verify `snapshot`'s ed25519 signature and sender against `trusted_keys`.
///
/// The signed message is `sha256(json(manifest)) || sha256(payload)` — two
/// concatenated 32-byte digests — signed by `manifest.sender_pubkey`. This
/// must match `ls-snapshot`'s signer exactly; it is not renegotiable here.
///
/// `trusted_keys`: pass the set of pubkeys the caller already trusts. An
/// **empty slice means trust-on-first-use** — any key that produces a valid
/// signature is accepted, since there's no keyring yet to check against.
/// This is a deliberate MVP tradeoff, not a hidden default: callers that
/// want real trust pinning must pass a non-empty list, and the "accept
/// anyone" behavior only happens when they explicitly pass `&[]`.
pub fn verify(snapshot: Snapshot, trusted_keys: &[[u8; 32]]) -> anyhow::Result<VerifiedSnapshot> {
    if !trusted_keys.is_empty() && !trusted_keys.contains(&snapshot.manifest.sender_pubkey) {
        anyhow::bail!("sender_pubkey is not in trusted_keys");
    }

    let verifying_key = VerifyingKey::from_bytes(&snapshot.manifest.sender_pubkey)
        .map_err(|e| anyhow::anyhow!("sender_pubkey is not a valid ed25519 key: {e}"))?;

    let manifest_digest = Sha256::digest(serde_json::to_vec(&snapshot.manifest)?);
    let payload_digest = Sha256::digest(&snapshot.payload);
    let mut message = [0u8; 64];
    message[..32].copy_from_slice(&manifest_digest);
    message[32..].copy_from_slice(&payload_digest);

    let signature = Signature::from_bytes(&snapshot.signature);
    verifying_key
        .verify(&message, &signature)
        .map_err(|e| anyhow::anyhow!("signature verification failed: {e}"))?;

    Ok(VerifiedSnapshot(snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use ls_snapshot::Manifest;
    use rand::rngs::OsRng;

    fn build_signed_snapshot(payload: Vec<u8>) -> (Snapshot, SigningKey) {
        let signing_key = SigningKey::generate(&mut OsRng);
        let manifest = Manifest {
            project_name: "demo".into(),
            git_commit: "deadbeef".into(),
            git_parent_commit: None,
            dependency_lock_hash: "0".repeat(64),
            db_seed_hash: "0".repeat(64),
            services: vec![],
            sender_pubkey: signing_key.verifying_key().to_bytes(),
            created_at: time::OffsetDateTime::now_utc(),
        };

        let manifest_digest = Sha256::digest(serde_json::to_vec(&manifest).unwrap());
        let payload_digest = Sha256::digest(&payload);
        let mut message = [0u8; 64];
        message[..32].copy_from_slice(&manifest_digest);
        message[32..].copy_from_slice(&payload_digest);
        let signature = signing_key.sign(&message).to_bytes();

        (
            Snapshot {
                manifest,
                signature,
                payload,
            },
            signing_key,
        )
    }

    #[test]
    fn verify_accepts_valid_signature_tofu() {
        let (snapshot, _key) = build_signed_snapshot(b"payload bytes".to_vec());
        assert!(verify(snapshot, &[]).is_ok());
    }

    #[test]
    fn verify_accepts_when_key_is_trusted() {
        let (snapshot, key) = build_signed_snapshot(b"payload bytes".to_vec());
        let trusted = [key.verifying_key().to_bytes()];
        assert!(verify(snapshot, &trusted).is_ok());
    }

    #[test]
    fn verify_rejects_untrusted_key() {
        let (snapshot, _key) = build_signed_snapshot(b"payload bytes".to_vec());
        let other_key = SigningKey::generate(&mut OsRng);
        let trusted = [other_key.verifying_key().to_bytes()];
        assert!(verify(snapshot, &trusted).is_err());
    }

    #[test]
    fn verify_rejects_tampered_payload() {
        let (mut snapshot, _key) = build_signed_snapshot(b"payload bytes".to_vec());
        snapshot.payload = b"tampered bytes!".to_vec();
        assert!(verify(snapshot, &[]).is_err());
    }

    #[test]
    fn verify_rejects_tampered_manifest() {
        let (mut snapshot, _key) = build_signed_snapshot(b"payload bytes".to_vec());
        snapshot.manifest.project_name = "evil".into();
        assert!(verify(snapshot, &[]).is_err());
    }
}
