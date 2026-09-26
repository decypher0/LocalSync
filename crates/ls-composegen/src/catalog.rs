//! The fixed set of things the wizard offers. Every dropdown in the UI is
//! generated from [`catalog`], and validation and generation look values up
//! here - so anything the UI offers is something the rest accepts, and adding
//! a runtime/version later is a one-place change.
//!
//! Deliberately small. Expanding the list is cheap; supporting an arbitrary
//! typed-in runtime is not - which is the whole reason these are fixed choices.

use serde::Serialize;

use crate::spec::{BuildTool, DbEngine, DbEnvPreset, ExtraKind, Runtime};

pub struct RuntimeInfo {
    pub runtime: Runtime,
    pub label: &'static str,
    /// Newest last.
    pub versions: &'static [&'static str],
    /// Which build tools this runtime supports - the only ones offered for it.
    pub build_tools: &'static [BuildTool],
}

pub const RUNTIMES: &[RuntimeInfo] = &[
    RuntimeInfo { runtime: Runtime::Java, label: "Java", versions: &["8", "11", "17", "21"], build_tools: &[BuildTool::Maven, BuildTool::Gradle] },
    RuntimeInfo { runtime: Runtime::Node, label: "Node.js", versions: &["18", "20", "22"], build_tools: &[BuildTool::Npm, BuildTool::Yarn, BuildTool::Pnpm] },
    RuntimeInfo { runtime: Runtime::Python, label: "Python", versions: &["3.10", "3.11", "3.12"], build_tools: &[BuildTool::Pip] },
    RuntimeInfo { runtime: Runtime::Go, label: "Go", versions: &["1.21", "1.22"], build_tools: &[BuildTool::GoBuild] },
];

pub struct DbInfo {
    pub engine: DbEngine,
    pub label: &'static str,
    pub image: &'static str,
    pub versions: &'static [&'static str],
    pub port: u16,
}

pub const DATABASES: &[DbInfo] = &[
    DbInfo { engine: DbEngine::Mysql, label: "MySQL", image: "docker.io/library/mysql", versions: &["8.0", "8.4"], port: 3306 },
    DbInfo { engine: DbEngine::Postgres, label: "PostgreSQL", image: "docker.io/library/postgres", versions: &["14", "15", "16"], port: 5432 },
    DbInfo { engine: DbEngine::Mongodb, label: "MongoDB", image: "docker.io/library/mongo", versions: &["6.0", "7.0"], port: 27017 },
];

pub struct ExtraInfo {
    pub kind: ExtraKind,
    pub label: &'static str,
    pub image: &'static str,
    pub versions: &'static [&'static str],
    pub port: u16,
}

pub const EXTRAS: &[ExtraInfo] = &[
    ExtraInfo { kind: ExtraKind::Redis, label: "Redis", image: "docker.io/library/redis", versions: &["6", "7"], port: 6379 },
    ExtraInfo { kind: ExtraKind::Rabbitmq, label: "RabbitMQ", image: "docker.io/library/rabbitmq", versions: &["3.12", "3.13"], port: 5672 },
    ExtraInfo { kind: ExtraKind::Memcached, label: "Memcached", image: "docker.io/library/memcached", versions: &["1.6"], port: 11211 },
];

pub fn runtime_info(runtime: Runtime) -> &'static RuntimeInfo {
    RUNTIMES.iter().find(|r| r.runtime == runtime).expect("every Runtime variant has a catalog entry")
}

pub fn db_info(engine: DbEngine) -> &'static DbInfo {
    DATABASES.iter().find(|d| d.engine == engine).expect("every DbEngine variant has a catalog entry")
}

pub fn extra_info(kind: ExtraKind) -> &'static ExtraInfo {
    EXTRAS.iter().find(|e| e.kind == kind).expect("every ExtraKind variant has a catalog entry")
}

/// The service name used in the generated compose file - and therefore the
/// hostname the app reaches it at.
pub fn db_service_name(engine: DbEngine) -> &'static str {
    match engine {
        DbEngine::Mysql => "mysql",
        DbEngine::Postgres => "postgres",
        DbEngine::Mongodb => "mongodb",
    }
}

