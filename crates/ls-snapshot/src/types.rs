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
    /// Round 17: the full per-folder breakdown for a multi-folder send (the
    /// developer's real case — several independent project folders sent
    /// together). Empty for every snapshot built before round 17, and for
    /// any snapshot still built via the original single-folder
    /// `create_snapshot` (which never populates this) — `#[serde(default)]`
    /// so an old manifest missing this field entirely still deserializes
    /// cleanly, and old code reading a new manifest just ignores the extra
    /// field, both by construction of how serde already handles this struct
    /// (no `deny_unknown_fields` anywhere on it). `git_commit`/
    /// `git_parent_commit` above stay populated with the *first* folder's
    /// values for a multi-folder send, so anything reading only those two
    /// top-level fields keeps working exactly as before; this is the
    /// complete, unambiguous breakdown for anything that needs more.
    #[serde(default)]
    pub folders: Vec<FolderInfo>,
    /// Round 17: `{folder, schema, dump_file, hash}` for every database dump
    /// this snapshot carries — either a developer-supplied dump file used
    /// as-is, or a freshly exported (full-table, never sampled) live dump.
    /// `dump_file` is the path inside the payload tar
    /// (`db-dumps/<folder>/<schema>.sql`); `hash` is the sha256 hex of that
    /// exact file's bytes, so a receiver can verify it without trusting the
    /// tar entry's metadata. Empty for anything without a database, and for
    /// every pre-round-17 manifest — same `#[serde(default)]` backward/
    /// forward compatibility reasoning as `folders` above.
    #[serde(default)]
    pub database_dumps: Vec<DatabaseDumpEntry>,
}

/// One folder's identity within a multi-folder snapshot — see
/// [`Manifest::folders`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FolderInfo {
    /// The folder's label inside the payload tar (its basename, de-duplicated
    /// if two selected folders share one — see `create_snapshot_multi`).
    pub name: String,
    pub git_commit: String,
    pub git_parent_commit: Option<String>,
}

/// Round 17 only ever produced MySQL dumps, so an older manifest missing
/// `DatabaseDumpEntry.engine` entirely is unambiguously that - a real,
/// correct backward-compatible default, not a guess.
fn default_dump_engine() -> String {
    "mysql".to_string()
}

/// One database dump packaged into a multi-folder snapshot — see
/// [`Manifest::database_dumps`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DatabaseDumpEntry {
    /// Matches a [`FolderInfo::name`] in the same manifest's `folders`.
    pub folder: String,
    /// The database/schema name this dump is of.
    pub schema: String,
    /// Path inside the payload tar, e.g. "db-dumps/orders-service/orders.sql".
    pub dump_file: String,
    /// sha256 hex of the dump file's exact bytes.
    pub hash: String,
    /// Round 18: one of `ls_dbsource::SUPPORTED_ENGINES` ("mysql",
    /// "postgres", "mongodb") - a future restore step needs to know which
    /// tool a given dump file needs (`psql`/`mysql` for a plain-SQL dump vs
    /// `mongorestore` for MongoDB's own tar'd BSON directory dump), and
    /// `dump_file`'s extension alone doesn't reliably say (both MySQL's
    /// and PostgreSQL's dumps are plain `.sql` text). `#[serde(default =
    /// "default_dump_engine")]` so a round-17 manifest (MySQL-only,
    /// pre-dates this field) still deserializes correctly rather than
    /// erroring or defaulting to an empty string.
    #[serde(default = "default_dump_engine")]
    pub engine: String,
}

/// Files LocalSync itself generated for a project that shipped without a
/// `docker-compose.yml` (the compose wizard). Bundled *instead of* looking for
/// them in the project: the sender's folder is never modified, and nothing
/// generated has to be committed to git to reach the receiver.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GeneratedFiles {
    /// The whole `docker-compose.yml`. Shipped where a project's own compose
    /// file would be, so the receiver's Run path finds it unchanged.
    pub compose_yaml: String,
    /// Extra files (a Dockerfile, ...), as (path relative to the project
    /// root, bytes). Shipped under `source/` - i.e. exactly where they would
    /// sit if they had been committed - so a compose `build:` context of "."
    /// finds them.
    pub files: Vec<(String, Vec<u8>)>,
}

/// Where a [`PendingDump`]'s content actually lives.
///
/// A real database dump can be multi-GB — reading one fully into memory just
/// to hand it to `create_snapshot_multi` (as the original `Vec<u8>`-only
/// shape forced every caller to do) is exactly what produced a genuine
/// `out of memory` failure against a real, large dump. `FilePath` lets the
/// command layer hand over a path instead and defer any actual reading to
/// the point the bytes are hashed/tar'd, where it can be done in bounded
/// chunks rather than one giant allocation. `Bytes` is kept for callers (and
/// this crate's own unit tests) that already have the content in memory —
/// e.g. a small dump, or a synthetic one built in a test — with no reason to
/// round-trip it through a temp file first.
#[derive(Debug, Clone)]
pub enum DumpSource {
    Bytes(Vec<u8>),
    FilePath(std::path::PathBuf),
}

/// A database dump ready to be packaged by [`crate::create_snapshot_multi`] —
/// produced either by `ls-dbsource`'s live export or by reading a developer-
/// supplied dump file as-is. Not part of the wire format itself (see
/// [`DatabaseDumpEntry`] for that); this is just the input side, kept in
/// `ls-snapshot` (rather than `ls-dbsource`, which has no reason to know
/// about snapshots/manifests at all) so the command layer has one obvious
/// place to build it from either source.
#[derive(Debug, Clone)]
pub struct PendingDump {
    /// Index into the `folders` slice passed to `create_snapshot_multi` —
    /// not a name, so the caller never has to duplicate this crate's own
    /// folder-label de-duplication logic to know what to pass here.
    pub folder_index: usize,
    pub schema: String,
    /// One of `ls_dbsource::SUPPORTED_ENGINES` - see
    /// `DatabaseDumpEntry::engine`'s doc comment for why this needs to
    /// travel with the dump rather than being inferred later.
    pub engine: String,
    pub source: DumpSource,
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
