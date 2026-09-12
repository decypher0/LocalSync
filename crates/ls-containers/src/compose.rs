//! Pure `docker-compose.yml` logic: no daemon, no filesystem, no process
//! spawning. Everything here is unit-testable on any machine.
//!
//! We don't parse compose into a typed struct (the spec is huge and we only
//! touch a handful of keys) — `serde_yaml::Value` plus targeted mutation is
//! the whole job.

use anyhow::{anyhow, Context, Result};
use ls_security::SandboxPolicy;
use ls_snapshot::Manifest;
use serde_yaml::Value;

/// Deterministic database data-volume name for a given seed hash. Same seed
/// == same volume == stock database images (MySQL, Postgres, ...) skip their
/// init-script directory on the second run because the data dir is already
/// populated.
pub fn db_volume_name(db_seed_hash: &str) -> String {
    format!("localsync-db-{db_seed_hash}")
}

/// Known database image keywords. Deliberately scoped to this repo's two
/// concrete stacks (MySQL/MariaDB, Postgres) rather than every database
/// engine that might exist someday — expand when a new stack actually needs
/// it, not before.
const DATABASE_IMAGE_KEYWORDS: &[&str] = &["mysql", "mariadb", "postgres", "postgresql"];

/// Compose service names the manifest says are a known database engine, by
/// inspecting `ServiceDef::image_or_build` (e.g. "image:mysql:8.0",
/// "image:postgres:16") against `DATABASE_IMAGE_KEYWORDS`.
pub fn database_service_names(manifest: &Manifest) -> Vec<String> {
    manifest
        .services
        .iter()
        .filter(|s| {
            let image = s.image_or_build.to_lowercase();
            DATABASE_IMAGE_KEYWORDS.iter().any(|kw| image.contains(kw))
        })
        .map(|s| s.name.clone())
        .collect()
}

/// Extract (service_name, "host:container") pairs from a compose file's
/// `ports:` lists. Handles the common string forms: "8080:8080" and
/// "127.0.0.1:8080:8080" (host IP dropped). A bare "8080" (container-only,
/// random host port) is passed through unchanged since there's no static
/// host port to report.
pub fn parse_service_ports(yaml: &str) -> Result<Vec<(String, String)>> {
    let doc: Value = serde_yaml::from_str(yaml).context("parsing docker-compose.yml as YAML")?;
    let mut out = Vec::new();

    let Some(services) = doc.get("services").and_then(Value::as_mapping) else {
        return Ok(out);
    };

    for (name, svc) in services {
        let Some(name) = name.as_str() else { continue };
        let Some(ports) = svc.get("ports").and_then(Value::as_sequence) else {
            continue;
        };
        for p in ports {
            if let Some(s) = normalize_port_entry(p) {
                out.push((name.to_string(), s));
            }
        }
    }

    Ok(out)
}

fn normalize_port_entry(v: &Value) -> Option<String> {
    let s = v.as_str().map(str::to_string).or_else(|| {
        // short numeric form, e.g. `ports: [8080]`
        v.as_u64().map(|n| n.to_string())
    })?;
    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        3 => Some(format!("{}:{}", parts[1], parts[2])), // drop host IP
        2 => Some(format!("{}:{}", parts[0], parts[1])),
        1 => Some(parts[0].to_string()),
        _ => Some(s),
    }
}

