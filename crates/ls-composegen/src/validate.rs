//! Validation: rejects a bad [`ComposeSpec`] with a specific, actionable
//! message per field *before* anything is generated.
//!
//! Every allowed value is looked up in [`crate::catalog`] (never a second
//! copy here), so the UI's dropdowns and these checks can't disagree.

use std::collections::HashMap;

use crate::catalog::{self, build_tool_label};
use crate::spec::{ComposeSpec, FieldError};

const MAX_RUN_COMMAND: usize = 500;
const MAX_ARTIFACT_PATH: usize = 200;
const MAX_DB_NAME: usize = 63;
const MAX_ENV_ROWS: usize = 100;

fn err(field: impl Into<String>, message: impl Into<String>) -> FieldError {
    FieldError { field: field.into(), message: message.into() }
}

/// `^[A-Za-z_][A-Za-z0-9_]*$`
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Only for a spec whose `versions` came from the catalog: checks `version`
/// is one of them, else says which are allowed.
fn check_version(errors: &mut Vec<FieldError>, field: String, what: &str, version: &str, allowed: &[&str]) {
    if !allowed.contains(&version) {
        errors.push(err(
            field,
            format!("{what} version \"{version}\" isn't supported. Choose one of: {}.", allowed.join(", ")),
        ));
    }
}

/// `Some(message)` if `path` isn't a plausible relative build-output path.
fn artifact_path_problem(path: &str) -> Option<String> {
    let example = "Use a path relative to the project folder, like target/*.jar (Maven) or build/libs/*.jar (Gradle).";
    if path.trim().is_empty() {
        return Some(format!("Enter where the build leaves the jar. {example}"));
    }
    if path.chars().any(char::is_control) {
        return Some(format!("The artifact path can't contain newlines or control characters. {example}"));
    }
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if path.starts_with('/') || path.starts_with('\\') || drive {
        return Some(format!("\"{path}\" is an absolute path, but it must be relative to the project folder. {example}"));
    }
    if path.split(['/', '\\']).any(|seg| seg == "..") {
        return Some(format!("\"{path}\" contains \"..\", but the artifact must be inside the project folder. {example}"));
    }
    if path.chars().count() > MAX_ARTIFACT_PATH {
        return Some(format!("The artifact path is longer than {MAX_ARTIFACT_PATH} characters. {example}"));
    }
    if let Some(bad) = path.chars().find(|c| !(c.is_ascii_alphanumeric() || "._-/*?[]+@".contains(*c))) {
        return Some(format!(
            "\"{path}\" contains {bad:?}, which isn't allowed. Use only letters, digits and . _ - / * ? [ ] + @ (no spaces). {example}"
        ));
    }
    None
}

