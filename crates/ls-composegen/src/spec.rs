//! The contract every part of the compose wizard builds against: what the
//! wizard collects ([`ComposeSpec`]), what validation says about it
//! ([`FieldError`]), and what generation produces ([`Generated`]).
//!
//! These types cross the JS/Rust boundary as JSON, so they follow this
//! project's existing convention: **field names are snake_case exactly as
//! written here** (nested struct fields get no camelCase conversion, unlike
//! top-level Tauri command argument names), and enums serialize as the
//! lowercase strings noted on each variant.

use serde::{Deserialize, Serialize};

/// Name of the app's service in the generated compose file.
pub const APP_SERVICE: &str = "app";

/// The generated Dockerfile's path relative to the project root. Fixed, and
/// deliberately not `Dockerfile`, so it can never collide with (or silently
/// replace) a Dockerfile the project already has.
pub const DOCKERFILE_NAME: &str = "Dockerfile.localsync";

/// Credentials for generated database containers. Throwaway values, like the
/// sample projects': the database only ever runs on the receiver's own
/// machine, and the person's real credentials are never sent.
pub const DB_USER: &str = "localsync";
pub const DB_PASSWORD: &str = "localsync_pw";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    Java,
    Node,
    Python,
    Go,
}

/// The build tool determines the build command - the wizard never asks for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildTool {
    Maven,
    Gradle,
    Npm,
    Yarn,
    Pnpm,
    Pip,
    /// `go build` with Go modules. Serializes as `"go"`.
    #[serde(rename = "go")]
    GoBuild,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DbEngine {
    Mysql,
    Postgres,
    Mongodb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtraKind {
    Redis,
    Rabbitmq,
    Memcached,
}

/// How the app reads its database connection. There is no way to know that
/// without asking (it varies by framework), so it's a fixed choice rather
/// than a guess or free text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DbEnvPreset {
    /// `DB_HOST`, `DB_PORT`, `DB_NAME`, `DB_USER`, `DB_PASSWORD`, `DATABASE_URL`.
    #[default]
    Standard,
    /// `SPRING_DATASOURCE_URL` / `_USERNAME` / `_PASSWORD` (or
    /// `SPRING_DATA_MONGODB_URI` for MongoDB).
    Spring,
    /// Inject nothing; the person sets connection variables themselves.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseSpec {
    pub engine: DbEngine,
    /// A version from the catalog for this engine (e.g. "8.0").
    pub version: String,
    /// The database/schema name created in the container. An identifier:
    /// letters, digits and underscores, not starting with a digit.
    pub database: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtraService {
    pub kind: ExtraKind,
    /// A version from the catalog for this service.
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
}

/// Everything the wizard collects for one project folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposeSpec {
    pub runtime: Runtime,
    /// A version from the catalog for `runtime` (e.g. "21", "3.12").
    pub runtime_version: String,
    /// Must be one the catalog lists for `runtime`.
    pub build_tool: BuildTool,
    /// How to start the app inside its container. Required and non-empty;
    /// pre-filled from the catalog where the runtime/tool implies one.
    pub run_command: String,
    /// The port the app listens on. The container's and the host's port are
    /// the same (see [`GenerateContext::host_port`] for the one exception).
    pub port: u16,
    /// Where the build leaves the artifact to run, relative to the project
    /// root (e.g. `target/*.jar`; `*` allowed). Required for Java, where it
    /// makes `java -jar` unambiguous; ignored for other runtimes.
    #[serde(default)]
    pub artifact_path: Option<String>,
    #[serde(default)]
    pub database: Option<DatabaseSpec>,
    #[serde(default)]
    pub db_env_preset: DbEnvPreset,
    #[serde(default)]
    pub extras: Vec<ExtraService>,
    /// Extra environment for the app, as a structured list.
    #[serde(default)]
    pub env: Vec<EnvVar>,
}

/// One thing wrong with a [`ComposeSpec`], phrased for the person filling in
/// the form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldError {
    /// Which field, in the form's own terms: `runtime`, `runtime_version`,
    /// `build_tool`, `run_command`, `port`, `artifact_path`, `database`,
    /// `database.version`, `database.database`, `extras[<i>].version`,
    /// `env[<i>].key`, `env[<i>].value`, ...
    pub field: String,
    /// Specific and actionable: says what's wrong *and* what would fix it.
    pub message: String,
}

/// What generation needs to know about where the project's files will be and
/// what will be alongside them at run time.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GenerateContext {
    /// The folder's label inside the snapshot payload (see
    /// `ls_snapshot::folder_labels`). The database dump, if any, is at
    /// `../db-dumps/<folder_label>/<file_name>` relative to the compose file.
    pub folder_label: String,
    /// The database dump being sent with this folder, if any.
    pub dump: Option<DumpInfo>,
    /// Publish the app on this host port instead of `spec.port`. Only the
    /// test-run uses this (the sender's own dev server may already be on the
    /// app's port); what is sent never sets it.
    #[serde(default)]
    pub host_port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DumpInfo {
    pub engine: DbEngine,
    /// The dump's file name inside `db-dumps/<label>/`, e.g. `xusom.sql`.
    pub file_name: String,
}

/// One generated file, relative to the project root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedFile {
    pub path: String,
    pub contents: String,
}

/// A value the generator changed to point at a container instead of the
/// sender's machine (`localhost:5432` -> `postgres:5432`), reported so the
/// wizard can show exactly what was rewritten rather than doing it silently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rewrite {
    /// The environment variable's key.
    pub key: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generated {
    /// The whole `docker-compose.yml`.
    pub compose_yaml: String,
    /// Everything else the compose file needs (the Dockerfile), relative to
    /// the project root.
    pub files: Vec<GeneratedFile>,
    pub rewrites: Vec<Rewrite>,
    /// Plain-language caveats worth showing (e.g. a MongoDB dump is not
    /// restored automatically).
    pub notes: Vec<String>,
    /// The host port the app is published on.
    pub host_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenerateError {
    /// The spec failed validation; nothing was generated.
    Invalid(Vec<FieldError>),
    /// A bug (the generated YAML didn't serialize, ...), not bad input.
    Internal(String),
}

impl std::fmt::Display for GenerateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenerateError::Invalid(errors) => {
                let joined: Vec<String> = errors.iter().map(|e| format!("{}: {}", e.field, e.message)).collect();
                write!(f, "invalid compose settings - {}", joined.join("; "))
            }
            GenerateError::Internal(m) => write!(f, "couldn't generate the compose file: {m}"),
        }
    }
}

impl std::error::Error for GenerateError {}