/// Rewrite `yaml` to enforce `policy` on every service, regardless of what
/// the (untrusted-but-signed) snapshot's own compose file says, and repoint
/// any database service's named data volume at `db_volume`.
///
/// Applied per service: `read_only`, a writable `tmpfs` for `/tmp`,
/// `mem_limit`, `cpus`, and `network_mode` is stripped so it falls back to
/// compose's default per-project bridge network (never trust a snapshot
/// that asks for `network_mode: host`).
pub fn apply_policy(
    yaml: &str,
    policy: &SandboxPolicy,
    db_services: &[String],
    db_volume: &str,
) -> Result<String> {
    let mut doc: Value = serde_yaml::from_str(yaml).context("parsing docker-compose.yml as YAML")?;
    let root = doc
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("docker-compose.yml root is not a mapping"))?;

    // Collect (service, named-volume-key) pairs for database services before
    // we start mutating, so we're not borrowing `services` and `root` at once.
    let mut volume_keys_to_pin = Vec::new();

    if let Some(Value::Mapping(services)) = root.get_mut("services") {
        for (name, svc) in services.iter_mut() {
            let Some(svc_map) = svc.as_mapping_mut() else {
                continue;
            };

            if let Some(Value::String(image)) = svc_map.get("image").cloned() {
                svc_map.insert(Value::from("image"), Value::from(canonicalize_image(&image)));
            }
            rewrite_build_context(svc_map);

            svc_map.insert(Value::from("read_only"), Value::from(policy.read_only_rootfs));
            svc_map.insert(
                Value::from("tmpfs"),
                Value::Sequence(vec![Value::from("/tmp")]),
            );
            svc_map.insert(Value::from("mem_limit"), Value::from(policy.memory_limit));
            svc_map.insert(Value::from("cpus"), Value::from(policy.cpu_limit));
            svc_map.remove("network_mode");

            let svc_name = name.as_str().unwrap_or_default();
            if db_services.iter().any(|n| n == svc_name) {
                if let Some(key) = named_volume_key(svc_map) {
                    volume_keys_to_pin.push(key);
                }
            }
        }
    }

    for key in volume_keys_to_pin {
        pin_volume_name(root, &key, db_volume);
    }

    serde_yaml::to_string(&doc).context("re-serializing docker-compose.yml")
}

/// Fully-qualifies a short image reference ("mysql:8.0" ->
/// "docker.io/library/mysql:8.0", "bitnami/mysql" -> "docker.io/bitnami/mysql").
/// Rootless Podman doesn't ship with unqualified-search-registries configured
/// by default, so a snapshot built on a machine that *does* have one set up
/// (or built with Docker, which always assumes Docker Hub) can otherwise ship
/// a compose file that fails to resolve on the receiver. Already-qualified
/// references (a registry host, or an explicit scheme) pass through untouched.
fn canonicalize_image(image: &str) -> String {
    if image.contains("://") {
        return image.to_string();
    }
    // No '/' at all means no registry/namespace component could be present -
    // any ':' here is the tag separator ("mysql:8.0"), not a host:port.
    let Some((first_segment, _)) = image.split_once('/') else {
        return format!("docker.io/library/{image}");
    };
    let looks_qualified =
        first_segment.contains('.') || first_segment.contains(':') || first_segment == "localhost";
    if looks_qualified {
        image.to_string()
    } else {
        format!("docker.io/{image}")
    }
}

/// The snapshot's `docker-compose.yml` ships at the payload root, but
/// `ls_snapshot::bundle` puts the git-tracked project tree it references
/// (build contexts) under `source/` there instead - `source/` is a `git
/// archive` of HEAD, kept separate from the top-level compose/db-seed copies
/// so those still ship even when the sender never committed them. A build
/// context of "./app" is only valid relative to where the tree actually
/// ended up post-unpack, i.e. "source/app". Rewrites in place; no-op for
/// services with no `build` key (image-only services need nothing here).
fn rewrite_build_context(svc_map: &mut serde_yaml::Mapping) {
    fn source_prefixed(s: &str) -> String {
        match s.strip_prefix("./").unwrap_or(s) {
            "." => "source".to_string(),
            trimmed => format!("source/{trimmed}"),
        }
    }

    if let Some(v) = svc_map.get_mut("build") {
        match v {
            Value::String(s) => *s = source_prefixed(s),
            Value::Mapping(m) => {
                if let Some(Value::String(ctx)) = m.get_mut("context") {
                    *ctx = source_prefixed(ctx);
                }
            }
            _ => {}
        }
    }
}

