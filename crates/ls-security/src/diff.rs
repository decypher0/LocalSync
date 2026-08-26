//! Reads `diff_stat.json` out of a verified snapshot's payload for the
//! "review before you run" screen. This only ever runs against a
//! [`VerifiedSnapshot`], so the payload has already passed signature check —
//! nothing here executes anything or unpacks `source/`.

use crate::verify::VerifiedSnapshot;
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::io::Read;
use tar::Archive;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffEntry {
    pub path: String,
    pub change_type: String,
    pub insertions: u32,
    pub deletions: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffSummary {
    pub entries: Vec<DiffEntry>,
    pub total_insertions: u32,
    pub total_deletions: u32,
}

/// Unpacks just `diff_stat.json` from the verified snapshot's tar.gz payload
/// and summarizes it. Does not touch `source/`, `docker-compose.yml`, or
/// `db-seed/` — those are `ls-containers`' concern once a human clicks run.
pub fn diff_summary(verified: &VerifiedSnapshot) -> anyhow::Result<DiffSummary> {
    let payload = &verified.snapshot().payload;
    let mut archive = Archive::new(GzDecoder::new(&payload[..]));

    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.to_str() == Some("diff_stat.json") {
            let mut contents = String::new();
            entry.read_to_string(&mut contents)?;
            let entries: Vec<DiffEntry> = serde_json::from_str(&contents)?;
            let total_insertions = entries.iter().map(|e| e.insertions).sum();
            let total_deletions = entries.iter().map(|e| e.deletions).sum();
            return Ok(DiffSummary {
                entries,
                total_insertions,
                total_deletions,
            });
        }
    }

    anyhow::bail!("payload does not contain diff_stat.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::verify;
    use ed25519_dalek::{Signer, SigningKey};
    use ls_snapshot::Manifest;
    use rand::rngs::OsRng;
    use sha2::{Digest, Sha256};
    use std::io::Write;
    use tar::Builder;

    fn make_payload_with_diff_stat(json: &str) -> Vec<u8> {
        let mut builder = Builder::new(Vec::new());
        let data = json.as_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_cksum();
        builder
            .append_data(&mut header, "diff_stat.json", data)
            .unwrap();
        let tar_bytes = builder.into_inner().unwrap();

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar_bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn diff_summary_parses_and_totals() {
        let json = r#"[
            {"path": "src/main.rs", "change_type": "modified", "insertions": 10, "deletions": 2},
            {"path": "src/new.rs", "change_type": "added", "insertions": 30, "deletions": 0},
            {"path": "old.txt", "change_type": "deleted", "insertions": 0, "deletions": 5}
        ]"#;
        let payload = make_payload_with_diff_stat(json);

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

        let snapshot = ls_snapshot::Snapshot {
            manifest,
            signature,
            payload,
        };
        let verified = verify(snapshot, &[]).expect("valid signature should verify");

        let summary = diff_summary(&verified).expect("diff_stat.json should parse");
        assert_eq!(summary.entries.len(), 3);
        assert_eq!(summary.total_insertions, 40);
        assert_eq!(summary.total_deletions, 7);
        assert_eq!(summary.entries[0].change_type, "modified");
    }
}