pub fn extra_service_name(kind: ExtraKind) -> &'static str {
    match kind {
        ExtraKind::Redis => "redis",
        ExtraKind::Rabbitmq => "rabbitmq",
        ExtraKind::Memcached => "memcached",
    }
}

pub fn build_tool_label(tool: BuildTool) -> &'static str {
    match tool {
        BuildTool::Maven => "Maven",
        BuildTool::Gradle => "Gradle",
        BuildTool::Npm => "npm",
        BuildTool::Yarn => "Yarn",
        BuildTool::Pnpm => "pnpm",
        BuildTool::Pip => "pip (requirements.txt)",
        BuildTool::GoBuild => "Go modules (go build)",
    }
}

/// The run command the runtime/tool implies, where there is a sensible one.
/// `None` means the person has to say (Python has no convention).
///
/// Java's is unambiguous *because* the build artifact is copied to a fixed
/// place (`/app/app.jar`) in the image; Go's likewise (`/app/app`).
pub fn default_run_command(runtime: Runtime, tool: BuildTool) -> Option<&'static str> {
    match (runtime, tool) {
        (Runtime::Java, _) => Some("java -jar /app/app.jar"),
        (Runtime::Node, BuildTool::Yarn) => Some("yarn start"),
        (Runtime::Node, BuildTool::Pnpm) => Some("pnpm start"),
        (Runtime::Node, _) => Some("npm start"),
        (Runtime::Go, _) => Some("/app/app"),
        (Runtime::Python, _) => None,
    }
}

/// Where the tool leaves its artifact by default. Only meaningful for
/// runtimes that build one to run (Java).
pub fn default_artifact_path(tool: BuildTool) -> Option<&'static str> {
    match tool {
        BuildTool::Maven => Some("target/*.jar"),
        BuildTool::Gradle => Some("build/libs/*.jar"),
        _ => None,
    }
}

pub fn needs_artifact_path(runtime: Runtime) -> bool {
    matches!(runtime, Runtime::Java)
}

pub fn preset_label(preset: DbEnvPreset) -> &'static str {
    match preset {
        DbEnvPreset::Standard => "Standard variables (DB_HOST, DB_PORT, DB_NAME, DB_USER, DB_PASSWORD, DATABASE_URL)",
        DbEnvPreset::Spring => "Spring Boot (SPRING_DATASOURCE_URL, SPRING_DATASOURCE_USERNAME, SPRING_DATASOURCE_PASSWORD)",
        DbEnvPreset::None => "None - I'll set them in the environment variables below",
    }
}

// ---------- what the UI receives ----------