/// Find the top-level volume key a service mounts (its first entry of the
/// form "key:/container/path[:ro]" where `key` isn't a bind-mount path).
fn named_volume_key(svc_map: &serde_yaml::Mapping) -> Option<String> {
    let volumes = svc_map.get("volumes")?.as_sequence()?;
    for v in volumes {
        let Some(s) = v.as_str() else { continue };
        let Some(key) = s.split(':').next() else {
            continue;
        };
        if !key.starts_with('.') && !key.starts_with('/') {
            return Some(key.to_string());
        }
    }
    None
}

/// Set `volumes.<key>.name` at the compose root so the actual podman volume
/// created is `db_volume`, independent of the compose project name.
fn pin_volume_name(root: &mut serde_yaml::Mapping, key: &str, db_volume: &str) {
    let vol_section = Value::from("volumes");
    if !matches!(root.get(&vol_section), Some(Value::Mapping(_))) {
        root.insert(vol_section.clone(), Value::Mapping(serde_yaml::Mapping::new()));
    }
    let Some(Value::Mapping(volumes)) = root.get_mut(&vol_section) else {
        return;
    };

    let vol_key = Value::from(key);
    if !matches!(volumes.get(&vol_key), Some(Value::Mapping(_))) {
        volumes.insert(vol_key.clone(), Value::Mapping(serde_yaml::Mapping::new()));
    }
    if let Some(Value::Mapping(m)) = volumes.get_mut(&vol_key) {
        m.insert(Value::from("name"), Value::from(db_volume));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ls_snapshot::ServiceDef;

    fn manifest_with_services(services: Vec<ServiceDef>) -> Manifest {
        Manifest {
            project_name: "demo".into(),
            git_commit: "deadbeef".into(),
            git_parent_commit: None,
            dependency_lock_hash: "0".repeat(64),
            db_seed_hash: "abc123".into(),
            services,
            sender_pubkey: [0u8; 32],
            created_at: time::OffsetDateTime::now_utc(),
            folders: vec![],
            database_dumps: vec![],
        }
    }

    #[test]
    fn volume_name_is_deterministic_from_seed_hash() {
        assert_eq!(db_volume_name("abc123"), "localsync-db-abc123");
        assert_eq!(db_volume_name("abc123"), db_volume_name("abc123"));
    }

    #[test]
    fn finds_database_service_by_image_mysql_and_postgres_alike() {
        let manifest = manifest_with_services(vec![
            ServiceDef {
                name: "app".into(),
                image_or_build: "build:./app".into(),
                ports: vec!["8080:8080".into()],
                depends_on: vec!["db".into()],
            },
            ServiceDef {
                name: "nginx".into(),
                image_or_build: "image:nginx:alpine".into(),
                ports: vec!["80:80".into()],
                depends_on: vec![],
            },
            ServiceDef {
                name: "db".into(),
                image_or_build: "image:mysql:8.0".into(),
                ports: vec!["3306:3306".into()],
                depends_on: vec![],
            },
        ]);
        assert_eq!(database_service_names(&manifest), vec!["db".to_string()]);

        let manifest_pg = manifest_with_services(vec![
            ServiceDef {
                name: "app".into(),
                image_or_build: "build:./app".into(),
                ports: vec!["8080:8080".into()],
                depends_on: vec!["postgres".into()],
            },
            ServiceDef {
                name: "postgres".into(),
                image_or_build: "image:docker.io/library/postgres:16".into(),
                ports: vec!["5432:5432".into()],
                depends_on: vec![],
            },
        ]);
        assert_eq!(
            database_service_names(&manifest_pg),
            vec!["postgres".to_string()]
        );
    }

    #[test]
    fn parses_ports_including_host_ip_form() {
        let yaml = r#"
services:
  app:
    image: whatever
    ports:
      - "8080:8080"
  db:
    image: mysql:8.0
    ports:
      - "127.0.0.1:3306:3306"
"#;
        let mut ports = parse_service_ports(yaml).unwrap();
        ports.sort();
        assert_eq!(
            ports,
            vec![
                ("app".to_string(), "8080:8080".to_string()),
                ("db".to_string(), "3306:3306".to_string()),
            ]
        );
    }

    #[test]
    fn canonicalizes_short_image_names_but_leaves_qualified_ones_alone() {
        assert_eq!(canonicalize_image("mysql:8.0"), "docker.io/library/mysql:8.0");
        assert_eq!(canonicalize_image("nginx"), "docker.io/library/nginx");
        assert_eq!(canonicalize_image("bitnami/mysql:8.0"), "docker.io/bitnami/mysql:8.0");
        assert_eq!(canonicalize_image("docker.io/library/mysql:8.0"), "docker.io/library/mysql:8.0");
        assert_eq!(canonicalize_image("ghcr.io/foo/bar:latest"), "ghcr.io/foo/bar:latest");
        assert_eq!(canonicalize_image("localhost:5000/foo"), "localhost:5000/foo");
    }

    #[test]
    fn apply_policy_locks_down_every_service_and_pins_db_volume() {
        let yaml = r#"
services:
  app:
    build: ./app
    network_mode: host
    ports:
      - "8080:8080"
  db:
    image: mysql:8.0
    volumes:
      - db-data:/var/lib/mysql
volumes:
  db-data: {}
"#;
        let policy = ls_security::default_policy();
        let rewritten =
            apply_policy(yaml, &policy, &["db".to_string()], "localsync-db-abc123").unwrap();
        let doc: Value = serde_yaml::from_str(&rewritten).unwrap();

        for svc_name in ["app", "db"] {
            let svc = doc["services"][svc_name].as_mapping().unwrap();
            assert_eq!(svc.get("read_only").unwrap().as_bool(), Some(true));
            assert_eq!(
                svc.get("mem_limit").unwrap().as_str(),
                Some(policy.memory_limit)
            );
            assert_eq!(svc.get("cpus").unwrap().as_str(), Some(policy.cpu_limit));
            assert!(svc.get("network_mode").is_none());
            let tmpfs = svc.get("tmpfs").unwrap().as_sequence().unwrap();
            assert_eq!(tmpfs[0].as_str(), Some("/tmp"));
        }

        assert_eq!(
            doc["volumes"]["db-data"]["name"].as_str(),
            Some("localsync-db-abc123")
        );
        assert_eq!(
            doc["services"]["db"]["image"].as_str(),
            Some("docker.io/library/mysql:8.0")
        );
        assert_eq!(doc["services"]["app"]["build"].as_str(), Some("source/app"));
    }

    #[test]
    fn rewrites_build_context_mapping_form_too() {
        let yaml = r#"
services:
  app:
    build:
      context: .
      dockerfile: Dockerfile
"#;
        let policy = ls_security::default_policy();
        let rewritten = apply_policy(yaml, &policy, &[], "unused").unwrap();
        let doc: Value = serde_yaml::from_str(&rewritten).unwrap();
        assert_eq!(doc["services"]["app"]["build"]["context"].as_str(), Some("source"));
    }

    #[test]
    fn apply_policy_creates_volume_entry_when_source_has_none() {
        let yaml = r#"
services:
  db:
    image: mysql:8.0
    volumes:
      - db-data:/var/lib/mysql
"#;
        let policy = ls_security::default_policy();
        let rewritten =
            apply_policy(yaml, &policy, &["db".to_string()], "localsync-db-xyz").unwrap();
        let doc: Value = serde_yaml::from_str(&rewritten).unwrap();
        assert_eq!(
            doc["volumes"]["db-data"]["name"].as_str(),
            Some("localsync-db-xyz")
        );
    }
}
