//! Generation: a validated [`ComposeSpec`] -> `docker-compose.yml` + the
//! Dockerfile it builds from.
//!
//! The receiver forces every service read-only with only `/tmp` writable, so
//! everything generated here keeps caches/home under `/tmp` and puts database
//! data in the named `db-data` volume. The YAML is built as a
//! `serde_yaml::Value` (insertion-ordered) so quoting is the serializer's job
//! and the same spec always produces byte-identical output.

use serde_yaml::{Mapping, Value};

use crate::catalog::{db_info, db_service_name, extra_info, extra_service_name};
use crate::spec::*;

/// Fixed root password for MySQL (throwaway, like [`DB_PASSWORD`]).
const MYSQL_ROOT_PASSWORD: &str = "localsync_root_pw";
const DB_VOLUME: &str = "db-data";

/// Validates `spec` first (returning [`GenerateError::Invalid`] if it fails -
/// nothing is generated from bad input), then generates.
pub fn generate(spec: &ComposeSpec, ctx: &GenerateContext) -> Result<Generated, GenerateError> {
    crate::validate::validate(spec).map_err(GenerateError::Invalid)?;
    generate_unchecked(spec, ctx)
}

fn s(v: &str) -> Value {
    Value::from(v)
}

fn map(pairs: Vec<(&str, Value)>) -> Value {
    Value::Mapping(pairs.into_iter().map(|(k, v)| (s(k), v)).collect())
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|i| i.to_string()).collect()
}

fn healthcheck(test: Vec<String>, retries: u32) -> Value {
    map(vec![
        ("test", Value::Sequence(test.into_iter().map(Value::from).collect())),
        ("interval", s("5s")),
        ("timeout", s("5s")),
        ("retries", Value::from(retries)),
    ])
}

/// Health check for an extra, where the image has a cheap one.
fn extra_health(kind: ExtraKind) -> Option<Vec<String>> {
    match kind {
        ExtraKind::Redis => Some(strings(&["CMD", "redis-cli", "ping"])),
        ExtraKind::Rabbitmq => Some(strings(&["CMD", "rabbitmq-diagnostics", "-q", "ping"])),
        ExtraKind::Memcached => None,
    }
}

/// Read-only-rootfs-friendly environment: home and caches live under `/tmp`.
fn runtime_env(spec: &ComposeSpec) -> Vec<(&'static str, &'static str)> {
    let mut env = vec![("HOME", "/tmp")];
    match spec.runtime {
        Runtime::Java => env.push(("JAVA_TOOL_OPTIONS", "-Djava.io.tmpdir=/tmp")),
        Runtime::Node => {
            env.push(("npm_config_cache", "/tmp/.npm"));
            env.push(("YARN_CACHE_FOLDER", "/tmp/.yarn"));
        }
        Runtime::Python => {
            env.push(("PYTHONDONTWRITEBYTECODE", "1"));
            env.push(("PYTHONUNBUFFERED", "1"));
        }
        Runtime::Go => {}
    }
    env
}

fn db_url(db: &DatabaseSpec) -> String {
    let host = db_service_name(db.engine);
    let port = db_info(db.engine).port;
    match db.engine {
        DbEngine::Mysql => format!("mysql://{DB_USER}:{DB_PASSWORD}@{host}:{port}/{}", db.database),
        DbEngine::Postgres => format!("postgres://{DB_USER}:{DB_PASSWORD}@{host}:{port}/{}", db.database),
        DbEngine::Mongodb => format!("mongodb://{DB_USER}:{DB_PASSWORD}@{host}:{port}/{}?authSource=admin", db.database),
    }
}

fn preset_env(db: &DatabaseSpec, preset: DbEnvPreset) -> Vec<(&'static str, String)> {
    let host = db_service_name(db.engine);
    let port = db_info(db.engine).port;
    match preset {
        DbEnvPreset::None => vec![],
        DbEnvPreset::Standard => vec![
            ("DB_HOST", host.to_string()),
            ("DB_PORT", port.to_string()),
            ("DB_NAME", db.database.clone()),
            ("DB_USER", DB_USER.to_string()),
            ("DB_PASSWORD", DB_PASSWORD.to_string()),
            ("DATABASE_URL", db_url(db)),
        ],
        DbEnvPreset::Spring => match db.engine {
            DbEngine::Mongodb => vec![("SPRING_DATA_MONGODB_URI", db_url(db))],
            engine => {
                let scheme = if engine == DbEngine::Mysql { "mysql" } else { "postgresql" };
                vec![
                    ("SPRING_DATASOURCE_URL", format!("jdbc:{scheme}://{host}:{port}/{}", db.database)),
                    ("SPRING_DATASOURCE_USERNAME", DB_USER.to_string()),
                    ("SPRING_DATASOURCE_PASSWORD", DB_PASSWORD.to_string()),
                ]
            }
        },
    }
}

