//! Reads `diff_stat.json` out of a verified snapshot's payload for the
//! "review before you run" screen. This only ever runs against a
//! [`VerifiedSnapshot`], so the payload has already passed signature check —
//! nothing here executes anything or unpacks `source/`.

use crate::verify::VerifiedSnapshot;
use anyhow::Context;
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

/// Unpacks `diff_stat.json` from the verified snapshot's tar.gz payload and
/// summarizes it. Does not touch `source/`, `docker-compose.yml`, or
/// `db-seed/` — those are `ls-containers`' concern once a human clicks run.
///
/// Round 17: a multi-folder snapshot (`ls_snapshot::create_snapshot_multi`)
/// has no single top-level `diff_stat.json` — each folder ships its own, at
/// `<folder>/diff_stat.json` (see `Manifest::folders`). Without this,
/// receiving *any* multi-folder snapshot would hard-error right here before
/// a human ever saw a review screen, which would make the whole wizard
/// feature unusable end to end, not just "execution deferred" the way the
/// round's own scope intends — so this reads every folder's diff_stat.json
/// and merges them, prefixing each entry's `path` with its folder name so
/// the review screen can tell which project a changed file belongs to.
/// Single-folder snapshots (everything before round 17, and
/// `create_snapshot`'s own output today) are checked first, and produce the
/// exact same output as before — this is purely additive.
pub fn diff_summary(verified: &VerifiedSnapshot) -> anyhow::Result<DiffSummary> {
    let snapshot = verified.snapshot();
    // Round 36: `snapshot.payload` is zstd now, not gzip - see
    // `ls_snapshot::bundle`'s own switch.
    let decoder = zstd::stream::read::Decoder::new(&snapshot.payload[..])
        .context("initializing zstd decoder for snapshot payload")?;
    let mut archive = Archive::new(decoder);
    let mut files: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().into_owned();
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        files.insert(path, buf);
    }

    if let Some(bytes) = files.get("diff_stat.json") {
        let entries: Vec<DiffEntry> = serde_json::from_slice(bytes)?;
        return Ok(summarize(entries));
    }

    if !snapshot.manifest.folders.is_empty() {
        let mut merged = Vec::new();
        for folder in &snapshot.manifest.folders {
            let key = format!("{}/diff_stat.json", folder.name);
            let Some(bytes) = files.get(&key) else {
                anyhow::bail!("payload is missing {key}");
            };
            let entries: Vec<DiffEntry> = serde_json::from_slice(bytes)?;
            merged.extend(entries.into_iter().map(|e| DiffEntry {
                path: format!("{}/{}", folder.name, e.path),
                ..e
            }));
        }
        return Ok(summarize(merged));
    }

    anyhow::bail!("payload does not contain diff_stat.json")
}

fn summarize(entries: Vec<DiffEntry>) -> DiffSummary {
    let total_insertions = entries.iter().map(|e| e.insertions).sum();
    let total_deletions = entries.iter().map(|e| e.deletions).sum();
    DiffSummary { entries, total_insertions, total_deletions }
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

        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
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
            folders: vec![],
            database_dumps: vec![],
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