/// Every problem with `spec`, all at once (not just the first), each naming
/// the field and saying what would fix it. `Ok(())` only if the spec is
/// complete and every value is one the catalog supports.
pub fn validate(spec: &ComposeSpec) -> Result<(), Vec<FieldError>> {
    let mut errors = Vec::new();
    let rt = catalog::runtime_info(spec.runtime);

    check_version(&mut errors, "runtime_version".into(), rt.label, &spec.runtime_version, rt.versions);

    if !rt.build_tools.contains(&spec.build_tool) {
        let tools: Vec<&str> = rt.build_tools.iter().map(|&t| build_tool_label(t)).collect();
        errors.push(err(
            "build_tool",
            format!(
                "{} doesn't support {}. {} supports: {}.",
                rt.label,
                build_tool_label(spec.build_tool),
                rt.label,
                tools.join(", ")
            ),
        ));
    }

    let cmd = spec.run_command.trim();
    if cmd.is_empty() {
        errors.push(err("run_command", "Enter the command that starts your app (for example: npm start)."));
    } else if spec.run_command.chars().any(char::is_control) {
        errors.push(err("run_command", "The run command must be a single line with no newlines or control characters. Put multiple steps in a script and run that."));
    } else if spec.run_command.chars().count() > MAX_RUN_COMMAND {
        errors.push(err("run_command", format!("The run command is longer than {MAX_RUN_COMMAND} characters. Move the details into a script and run that.")));
    }

    match spec.port {
        0 => errors.push(err("port", "Enter the port your app listens on, a number between 1024 and 65535.")),
        1..=1023 => errors.push(err(
            "port",
            format!(
                "Port {} is below 1024. Podman runs without admin rights and can't publish host ports below 1024. Make your app listen on a port of 1024 or higher (for example 8080) and enter that here.",
                spec.port
            ),
        )),
        _ => {}
    }

    if catalog::needs_artifact_path(spec.runtime) {
        match spec.artifact_path.as_deref() {
            None => errors.push(err("artifact_path", "Enter where the build leaves the jar, relative to the project folder, like target/*.jar (Maven) or build/libs/*.jar (Gradle).")),
            Some(p) => {
                if let Some(m) = artifact_path_problem(p) {
                    errors.push(err("artifact_path", m));
                }
            }
        }
    }

    if let Some(db) = &spec.database {
        let info = catalog::db_info(db.engine);
        check_version(&mut errors, "database.version".into(), info.label, &db.version, info.versions);
        if !is_identifier(&db.database) || db.database.chars().count() > MAX_DB_NAME {
            errors.push(err(
                "database.database",
                format!(
                    "Database name \"{}\" isn't valid. Use up to {MAX_DB_NAME} letters, digits and underscores, not starting with a digit (for example: my_app).",
                    db.database
                ),
            ));
        }
    }

    let mut seen_extras = Vec::new();
    for (i, extra) in spec.extras.iter().enumerate() {
        let info = catalog::extra_info(extra.kind);
        if seen_extras.contains(&extra.kind) {
            errors.push(err(format!("extras[{i}].kind"), format!("{} is added more than once. Keep just one {} entry.", info.label, info.label)));
        } else {
            seen_extras.push(extra.kind);
        }
        check_version(&mut errors, format!("extras[{i}].version"), info.label, &extra.version, info.versions);
    }

    if spec.env.len() > MAX_ENV_ROWS {
        errors.push(err("env", format!("There are {} environment variables; the limit is {MAX_ENV_ROWS}. Remove some, or read them from a file inside your project instead.", spec.env.len())));
    }
    let mut first_row: HashMap<&str, usize> = HashMap::new();
    for (i, var) in spec.env.iter().enumerate() {
        if var.key.is_empty() {
            errors.push(err(format!("env[{i}].key"), "The variable name is empty. Enter a name like API_URL, or remove this row."));
        } else if !is_identifier(&var.key) {
            errors.push(err(
                format!("env[{i}].key"),
                format!("\"{}\" isn't a valid variable name. Use letters, digits and underscores, not starting with a digit (for example: API_URL).", var.key),
            ));
        } else if let Some(&earlier) = first_row.get(var.key.as_str()) {
            errors.push(err(
                format!("env[{i}].key"),
                format!("\"{}\" is already set in row {}. Remove one of the two, or rename it.", var.key, earlier + 1),
            ));
        } else {
            first_row.insert(&var.key, i);
        }
        if var.value.contains(['\0', '\n', '\r']) {
            errors.push(err(format!("env[{i}].value"), "The value can't contain a newline or NUL character. Use a single line."));
        }
    }

    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::*;

    fn java() -> ComposeSpec {
        ComposeSpec {
            runtime: Runtime::Java,
            runtime_version: "21".into(),
            build_tool: BuildTool::Maven,
            run_command: "java -jar /app/app.jar".into(),
            port: 8080,
            artifact_path: Some("target/*.jar".into()),
            database: None,
            db_env_preset: DbEnvPreset::Standard,
            extras: vec![],
            env: vec![],
        }
    }

    fn node() -> ComposeSpec {
        ComposeSpec { runtime: Runtime::Node, runtime_version: "20".into(), build_tool: BuildTool::Yarn, run_command: "yarn start".into(), port: 3000, artifact_path: None, ..java() }
    }

    fn python() -> ComposeSpec {
        ComposeSpec { runtime: Runtime::Python, runtime_version: "3.12".into(), build_tool: BuildTool::Pip, run_command: "python app.py".into(), port: 5000, artifact_path: None, ..java() }
    }

    fn go() -> ComposeSpec {
        ComposeSpec { runtime: Runtime::Go, runtime_version: "1.22".into(), build_tool: BuildTool::GoBuild, run_command: "/app/app".into(), port: 8081, artifact_path: None, ..java() }
    }

    fn errs(spec: &ComposeSpec) -> Vec<FieldError> {
        validate(spec).expect_err("expected validation to fail")
    }

    /// The single error for `field`; fails if there are none or other fields also failed.
    fn only(spec: &ComposeSpec, field: &str) -> String {
        let e = errs(spec);
        assert_eq!(e.len(), 1, "expected exactly one error, got {e:?}");
        assert_eq!(e[0].field, field);
        e[0].message.clone()
    }

    fn fields(spec: &ComposeSpec) -> Vec<String> {
        let mut f: Vec<String> = errs(spec).into_iter().map(|e| e.field).collect();
        f.sort();
        f
    }

    fn var(k: &str, v: &str) -> EnvVar {
        EnvVar { key: k.into(), value: v.into() }
    }

    #[test]
    fn a_valid_spec_for_each_runtime_passes() {
        assert_eq!(validate(&java()), Ok(()));
        assert_eq!(validate(&node()), Ok(()));
        assert_eq!(validate(&python()), Ok(()));
        assert_eq!(validate(&go()), Ok(()));
    }

    #[test]
    fn every_catalog_version_and_tool_is_accepted() {
        for r in catalog::RUNTIMES {
            for v in r.versions {
                for &t in r.build_tools {
                    let s = ComposeSpec { runtime: r.runtime, runtime_version: v.to_string(), build_tool: t, ..java() };
                    assert_eq!(validate(&s), Ok(()), "{} {v} {t:?}", r.label);
                }
            }
        }
    }

    #[test]
    fn bad_runtime_version() {
        let m = only(&ComposeSpec { runtime_version: "8".into(), ..java() }, "runtime_version");
        assert!(m.contains("17") && m.contains("21"), "{m}");
    }

    #[test]
    fn unsupported_build_tool_pairings_fail_and_name_the_supported_tools() {
        let m = only(&ComposeSpec { build_tool: BuildTool::Npm, ..java() }, "build_tool");
        assert!(m.contains("Maven") && m.contains("Gradle"), "{m}");
        let m = only(&ComposeSpec { build_tool: BuildTool::Maven, ..node() }, "build_tool");
        assert!(m.contains("npm") && m.contains("Yarn") && m.contains("pnpm"), "{m}");
        let m = only(&ComposeSpec { build_tool: BuildTool::Gradle, ..python() }, "build_tool");
        assert!(m.contains("pip"), "{m}");
        let m = only(&ComposeSpec { build_tool: BuildTool::Pip, ..go() }, "build_tool");
        assert!(m.contains("go build"), "{m}");
    }

    #[test]
    fn run_command_empty_or_whitespace() {
        only(&ComposeSpec { run_command: "".into(), ..node() }, "run_command");
        only(&ComposeSpec { run_command: "  \t ".into(), ..node() }, "run_command");
    }

    #[test]
    fn run_command_with_newline_or_control_char() {
        only(&ComposeSpec { run_command: "npm start\nrm -rf /".into(), ..node() }, "run_command");
        only(&ComposeSpec { run_command: "npm start\u{7}".into(), ..node() }, "run_command");
    }

    #[test]
    fn run_command_length_limit() {
        assert_eq!(validate(&ComposeSpec { run_command: "a".repeat(500), ..node() }), Ok(()));
        only(&ComposeSpec { run_command: "a".repeat(501), ..node() }, "run_command");
    }

    #[test]
    fn port_zero_and_privileged_ports() {
        let m = only(&ComposeSpec { port: 0, ..node() }, "port");
        assert!(m.contains("1024") && m.contains("65535"), "{m}");
        for p in [1u16, 80, 1023] {
            let m = only(&ComposeSpec { port: p, ..node() }, "port");
            assert!(m.contains("below 1024") && m.contains("Podman") && m.contains("1024 or higher"), "{m}");
        }
    }

    #[test]
    fn port_boundaries_are_accepted() {
        assert_eq!(validate(&ComposeSpec { port: 1024, ..node() }), Ok(()));
        assert_eq!(validate(&ComposeSpec { port: 65535, ..node() }), Ok(()));
    }

    #[test]
    fn java_requires_an_artifact_path() {
        let m = only(&ComposeSpec { artifact_path: None, ..java() }, "artifact_path");
        assert!(m.contains("target/*.jar"), "{m}");
        only(&ComposeSpec { artifact_path: Some("".into()), ..java() }, "artifact_path");
    }

    #[test]
    fn artifact_path_shape_rules() {
        let bad = [
            "/abs/app.jar",
            "\\share\\app.jar",
            "C:/x/app.jar",
            "c:app.jar",
            "../app.jar",
            "target/../../app.jar",
            "target\\..\\app.jar",
            "   ",
            "target/app.jar\n",
            "target/my app.jar",
            "target/app$.jar",
            "target/{a,b}.jar",
        ];
        for p in bad {
            let m = only(&ComposeSpec { artifact_path: Some(p.into()), ..java() }, "artifact_path");
            assert!(m.contains("target/*.jar"), "{p:?}: {m}");
        }
        only(&ComposeSpec { artifact_path: Some("a".repeat(201)), ..java() }, "artifact_path");
    }

    #[test]
    fn artifact_path_accepts_globs_and_the_length_limit() {
        for p in ["target/*.jar", "build/libs/*.jar", "app-1.0_x+y@z/[a-z]?.jar", "out..name/app.jar", &"a".repeat(200)] {
            assert_eq!(validate(&ComposeSpec { artifact_path: Some(p.into()), ..java() }), Ok(()), "{p}");
        }
    }

    #[test]
    fn artifact_path_is_ignored_for_non_java_runtimes() {
        for base in [node(), python(), go()] {
            let s = ComposeSpec { artifact_path: Some("/../junk path\n".into()), ..base };
            assert_eq!(validate(&s), Ok(()));
        }
    }

    fn db(engine: DbEngine, version: &str, name: &str) -> Option<DatabaseSpec> {
        Some(DatabaseSpec { engine, version: version.into(), database: name.into() })
    }

    #[test]
    fn valid_database_passes() {
        assert_eq!(validate(&ComposeSpec { database: db(DbEngine::Postgres, "16", "my_app1"), ..node() }), Ok(()));
        assert_eq!(validate(&ComposeSpec { database: db(DbEngine::Mysql, "8.0", &"a".repeat(63)), ..node() }), Ok(()));
    }

    #[test]
    fn bad_database_version_lists_that_engines_versions() {
        let m = only(&ComposeSpec { database: db(DbEngine::Postgres, "9", "app"), ..node() }, "database.version");
        assert!(m.contains("14") && m.contains("15") && m.contains("16"), "{m}");
        // The version list is per engine: a valid Postgres version is wrong for MySQL.
        only(&ComposeSpec { database: db(DbEngine::Mysql, "16", "app"), ..node() }, "database.version");
    }

    #[test]
    fn bad_database_names() {
        for name in ["", "1app", "my-app", "my app", "app;drop", "é", &"a".repeat(64)] {
            only(&ComposeSpec { database: db(DbEngine::Postgres, "16", name), ..node() }, "database.database");
        }
    }

    fn extra(kind: ExtraKind, version: &str) -> ExtraService {
        ExtraService { kind, version: version.into() }
    }

    #[test]
    fn valid_extras_pass() {
        let s = ComposeSpec { extras: vec![extra(ExtraKind::Redis, "7"), extra(ExtraKind::Rabbitmq, "3.13"), extra(ExtraKind::Memcached, "1.6")], ..node() };
        assert_eq!(validate(&s), Ok(()));
    }

    #[test]
    fn duplicate_extras_are_reported_on_the_later_one() {
        let s = ComposeSpec { extras: vec![extra(ExtraKind::Redis, "6"), extra(ExtraKind::Memcached, "1.6"), extra(ExtraKind::Redis, "7")], ..node() };
        let m = only(&s, "extras[2].kind");
        assert!(m.contains("Redis"), "{m}");
    }

    #[test]
    fn bad_extra_version_lists_allowed_versions() {
        let m = only(&ComposeSpec { extras: vec![extra(ExtraKind::Rabbitmq, "4")], ..node() }, "extras[0].version");
        assert!(m.contains("3.12") && m.contains("3.13"), "{m}");
    }

    #[test]
    fn env_key_problems() {
        only(&ComposeSpec { env: vec![var("", "x")], ..node() }, "env[0].key");
        only(&ComposeSpec { env: vec![var("MY-KEY", "x")], ..node() }, "env[0].key");
        only(&ComposeSpec { env: vec![var("1KEY", "x")], ..node() }, "env[0].key");
        only(&ComposeSpec { env: vec![var("A B", "x")], ..node() }, "env[0].key");
    }

    #[test]
    fn duplicate_env_key_is_reported_on_the_later_row_naming_the_earlier() {
        let s = ComposeSpec { env: vec![var("A", "1"), var("B", "2"), var("A", "3")], ..node() };
        let e = errs(&s);
        assert_eq!(e.len(), 1, "{e:?}");
        assert_eq!(e[0].field, "env[2].key");
        assert!(e[0].message.contains("row 1"), "{}", e[0].message);
        // case-sensitive: these are different keys
        assert_eq!(validate(&ComposeSpec { env: vec![var("a", "1"), var("A", "2")], ..node() }), Ok(()));
    }

    #[test]
    fn env_row_limit() {
        let rows = |n: usize| (0..n).map(|i| var(&format!("K{i}"), "v")).collect::<Vec<_>>();
        assert_eq!(validate(&ComposeSpec { env: rows(100), ..node() }), Ok(()));
        only(&ComposeSpec { env: rows(101), ..node() }, "env");
    }

    #[test]
    fn env_values() {
        assert_eq!(validate(&ComposeSpec { env: vec![var("EMPTY", ""), var("URL", "http://x?a=b c")], ..node() }), Ok(()), "empty and spaced values are fine");
        only(&ComposeSpec { env: vec![var("A", "line1\nline2")], ..node() }, "env[0].value");
        only(&ComposeSpec { env: vec![var("A", "nul\0byte")], ..node() }, "env[0].value");
    }

    #[test]
    fn several_problems_are_all_returned_at_once() {
        let s = ComposeSpec {
            runtime_version: "8".into(),
            build_tool: BuildTool::Npm,
            run_command: " ".into(),
            port: 80,
            artifact_path: Some("/abs.jar".into()),
            database: db(DbEngine::Postgres, "9", "1bad"),
            extras: vec![extra(ExtraKind::Redis, "5"), extra(ExtraKind::Redis, "7")],
            env: vec![var("", "x"), var("A", "a\nb"), var("A", "c")],
            ..java()
        };
        assert_eq!(
            fields(&s),
            [
                "artifact_path", "build_tool", "database.database", "database.version", "env[0].key", "env[1].value",
                "env[2].key", "extras[0].version", "extras[1].kind", "port", "run_command", "runtime_version"
            ]
        );
    }

    #[test]
    fn every_message_is_non_empty_and_catalog_messages_list_the_allowed_values() {
        let s = ComposeSpec {
            runtime_version: "8".into(),
            build_tool: BuildTool::Npm,
            database: db(DbEngine::Mongodb, "5", "app"),
            extras: vec![extra(ExtraKind::Redis, "5")],
            ..java()
        };
        let e = errs(&s);
        assert_eq!(e.len(), 4);
        for fe in &e {
            assert!(!fe.message.trim().is_empty() && !fe.field.is_empty(), "{fe:?}");
            let allowed: &[&str] = match fe.field.as_str() {
                "runtime_version" => catalog::runtime_info(Runtime::Java).versions,
                "database.version" => catalog::db_info(DbEngine::Mongodb).versions,
                "extras[0].version" => catalog::extra_info(ExtraKind::Redis).versions,
                "build_tool" => &["Maven", "Gradle"],
                other => panic!("unexpected field {other}"),
            };
            for a in allowed {
                assert!(fe.message.contains(a), "{}: missing {a} in {}", fe.field, fe.message);
            }
        }
    }
}