/// Rewrites `localhost:<P>` / `127.0.0.1:<P>` to `<service>:<P>` when P is the
/// default port of a service in this compose. Port-anchored on purpose: a bare
/// `localhost`, or a port that isn't one of `services`, is never touched.
fn rewrite_localhost(value: &str, services: &[(&str, u16)]) -> String {
    const NEEDLES: [&str; 2] = ["localhost:", "127.0.0.1:"];
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < value.len() {
        let rest = &value[i..];
        let prev_ok = value[..i].chars().next_back().map_or(true, |c| !(c.is_alphanumeric() || matches!(c, '.' | '-' | '_')));
        if prev_ok {
            if let Some(needle) = NEEDLES.iter().find(|n| rest.starts_with(**n)) {
                let digits: String = rest[needle.len()..].chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Some((name, _)) = digits.parse::<u16>().ok().and_then(|p| services.iter().find(|(_, sp)| *sp == p)) {
                    out.push_str(name);
                    out.push(':');
                    out.push_str(&digits);
                    i += needle.len() + digits.len();
                    continue;
                }
            }
        }
        let c = rest.chars().next().expect("i < len");
        out.push(c);
        i += c.len_utf8();
    }
    out
}

fn dockerfile(spec: &ComposeSpec) -> String {
    let v = &spec.runtime_version;
    match spec.runtime {
        Runtime::Java => {
            let (image, build) = match spec.build_tool {
                BuildTool::Gradle => (format!("docker.io/library/gradle:8-jdk{v}"), "gradle build -x test --no-daemon"),
                _ => (format!("docker.io/library/maven:3.9-eclipse-temurin-{v}"), "mvn -B -q -DskipTests package"),
            };
            let artifact = spec.artifact_path.as_deref().unwrap_or("target/*.jar");
            format!(
                "FROM {image} AS build\nWORKDIR /src\nCOPY . .\nRUN {build}\n\n\
                 FROM docker.io/library/eclipse-temurin:{v}-jre\nWORKDIR /app\nCOPY --from=build /src/{artifact} /app/app.jar\n"
            )
        }
        Runtime::Node => {
            let (env, install) = match spec.build_tool {
                BuildTool::Yarn => ("", "yarn install --frozen-lockfile"),
                // Corepack's downloaded pnpm must live in the image (the runtime home is /tmp).
                BuildTool::Pnpm => ("ENV COREPACK_HOME=/usr/local/share/corepack\n", "corepack enable && pnpm install --frozen-lockfile"),
                _ => ("", "if [ -f package-lock.json ]; then npm ci; else npm install; fi"),
            };
            format!("FROM docker.io/library/node:{v}-slim\n{env}WORKDIR /app\nCOPY . .\nRUN {install}\n")
        }
        Runtime::Python => format!(
            "FROM docker.io/library/python:{v}-slim\nWORKDIR /app\nCOPY . .\n\
             RUN if [ -f requirements.txt ]; then pip install --no-cache-dir -r requirements.txt; fi\n"
        ),
        Runtime::Go => format!(
            "FROM docker.io/library/golang:{v} AS build\nWORKDIR /src\nCOPY . .\nRUN CGO_ENABLED=0 go build -o /out/app .\n\n\
             FROM docker.io/library/debian:bookworm-slim\nWORKDIR /app\nCOPY --from=build /out/app /app/app\n"
        ),
    }
}

fn db_service(db: &DatabaseSpec, ctx: &GenerateContext, notes: &mut Vec<String>) -> Value {
    let info = db_info(db.engine);
    let (env, data_dir, ping): (Vec<(&str, String)>, &str, Vec<String>) = match db.engine {
        DbEngine::Mysql => (
            vec![
                ("MYSQL_DATABASE", db.database.clone()),
                ("MYSQL_USER", DB_USER.into()),
                ("MYSQL_PASSWORD", DB_PASSWORD.into()),
                ("MYSQL_ROOT_PASSWORD", MYSQL_ROOT_PASSWORD.into()),
            ],
            "/var/lib/mysql",
            // TCP (127.0.0.1), not the socket: the entrypoint's temporary init
            // server is socket-only, so this only passes once the dump is imported.
            strings(&["CMD", "mysqladmin", "ping", "-h", "127.0.0.1", "-u", "root", &format!("-p{MYSQL_ROOT_PASSWORD}")]),
        ),
        DbEngine::Postgres => (
            vec![("POSTGRES_DB", db.database.clone()), ("POSTGRES_USER", DB_USER.into()), ("POSTGRES_PASSWORD", DB_PASSWORD.into())],
            "/var/lib/postgresql/data",
            // Same reasoning: init runs with listen_addresses='', so TCP only answers after it.
            strings(&["CMD", "pg_isready", "-h", "127.0.0.1", "-U", DB_USER, "-d", &db.database]),
        ),
        DbEngine::Mongodb => (
            vec![
                ("MONGO_INITDB_ROOT_USERNAME", DB_USER.into()),
                ("MONGO_INITDB_ROOT_PASSWORD", DB_PASSWORD.into()),
                ("MONGO_INITDB_DATABASE", db.database.clone()),
            ],
            "/data/db",
            strings(&["CMD", "mongosh", "--quiet", "--eval", "db.adminCommand('ping').ok"]),
        ),
    };

    let mut volumes = vec![format!("{DB_VOLUME}:{data_dir}")];
    let mut has_dump = false;
    if let Some(dump) = &ctx.dump {
        match (db.engine, dump.engine) {
            (DbEngine::Mongodb, _) => notes.push(
                "The MongoDB dump is sent along but is not restored automatically - the MongoDB image can't import it on first start, so the receiver's MongoDB starts empty.".into(),
            ),
            (a, b) if a != b => notes.push(format!(
                "The database dump is for {} but this project's database is {}, so it is not loaded.",
                db_info(b).label, info.label
            )),
            _ => {
                volumes.push(format!("../db-dumps/{}:/docker-entrypoint-initdb.d:ro", ctx.folder_label));
                has_dump = true;
            }
        }
    }

    map(vec![
        ("image", Value::from(format!("{}:{}", info.image, db.version))),
        ("environment", Value::Mapping(env.into_iter().map(|(k, v)| (s(k), Value::from(v))).collect())),
        ("volumes", Value::Sequence(volumes.into_iter().map(Value::from).collect())),
        // A big dump takes a while to import; don't give up on the health check early.
        ("healthcheck", healthcheck(ping, if has_dump { 120 } else { 20 })),
    ])
}