#[derive(Debug, Clone, Serialize)]
pub struct Catalog {
    pub runtimes: Vec<RuntimeEntry>,
    pub databases: Vec<ServiceEntry<DbEngine>>,
    pub extras: Vec<ServiceEntry<ExtraKind>>,
    pub db_env_presets: Vec<PresetEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeEntry {
    pub runtime: Runtime,
    pub label: String,
    pub versions: Vec<String>,
    pub build_tools: Vec<BuildToolEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuildToolEntry {
    pub tool: BuildTool,
    pub label: String,
    /// Pre-fill for the run-command field; `None` = no sensible default.
    pub default_run_command: Option<String>,
    /// Whether the artifact path is asked for (and required) with this tool.
    pub needs_artifact_path: bool,
    pub default_artifact_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceEntry<K> {
    pub kind: K,
    pub label: String,
    pub versions: Vec<String>,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct PresetEntry {
    pub preset: DbEnvPreset,
    pub label: String,
}

/// The whole catalog as the UI consumes it (one JSON value; every dropdown,
/// default and pre-fill in the wizard comes from here).
pub fn catalog() -> Catalog {
    Catalog {
        runtimes: RUNTIMES
            .iter()
            .map(|r| RuntimeEntry {
                runtime: r.runtime,
                label: r.label.to_string(),
                versions: r.versions.iter().map(|v| v.to_string()).collect(),
                build_tools: r
                    .build_tools
                    .iter()
                    .map(|&tool| BuildToolEntry {
                        tool,
                        label: build_tool_label(tool).to_string(),
                        default_run_command: default_run_command(r.runtime, tool).map(str::to_string),
                        needs_artifact_path: needs_artifact_path(r.runtime),
                        default_artifact_path: default_artifact_path(tool).map(str::to_string),
                    })
                    .collect(),
            })
            .collect(),
        databases: DATABASES
            .iter()
            .map(|d| ServiceEntry { kind: d.engine, label: d.label.to_string(), versions: d.versions.iter().map(|v| v.to_string()).collect(), port: d.port })
            .collect(),
        extras: EXTRAS
            .iter()
            .map(|e| ServiceEntry { kind: e.kind, label: e.label.to_string(), versions: e.versions.iter().map(|v| v.to_string()).collect(), port: e.port })
            .collect(),
        db_env_presets: [DbEnvPreset::Standard, DbEnvPreset::Spring, DbEnvPreset::None]
            .into_iter()
            .map(|preset| PresetEntry { preset, label: preset_label(preset).to_string() })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_runtime_offers_versions_and_at_least_one_build_tool() {
        for r in RUNTIMES {
            assert!(!r.versions.is_empty(), "{}", r.label);
            assert!(!r.build_tools.is_empty(), "{}", r.label);
        }
    }

    #[test]
    fn no_duplicate_entries_anywhere() {
        let mut runtimes = std::collections::HashSet::new();
        for r in RUNTIMES {
            assert!(runtimes.insert(r.runtime), "duplicate runtime {:?}", r.runtime);
        }
        let mut engines = std::collections::HashSet::new();
        for d in DATABASES {
            assert!(engines.insert(d.engine), "duplicate database {:?}", d.engine);
        }
        let mut extras = std::collections::HashSet::new();
        for e in EXTRAS {
            assert!(extras.insert(e.kind), "duplicate extra {:?}", e.kind);
        }
        let versions_unique = |vs: &[&str]| vs.iter().collect::<std::collections::HashSet<_>>().len() == vs.len();
        assert!(RUNTIMES.iter().all(|r| versions_unique(r.versions)));
        assert!(DATABASES.iter().all(|d| versions_unique(d.versions)));
        assert!(EXTRAS.iter().all(|e| versions_unique(e.versions)));
    }

    #[test]
    fn every_enum_variant_has_a_catalog_entry() {
        // These panic (via `expect`) if a variant is ever added without one.
        for rt in [Runtime::Java, Runtime::Node, Runtime::Python, Runtime::Go] {
            runtime_info(rt);
        }
        for e in [DbEngine::Mysql, DbEngine::Postgres, DbEngine::Mongodb] {
            db_info(e);
            db_service_name(e);
        }
        for k in [ExtraKind::Redis, ExtraKind::Rabbitmq, ExtraKind::Memcached] {
            extra_info(k);
            extra_service_name(k);
        }
    }

    #[test]
    fn java_asks_for_an_artifact_path_and_has_defaults_for_it() {
        assert!(needs_artifact_path(Runtime::Java));
        assert_eq!(default_artifact_path(BuildTool::Maven), Some("target/*.jar"));
        assert_eq!(default_artifact_path(BuildTool::Gradle), Some("build/libs/*.jar"));
        assert!(!needs_artifact_path(Runtime::Node));
    }

    #[test]
    fn python_has_no_default_run_command_but_the_others_do() {
        assert_eq!(default_run_command(Runtime::Python, BuildTool::Pip), None);
        assert_eq!(default_run_command(Runtime::Node, BuildTool::Yarn), Some("yarn start"));
        assert_eq!(default_run_command(Runtime::Java, BuildTool::Maven), Some("java -jar /app/app.jar"));
    }

    #[test]
    fn the_ui_catalog_serializes_with_lowercase_enum_values() {
        let json = serde_json::to_value(catalog()).unwrap();
        assert_eq!(json["runtimes"][0]["runtime"], "java");
        assert_eq!(json["runtimes"][0]["build_tools"][0]["tool"], "maven");
        assert_eq!(json["runtimes"][3]["build_tools"][0]["tool"], "go", "GoBuild serializes as \"go\"");
        assert_eq!(json["databases"][1]["kind"], "postgres");
        assert_eq!(json["extras"][1]["kind"], "rabbitmq");
        assert_eq!(json["db_env_presets"][1]["preset"], "spring");
    }
}
