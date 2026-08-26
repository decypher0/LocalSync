use serde::{Deserialize, Serialize};

/// serde's built-in fixed-size-array support only goes up to 32 elements, so
/// `[u8; 64]` needs a manual (de)serializer — this changes nothing about the
/// field's Rust type or name, only how the derive macro encodes it.
mod signature_bytes {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let bytes = Vec::<u8>::deserialize(d)?;
        bytes
            .try_into()
            .map_err(|v: Vec<u8>| D::Error::custom(format!("expected 64 bytes, got {}", v.len())))
    }
}

/// One container/service a snapshot needs on the receiver side. `ls-containers`
/// reads this to know what to start; the actual `docker-compose.yml` ships
/// inside the snapshot payload verbatim, so nothing here re-implements
/// compose parsing — this is just enough for the UI/orchestrator to reason
/// about what's coming up without unpacking the payload first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDef {
    pub name: String,
    /// "build:./app" or "image:mysql:8.0"
    pub image_or_build: String,
    /// host:container, e.g. "8080:8080"
    pub ports: Vec<String>,
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub project_name: String,
    pub git_commit: String,
    pub git_parent_commit: Option<String>,
    /// sha256 of the dependency lockfile(s) (pom.xml/package-lock.json/etc) —
    /// keys the build-layer cache. Unchanged hash = unchanged deps = Podman's
    /// own layer cache does the work, nothing custom needed here.
    pub dependency_lock_hash: String,
    /// sha256 of the seed SQL/schema — keys the receiver-side DB volume so a
    /// second snapshot of the same project reuses the already-seeded volume.
    pub db_seed_hash: String,
    pub services: Vec<ServiceDef>,
    pub sender_pubkey: [u8; 32],
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
}

/// A snapshot as it travels over the wire / sits on disk.
///
/// `signature` is an ed25519 signature by `manifest.sender_pubkey` over
/// `sha256(manifest_json_bytes) || sha256(payload)`, so tampering with either
/// half invalidates it. `payload` is a tar.gz containing `source/` (the
/// diffed project tree), `docker-compose.yml`, and `db-seed/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub manifest: Manifest,
    #[serde(with = "signature_bytes")]
    pub signature: [u8; 64],
    pub payload: Vec<u8>,
}