/// The real generator. [`generate`] = validate + this.
pub(crate) fn generate_unchecked(spec: &ComposeSpec, ctx: &GenerateContext) -> Result<Generated, GenerateError> {
    let host_port = ctx.host_port.unwrap_or(spec.port);
    let mut notes = Vec::new();
    let mut rewrites = Vec::new();

    // Every service in this compose that the app could name in a connection string.
    let mut known: Vec<(&str, u16)> = Vec::new();
    if let Some(db) = &spec.database {
        known.push((db_service_name(db.engine), db_info(db.engine).port));
    }
    for e in &spec.extras {
        known.push((extra_service_name(e.kind), extra_info(e.kind).port));
    }

    // environment: runtime basics < database preset < user rows.
    let mut env = Mapping::new();
    for (k, v) in runtime_env(spec) {
        env.insert(s(k), s(v));
    }
    if let Some(db) = &spec.database {
        for (k, v) in preset_env(db, spec.db_env_preset) {
            env.insert(s(k), Value::from(v));
        }
    }
    for row in &spec.env {
        let value = rewrite_localhost(&row.value, &known);
        if value != row.value {
            rewrites.push(Rewrite { key: row.key.clone(), from: row.value.clone(), to: value.clone() });
        }
        env.insert(s(&row.key), Value::from(value));
    }

    let mut depends = Mapping::new();
    let mut services = Mapping::new();
    let mut volumes = Mapping::new();
    if let Some(db) = &spec.database {
        let name = db_service_name(db.engine);
        depends.insert(s(name), map(vec![("condition", s("service_healthy"))]));
        services.insert(s(name), db_service(db, ctx, &mut notes));
        volumes.insert(s(DB_VOLUME), Value::Mapping(Mapping::new()));
    } else if ctx.dump.is_some() {
        notes.push("A database dump is included but no database is configured, so it is not loaded.".into());
    }
    for e in &spec.extras {
        let (info, name, health) = (extra_info(e.kind), extra_service_name(e.kind), extra_health(e.kind));
        let tag = if e.kind == ExtraKind::Rabbitmq { format!("{}-alpine", e.version) } else { e.version.clone() };
        let condition = if health.is_some() { "service_healthy" } else { "service_started" };
        depends.insert(s(name), map(vec![("condition", s(condition))]));
        let mut svc = vec![("image", Value::from(format!("{}:{tag}", info.image)))];
        if let Some(test) = health {
            svc.push(("healthcheck", healthcheck(test, 10)));
        }
        services.insert(s(name), map(svc));
    }

    let mut app = vec![
        ("build", map(vec![("context", s(".")), ("dockerfile", s(DOCKERFILE_NAME))])),
        ("command", Value::Sequence(vec![s("sh"), s("-c"), s(&spec.run_command)])),
        ("ports", Value::Sequence(vec![Value::from(format!("{host_port}:{}", spec.port))])),
        ("environment", Value::Mapping(env)),
    ];
    if !depends.is_empty() {
        app.push(("depends_on", Value::Mapping(depends)));
    }
    app.push(("restart", s("on-failure")));

    // App first, then the containers it depends on.
    let mut all = Mapping::new();
    all.insert(s(APP_SERVICE), map(app));
    all.extend(services);
    let mut root = vec![("services", Value::Mapping(all))];
    if !volumes.is_empty() {
        root.push(("volumes", Value::Mapping(volumes)));
    }
    let compose_yaml = serde_yaml::to_string(&map(root)).map_err(|e| GenerateError::Internal(e.to_string()))?;

    if spec.runtime == Runtime::Node {
        notes.push("Node projects are installed but not built. If yours needs a build step (e.g. TypeScript), put it in the run command, like: sh -c \"npm run build && npm start\".".into());
    }
    if spec.runtime == Runtime::Java && spec.build_tool == BuildTool::Gradle && spec.artifact_path.as_deref().is_some_and(|p| p.contains('*')) {
        notes.push("Gradle often builds a second \"-plain.jar\" next to the runnable jar. If the build fails while copying the jar, set the artifact path to the exact jar file name.".into());
    }

    Ok(Generated {
        compose_yaml,
        files: vec![GeneratedFile { path: DOCKERFILE_NAME.into(), contents: dockerfile(spec) }],
        rewrites,
        notes,
        host_port,
    })
}

// `ls_containers::compose` is a private module, so the round-trip test compiles
// its source file directly (the same code the receiver runs).
#[cfg(test)]
#[allow(dead_code)]
#[path = "../../ls-containers/src/compose.rs"]
mod receiver_compose;

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(runtime: Runtime, version: &str, tool: BuildTool, run: &str, port: u16) -> ComposeSpec {
        ComposeSpec {
            runtime,
            runtime_version: version.into(),
            build_tool: tool,
            run_command: run.into(),
            port,
            artifact_path: None,
            database: None,
            db_env_preset: DbEnvPreset::Standard,
            extras: vec![],
            env: vec![],
        }
    }

    fn java_maven() -> ComposeSpec {
        let mut sp = spec(Runtime::Java, "21", BuildTool::Maven, "java -jar /app/app.jar", 8080);
        sp.artifact_path = Some("target/*.jar".into());
        sp
    }

    fn python() -> ComposeSpec {
        spec(Runtime::Python, "3.12", BuildTool::Pip, "python app.py", 5000)
    }

    fn db(engine: DbEngine, version: &str, name: &str) -> Option<DatabaseSpec> {
        Some(DatabaseSpec { engine, version: version.into(), database: name.into() })
    }

    fn ctx() -> GenerateContext {
        GenerateContext { folder_label: "shop".into(), dump: None, host_port: None }
    }

    fn ctx_dump(engine: DbEngine) -> GenerateContext {
        GenerateContext { dump: Some(DumpInfo { engine, file_name: "shop.sql".into() }), ..ctx() }
    }

    fn gen(sp: &ComposeSpec, c: &GenerateContext) -> Generated {
        generate_unchecked(sp, c).unwrap()
    }

    fn yaml(g: &Generated) -> Value {
        serde_yaml::from_str(&g.compose_yaml).expect("generated compose parses as YAML")
    }

    fn dockerfile_of(g: &Generated) -> &str {
        assert_eq!(g.files.len(), 1);
        assert_eq!(g.files[0].path, DOCKERFILE_NAME);
        &g.files[0].contents
    }

    fn svc<'a>(y: &'a Value, name: &str) -> &'a Value {
        y.get("services").and_then(|s| s.get(name)).unwrap_or_else(|| panic!("no service {name}"))
    }

    fn env_of(y: &Value, name: &str) -> Vec<(String, String)> {
        svc(y, name)["environment"]
            .as_mapping()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.as_str().unwrap().into(), v.as_str().expect("env values are strings").into()))
            .collect()
    }

    fn env_get(y: &Value, key: &str) -> Option<String> {
        env_of(y, APP_SERVICE).into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    fn strs(v: &Value) -> Vec<&str> {
        v.as_sequence().unwrap().iter().map(|x| x.as_str().unwrap()).collect()
    }

    // ---------- per runtime ----------

    #[test]
    fn java_maven_builds_with_maven_and_runs_the_copied_jar() {
        let g = gen(&java_maven(), &ctx());
        let d = dockerfile_of(&g);
        assert!(d.contains("FROM docker.io/library/maven:3.9-eclipse-temurin-21 AS build"), "{d}");
        assert!(d.contains("RUN mvn -B -q -DskipTests package"));
        assert!(d.contains("FROM docker.io/library/eclipse-temurin:21-jre"));
        assert!(d.contains("COPY --from=build /src/target/*.jar /app/app.jar"));
        let y = yaml(&g);
        let app = svc(&y, APP_SERVICE);
        assert_eq!(app["build"]["context"], ".");
        assert_eq!(app["build"]["dockerfile"], DOCKERFILE_NAME);
        assert_eq!(strs(&app["command"]), ["sh", "-c", "java -jar /app/app.jar"]);
        assert_eq!(strs(&app["ports"]), ["8080:8080"]);
        assert_eq!(app["restart"], "on-failure");
        assert_eq!(g.host_port, 8080);
    }

    #[test]
    fn java_gradle_uses_the_gradle_image_command_and_its_own_artifact_path() {
        let mut sp = java_maven();
        sp.runtime_version = "17".into();
        sp.build_tool = BuildTool::Gradle;
        sp.artifact_path = Some("build/libs/*.jar".into());
        let g = gen(&sp, &ctx());
        let d = dockerfile_of(&g);
        assert!(d.contains("FROM docker.io/library/gradle:8-jdk17 AS build"), "{d}");
        assert!(d.contains("RUN gradle build -x test --no-daemon"));
        assert!(d.contains("FROM docker.io/library/eclipse-temurin:17-jre"));
        assert!(d.contains("COPY --from=build /src/build/libs/*.jar /app/app.jar"));
        assert!(!d.contains("mvn"));
        assert!(g.notes.iter().any(|n| n.contains("-plain.jar")), "{:?}", g.notes);
    }

    #[test]
    fn node_npm_yarn_pnpm_install_differently_and_carry_the_build_step_note() {
        let npm = gen(&spec(Runtime::Node, "20", BuildTool::Npm, "npm start", 3000), &ctx());
        let d = dockerfile_of(&npm);
        assert!(d.contains("FROM docker.io/library/node:20-slim"));
        assert!(d.contains("RUN if [ -f package-lock.json ]; then npm ci; else npm install; fi"));
        assert!(d.contains("COPY . ."));
        assert!(npm.notes.iter().any(|n| n.contains("build step") && n.contains("npm run build")), "{:?}", npm.notes);
        assert_eq!(strs(&svc(&yaml(&npm), APP_SERVICE)["command"]), ["sh", "-c", "npm start"]);

        let yarn = gen(&spec(Runtime::Node, "22", BuildTool::Yarn, "yarn start", 3000), &ctx());
        let d = dockerfile_of(&yarn);
        assert!(d.contains("FROM docker.io/library/node:22-slim"));
        assert!(d.contains("RUN yarn install --frozen-lockfile"));
        assert!(!d.contains("npm ci"));

        let pnpm = gen(&spec(Runtime::Node, "18", BuildTool::Pnpm, "pnpm start", 3000), &ctx());
        let d = dockerfile_of(&pnpm);
        assert!(d.contains("RUN corepack enable && pnpm install --frozen-lockfile"));
        assert!(d.contains("ENV COREPACK_HOME="), "corepack's cache must live in the image, not the read-only home");
    }

    #[test]
    fn python_installs_requirements_only_if_present() {
        let g = gen(&python(), &ctx());
        let d = dockerfile_of(&g);
        assert!(d.contains("FROM docker.io/library/python:3.12-slim"));
        assert!(d.contains("RUN if [ -f requirements.txt ]; then pip install --no-cache-dir -r requirements.txt; fi"));
        assert_eq!(strs(&svc(&yaml(&g), APP_SERVICE)["ports"]), ["5000:5000"]);
    }

    #[test]
    fn go_builds_a_static_binary_into_a_slim_final_image() {
        let g = gen(&spec(Runtime::Go, "1.22", BuildTool::GoBuild, "/app/app", 8081), &ctx());
        let d = dockerfile_of(&g);
        assert!(d.contains("FROM docker.io/library/golang:1.22 AS build"));
        assert!(d.contains("RUN CGO_ENABLED=0 go build -o /out/app ."));
        assert!(d.contains("FROM docker.io/library/debian:bookworm-slim"));
        assert!(d.contains("COPY --from=build /out/app /app/app"));
        assert_eq!(strs(&svc(&yaml(&g), APP_SERVICE)["command"]), ["sh", "-c", "/app/app"]);
    }

    #[test]
    fn every_runtime_gets_read_only_rootfs_friendly_env() {
        let cases: Vec<(ComposeSpec, Vec<(&str, &str)>)> = vec![
            (java_maven(), vec![("HOME", "/tmp"), ("JAVA_TOOL_OPTIONS", "-Djava.io.tmpdir=/tmp")]),
            (
                spec(Runtime::Node, "20", BuildTool::Npm, "npm start", 3000),
                vec![("HOME", "/tmp"), ("npm_config_cache", "/tmp/.npm"), ("YARN_CACHE_FOLDER", "/tmp/.yarn")],
            ),
            (python(), vec![("HOME", "/tmp"), ("PYTHONDONTWRITEBYTECODE", "1"), ("PYTHONUNBUFFERED", "1")]),
            (spec(Runtime::Go, "1.21", BuildTool::GoBuild, "/app/app", 8080), vec![("HOME", "/tmp")]),
        ];
        for (sp, expected) in cases {
            let y = yaml(&gen(&sp, &ctx()));
            for (k, v) in expected {
                assert_eq!(env_get(&y, k).as_deref(), Some(v), "{:?} {k}", sp.runtime);
            }
        }
    }

    #[test]
    fn run_command_with_pipes_and_quotes_stays_one_shell_argument() {
        let mut sp = python();
        sp.run_command = "sh -c \"npm run build && npm start\" | tee 'out: 1' # x".into();
        let y = yaml(&gen(&sp, &ctx()));
        assert_eq!(strs(&svc(&y, APP_SERVICE)["command"]), ["sh", "-c", sp.run_command.as_str()]);
    }

    // ---------- databases ----------

    #[test]
    fn mysql_with_dump_mounts_it_lists_the_data_volume_first_and_waits_generously() {
        let mut sp = java_maven();
        sp.database = db(DbEngine::Mysql, "8.4", "shop_db");
        let g = gen(&sp, &ctx_dump(DbEngine::Mysql));
        let y = yaml(&g);
        let mysql = svc(&y, "mysql");
        assert_eq!(mysql["image"], "docker.io/library/mysql:8.4");
        assert_eq!(strs(&mysql["volumes"]), ["db-data:/var/lib/mysql", "../db-dumps/shop:/docker-entrypoint-initdb.d:ro"]);
        let e = env_of(&y, "mysql");
        for (k, v) in [
            ("MYSQL_DATABASE", "shop_db"),
            ("MYSQL_USER", DB_USER),
            ("MYSQL_PASSWORD", DB_PASSWORD),
            ("MYSQL_ROOT_PASSWORD", MYSQL_ROOT_PASSWORD),
        ] {
            assert!(e.contains(&(k.into(), v.into())), "{k} in {e:?}");
        }
        let hc = &mysql["healthcheck"];
        assert_eq!(strs(&hc["test"])[..3], ["CMD", "mysqladmin", "ping"]);
        assert!(hc["retries"].as_u64().unwrap() >= 60, "a big dump takes a while");
        assert!(y["volumes"].as_mapping().unwrap().contains_key("db-data"));
        assert_eq!(svc(&y, APP_SERVICE)["depends_on"]["mysql"]["condition"], "service_healthy");
        assert!(g.notes.is_empty(), "{:?}", g.notes);
    }

    #[test]
    fn mysql_without_dump_has_no_init_mount() {
        let mut sp = java_maven();
        sp.database = db(DbEngine::Mysql, "8.0", "shop_db");
        let y = yaml(&gen(&sp, &ctx()));
        assert_eq!(strs(&svc(&y, "mysql")["volumes"]), ["db-data:/var/lib/mysql"]);
        assert!(svc(&y, "mysql")["healthcheck"]["retries"].as_u64().unwrap() < 60);
    }

    #[test]
    fn postgres_gets_its_image_env_data_dir_and_dump_mount() {
        let mut sp = python();
        sp.database = db(DbEngine::Postgres, "16", "shop_db");
        let y = yaml(&gen(&sp, &ctx_dump(DbEngine::Postgres)));
        let pg = svc(&y, "postgres");
        assert_eq!(pg["image"], "docker.io/library/postgres:16");
        assert_eq!(strs(&pg["volumes"]), ["db-data:/var/lib/postgresql/data", "../db-dumps/shop:/docker-entrypoint-initdb.d:ro"]);
        let e = env_of(&y, "postgres");
        assert!(e.contains(&("POSTGRES_DB".into(), "shop_db".into())));
        assert!(e.contains(&("POSTGRES_USER".into(), DB_USER.into())));
        assert!(e.contains(&("POSTGRES_PASSWORD".into(), DB_PASSWORD.into())));
        assert_eq!(strs(&pg["healthcheck"]["test"])[..2], ["CMD", "pg_isready"]);
    }

    #[test]
    fn mongodb_dump_is_not_mounted_and_a_note_says_so() {
        let mut sp = python();
        sp.database = db(DbEngine::Mongodb, "7.0", "shop_db");
        let g = gen(&sp, &ctx_dump(DbEngine::Mongodb));
        let y = yaml(&g);
        let mongo = svc(&y, "mongodb");
        assert_eq!(mongo["image"], "docker.io/library/mongo:7.0");
        assert_eq!(strs(&mongo["volumes"]), ["db-data:/data/db"], "no initdb mount for mongo");
        assert!(!g.compose_yaml.contains("db-dumps"));
        assert!(g.notes.iter().any(|n| n.contains("MongoDB") && n.contains("not restored automatically")), "{:?}", g.notes);
        assert!(env_of(&y, "mongodb").contains(&("MONGO_INITDB_ROOT_USERNAME".into(), DB_USER.into())));
    }

    #[test]
    fn a_dump_for_a_different_engine_is_not_mounted() {
        let mut sp = python();
        sp.database = db(DbEngine::Postgres, "16", "shop_db");
        let g = gen(&sp, &ctx_dump(DbEngine::Mysql));
        assert!(!g.compose_yaml.contains("db-dumps"));
        assert!(g.notes.iter().any(|n| n.contains("MySQL") && n.contains("PostgreSQL")), "{:?}", g.notes);
    }

    #[test]
    fn no_database_means_no_db_service_no_volume_and_no_db_env() {
        let g = gen(&python(), &ctx());
        let y = yaml(&g);
        assert_eq!(y["services"].as_mapping().unwrap().len(), 1);
        assert!(y.get("volumes").is_none());
        assert!(svc(&y, APP_SERVICE).get("depends_on").is_none());
        for (k, _) in env_of(&y, APP_SERVICE) {
            assert!(!k.starts_with("DB_") && !k.starts_with("SPRING_") && k != "DATABASE_URL", "{k}");
        }
    }

    // ---------- presets ----------

    fn preset_env_for(engine: DbEngine, version: &str, preset: DbEnvPreset) -> Vec<(String, String)> {
        let mut sp = python();
        sp.database = db(engine, version, "shop_db");
        sp.db_env_preset = preset;
        let y = yaml(&gen(&sp, &ctx()));
        env_of(&y, APP_SERVICE).into_iter().filter(|(k, _)| k != "HOME" && !k.starts_with("PYTHON")).collect()
    }

    fn kv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn standard_preset_per_engine() {
        assert_eq!(
            preset_env_for(DbEngine::Mysql, "8.0", DbEnvPreset::Standard),
            kv(&[
                ("DB_HOST", "mysql"),
                ("DB_PORT", "3306"),
                ("DB_NAME", "shop_db"),
                ("DB_USER", "localsync"),
                ("DB_PASSWORD", "localsync_pw"),
                ("DATABASE_URL", "mysql://localsync:localsync_pw@mysql:3306/shop_db")
            ])
        );
        assert_eq!(
            preset_env_for(DbEngine::Postgres, "16", DbEnvPreset::Standard),
            kv(&[
                ("DB_HOST", "postgres"),
                ("DB_PORT", "5432"),
                ("DB_NAME", "shop_db"),
                ("DB_USER", "localsync"),
                ("DB_PASSWORD", "localsync_pw"),
                ("DATABASE_URL", "postgres://localsync:localsync_pw@postgres:5432/shop_db")
            ])
        );
        assert_eq!(
            preset_env_for(DbEngine::Mongodb, "7.0", DbEnvPreset::Standard),
            kv(&[
                ("DB_HOST", "mongodb"),
                ("DB_PORT", "27017"),
                ("DB_NAME", "shop_db"),
                ("DB_USER", "localsync"),
                ("DB_PASSWORD", "localsync_pw"),
                ("DATABASE_URL", "mongodb://localsync:localsync_pw@mongodb:27017/shop_db?authSource=admin")
            ])
        );
    }

    #[test]
    fn spring_preset_per_engine() {
        assert_eq!(
            preset_env_for(DbEngine::Mysql, "8.0", DbEnvPreset::Spring),
            kv(&[
                ("SPRING_DATASOURCE_URL", "jdbc:mysql://mysql:3306/shop_db"),
                ("SPRING_DATASOURCE_USERNAME", "localsync"),
                ("SPRING_DATASOURCE_PASSWORD", "localsync_pw")
            ])
        );
        assert_eq!(
            preset_env_for(DbEngine::Postgres, "15", DbEnvPreset::Spring),
            kv(&[
                ("SPRING_DATASOURCE_URL", "jdbc:postgresql://postgres:5432/shop_db"),
                ("SPRING_DATASOURCE_USERNAME", "localsync"),
                ("SPRING_DATASOURCE_PASSWORD", "localsync_pw")
            ])
        );
        assert_eq!(
            preset_env_for(DbEngine::Mongodb, "6.0", DbEnvPreset::Spring),
            kv(&[("SPRING_DATA_MONGODB_URI", "mongodb://localsync:localsync_pw@mongodb:27017/shop_db?authSource=admin")])
        );
    }

    #[test]
    fn none_preset_injects_nothing() {
        assert!(preset_env_for(DbEngine::Mysql, "8.0", DbEnvPreset::None).is_empty());
    }

    // ---------- user env & rewrites ----------

    fn with_env(mut sp: ComposeSpec, rows: &[(&str, &str)]) -> ComposeSpec {
        sp.env = rows.iter().map(|(k, v)| EnvVar { key: k.to_string(), value: v.to_string() }).collect();
        sp
    }

    #[test]
    fn user_env_overrides_a_preset_key_and_keeps_all_others() {
        let mut sp = with_env(python(), &[("DB_HOST", "custom-host"), ("EXTRA", "1")]);
        sp.database = db(DbEngine::Mysql, "8.0", "shop_db");
        let y = yaml(&gen(&sp, &ctx()));
        assert_eq!(env_get(&y, "DB_HOST").as_deref(), Some("custom-host"));
        assert_eq!(env_get(&y, "DB_PORT").as_deref(), Some("3306"));
        assert_eq!(env_get(&y, "EXTRA").as_deref(), Some("1"));
        assert_eq!(env_of(&y, APP_SERVICE).iter().filter(|(k, _)| k == "DB_HOST").count(), 1);
    }

    #[test]
    fn localhost_with_the_database_port_is_rewritten_to_the_service_and_recorded() {
        let mut sp = with_env(java_maven(), &[("JDBC", "jdbc:mysql://localhost:3306/x"), ("OTHER", "127.0.0.1:3306")]);
        sp.database = db(DbEngine::Mysql, "8.0", "x");
        let g = gen(&sp, &ctx());
        let y = yaml(&g);
        assert_eq!(env_get(&y, "JDBC").as_deref(), Some("jdbc:mysql://mysql:3306/x"));
        assert_eq!(env_get(&y, "OTHER").as_deref(), Some("mysql:3306"));
        assert_eq!(
            g.rewrites,
            vec![
                Rewrite { key: "JDBC".into(), from: "jdbc:mysql://localhost:3306/x".into(), to: "jdbc:mysql://mysql:3306/x".into() },
                Rewrite { key: "OTHER".into(), from: "127.0.0.1:3306".into(), to: "mysql:3306".into() },
            ]
        );
    }

    #[test]
    fn unknown_ports_and_bare_localhost_are_left_alone() {
        let mut sp = with_env(
            python(),
            &[
                ("A", "http://localhost:9999/x"),
                ("B", "localhost"),
                ("C", "http://localhost/x"),
                ("D", "localhost:33060"),
                ("E", "mylocalhost:3306"),
                ("F", "localhost:3306x"),
            ],
        );
        sp.database = db(DbEngine::Mysql, "8.0", "x");
        let g = gen(&sp, &ctx());
        let y = yaml(&g);
        for (k, v) in [
            ("A", "http://localhost:9999/x"),
            ("B", "localhost"),
            ("C", "http://localhost/x"),
            ("D", "localhost:33060"),
            ("E", "mylocalhost:3306"),
        ] {
            assert_eq!(env_get(&y, k).as_deref(), Some(v), "{k}");
        }
        // "localhost:3306x" is the mysql port followed by a non-digit, so it is rewritten.
        assert_eq!(env_get(&y, "F").as_deref(), Some("mysql:3306x"));
        assert_eq!(g.rewrites.len(), 1);
        assert_eq!(g.rewrites[0].key, "F");
    }

    #[test]
    fn redis_url_is_rewritten_only_when_redis_is_selected() {
        let row = [("REDIS_URL", "redis://127.0.0.1:6379")];
        let without = gen(&with_env(python(), &row), &ctx());
        assert_eq!(env_get(&yaml(&without), "REDIS_URL").as_deref(), Some("redis://127.0.0.1:6379"));
        assert!(without.rewrites.is_empty());

        let mut sp = with_env(python(), &row);
        sp.extras = vec![ExtraService { kind: ExtraKind::Redis, version: "7".into() }];
        let with = gen(&sp, &ctx());
        assert_eq!(env_get(&yaml(&with), "REDIS_URL").as_deref(), Some("redis://redis:6379"));
        assert_eq!(
            with.rewrites,
            vec![Rewrite { key: "REDIS_URL".into(), from: "redis://127.0.0.1:6379".into(), to: "redis://redis:6379".into() }]
        );
    }

    #[test]
    fn env_values_that_look_like_yaml_scalars_stay_strings() {
        let vals = [("A", "true"), ("B", "8080"), ("C", "null"), ("D", "a: b # c"), ("E", "'quoted'"), ("F", "- x"), ("G", "")];
        let y = yaml(&gen(&with_env(python(), &vals), &ctx()));
        for (k, v) in vals {
            assert_eq!(env_get(&y, k).as_deref(), Some(v), "{k}");
        }
    }

    // ---------- extras ----------

    #[test]
    fn extras_get_catalog_images_health_checks_and_matching_depends_on() {
        let mut sp = python();
        sp.extras = vec![
            ExtraService { kind: ExtraKind::Redis, version: "6".into() },
            ExtraService { kind: ExtraKind::Rabbitmq, version: "3.13".into() },
            ExtraService { kind: ExtraKind::Memcached, version: "1.6".into() },
        ];
        let y = yaml(&gen(&sp, &ctx()));
        assert_eq!(svc(&y, "redis")["image"], "docker.io/library/redis:6");
        assert_eq!(strs(&svc(&y, "redis")["healthcheck"]["test"]), ["CMD", "redis-cli", "ping"]);
        assert_eq!(svc(&y, "rabbitmq")["image"], "docker.io/library/rabbitmq:3.13-alpine");
        assert_eq!(strs(&svc(&y, "rabbitmq")["healthcheck"]["test"]), ["CMD", "rabbitmq-diagnostics", "-q", "ping"]);
        assert_eq!(svc(&y, "memcached")["image"], "docker.io/library/memcached:1.6");
        assert!(svc(&y, "memcached").get("healthcheck").is_none());
        let deps = &svc(&y, APP_SERVICE)["depends_on"];
        assert_eq!(deps["redis"]["condition"], "service_healthy");
        assert_eq!(deps["rabbitmq"]["condition"], "service_healthy");
        assert_eq!(deps["memcached"]["condition"], "service_started");
    }

    // ---------- context & determinism ----------

    #[test]
    fn host_port_override_changes_only_the_host_side() {
        let g = gen(&java_maven(), &GenerateContext { host_port: Some(18080), ..ctx() });
        assert_eq!(strs(&svc(&yaml(&g), APP_SERVICE)["ports"]), ["18080:8080"]);
        assert_eq!(g.host_port, 18080);
    }

    #[test]
    fn same_spec_generates_byte_identical_output() {
        let mut sp = with_env(java_maven(), &[("Z", "1"), ("A", "localhost:3306"), ("M", "x")]);
        sp.database = db(DbEngine::Mysql, "8.0", "shop_db");
        sp.extras = vec![
            ExtraService { kind: ExtraKind::Redis, version: "7".into() },
            ExtraService { kind: ExtraKind::Memcached, version: "1.6".into() },
        ];
        let c = ctx_dump(DbEngine::Mysql);
        let (a, b) = (gen(&sp, &c), gen(&sp, &c));
        assert_eq!(a, b);
        assert_eq!(a.compose_yaml, b.compose_yaml);
        // User rows keep the order the person entered them in.
        let keys: Vec<String> = env_of(&yaml(&a), APP_SERVICE).into_iter().map(|(k, _)| k).collect();
        let pos = |k: &str| keys.iter().position(|x| x == k).unwrap();
        assert!(pos("Z") < pos("A") && pos("A") < pos("M"));
    }

    // ---------- round trip through the receiver's own compose code ----------

    fn round_trip(sp: &ComposeSpec, c: &GenerateContext) -> (Generated, Value) {
        let g = gen(sp, c);
        let db_services: Vec<String> = sp.database.iter().map(|d| db_service_name(d.engine).to_string()).collect();
        let policy = ls_security::default_policy();
        let out = receiver_compose::apply_policy(&g.compose_yaml, &policy, &db_services, "localsync-db-test")
            .expect("apply_policy accepts the generated compose");
        let ports = receiver_compose::parse_service_ports(&out).expect("parse_service_ports accepts it");
        assert_eq!(ports, vec![(APP_SERVICE.to_string(), format!("{}:{}", g.host_port, sp.port))]);
        let y: Value = serde_yaml::from_str(&out).unwrap();
        for (name, s) in y["services"].as_mapping().unwrap() {
            assert_eq!(s["read_only"], true, "{name:?}");
            assert_eq!(strs(&s["tmpfs"]), ["/tmp"]);
        }
        (g, y)
    }

    #[test]
    fn java_mysql_round_trips_through_apply_policy() {
        let mut sp = java_maven();
        sp.database = db(DbEngine::Mysql, "8.0", "shop_db");
        sp.extras = vec![ExtraService { kind: ExtraKind::Redis, version: "7".into() }];
        let (_, y) = round_trip(&sp, &ctx_dump(DbEngine::Mysql));
        assert_eq!(svc(&y, APP_SERVICE)["build"]["context"], "source", "\".\" is rewritten into the snapshot's source/ dir");
        assert_eq!(svc(&y, APP_SERVICE)["build"]["dockerfile"], DOCKERFILE_NAME);
        assert_eq!(y["volumes"]["db-data"]["name"], "localsync-db-test", "the DB data volume was found and pinned");
        assert_eq!(svc(&y, "mysql")["image"], "docker.io/library/mysql:8.0");
    }

    #[test]
    fn python_round_trips_through_apply_policy() {
        round_trip(&python(), &GenerateContext { host_port: Some(15000), ..ctx() });
    }

    #[test]
    fn postgres_round_trips_and_pins_the_data_volume() {
        let mut sp = python();
        sp.database = db(DbEngine::Postgres, "16", "shop_db");
        let (_, y) = round_trip(&sp, &ctx());
        assert_eq!(y["volumes"]["db-data"]["name"], "localsync-db-test");
    }

    // ---------- public entry point ----------

    #[test]
    fn public_generate_validates_then_generates() {
        assert!(generate(&java_maven(), &ctx()).is_ok());
        let mut bad = java_maven();
        bad.run_command = String::new();
        assert!(matches!(generate(&bad, &ctx()), Err(GenerateError::Invalid(_))));
    }
}
