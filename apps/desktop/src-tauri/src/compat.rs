//! "Is this update still compatible?" - run before a Push update / Send
//! again goes out.
//!
//! Quick checks always run (no containers): the saved compose settings still
//! validate, the dump file is still there, the build file and packaging still
//! match the settings, and the config files that changed since the last
//! device's version carry no *new* absolute path to a file outside the
//! project. A changed file that now holds a secret-looking value is a note,
//! never a problem.
//!
//! When the changes touch build or config files (or a chosen file from
//! outside the project was edited), the folder is also
//! test-run exactly like the Setup wizard's test run (`compose_wizard::
//! run_compose_test`: packaged as sent, built and started in Podman with a
//! temp host port and a fresh database volume). A pass is remembered for
//! that folder's tree id, so the same version isn't booted twice.
//!
//! A folder that ships its own docker-compose.yml has no `ComposeSpec`;
//! `run_compose_test` can only boot generated settings, so such a folder gets
//! the quick checks and a note instead of a test run.
//!
//! Reads the project only (read-only git); never writes to it.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::commands::FolderPlanDto;
use crate::project_session::ProjectSession;
use crate::state::AppState;

const GIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Bigger changed files are not scanned for paths or secrets.
const MAX_SCAN_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct CompatTest {
    pub ok: bool,
    pub error: Option<String>,
    pub output_tail: String,
    pub secs: u64,
    /// Passed earlier for this exact version; not booted again.
    pub cached: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompatReport {
    /// Nothing blocks the send.
    pub ok: bool,
    /// Each one blocks the default path (the person can still Send anyway).
    pub problems: Vec<String>,
    /// Never block.
    pub notes: Vec<String>,
    /// The test run, when one was needed (and possible).
    pub test: Option<CompatTest>,
    /// The folders' tree ids being checked, joined: what a pass is tied to.
    pub tree_key: String,
}

/// Passed test runs, keyed by [`pass_key`]. ponytail: in memory only, so a
/// restart tests again once; persist it if that ever matters.
static PASSED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn pass_key(folder: &FolderPlanDto, tree: &str) -> String {
    let dump = folder.dump.as_ref().map(|d| (&d.schema, &d.file_path, &d.engine));
    // The external files travel with the version too: editing one is a new version to test.
    let externals = crate::externals::externals_fingerprint(&folder.externals);
    format!("{}|{tree}|{}", folder.path, serde_json::to_string(&(&folder.compose, dump, externals)).unwrap_or_default())
}

fn passed(key: &str) -> bool {
    PASSED.lock().map(|p| p.as_ref().is_some_and(|s| s.contains(key))).unwrap_or(false)
}

fn remember_pass(key: String) {
    if let Ok(mut p) = PASSED.lock() {
        p.get_or_insert_with(HashSet::new).insert(key);
    }
}

#[tauri::command]
pub async fn check_update_compat<R: tauri::Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    session_id: String,
) -> Result<CompatReport, String> {
    let session = state
        .project_sessions
        .lock()
        .map_err(|e| e.to_string())?
        .get(&session_id)
        .cloned()
        .ok_or_else(|| format!("no open session with id {session_id}"))?;
    Ok(check_session(app, &session, crate::compose_wizard::wait_timeout()).await)
}

/// What one folder looks like now against what the last device received.
struct FolderState {
    current: String,
    tree: String,
    /// The last device's commit for this folder, if it has one.
    base: Option<String>,
    /// Changed since `base` (relative to the folder); empty with no base.
    changed: Vec<String>,
}

/// `selection`: the session's choice of new files, so `current` is the
/// commit a send would build from.
fn folder_state(root: &Path, base: Option<String>, selection: &ls_snapshot::NewFileSelection) -> Result<FolderState, String> {
    let current = ls_snapshot::snapshot_commit_with(root, selection).map_err(|e| format!("reading {}: {e:#}", root.display()))?;
    let tree = ls_snapshot::folder_tree(root, &current).unwrap_or_else(|_| current.clone());
    let changed = match &base {
        None => Vec::new(),
        Some(b) => changed_files(root, b, &current).unwrap_or_else(|_| {
            // The device's commit is gone (history rewritten, or pruned):
            // compare against nothing, so every file counts as changed.
            git_lines(root, &["ls-tree", "-r", "--name-only", &current]).unwrap_or_default()
        }),
    };
    Ok(FolderState { current, tree, base, changed })
}

/// Files that differ between `base` and `current`, relative to the folder.
/// The merger may swap this for the Linux branch's `project_changes`.
fn changed_files(root: &Path, base: &str, current: &str) -> anyhow::Result<Vec<String>> {
    git_lines(root, &["diff", "--name-only", "--no-renames", "--relative", base, current, "--"])
}

fn git_lines(root: &Path, args: &[&str]) -> anyhow::Result<Vec<String>> {
    let out = ls_snapshot::run_git(root, args, &[], GIT_TIMEOUT)?;
    Ok(String::from_utf8_lossy(&out).lines().filter(|l| !l.is_empty()).map(String::from).collect())
}

/// `path`'s text at `commit`; `None` when it isn't there (or isn't text-sized).
fn file_at(root: &Path, commit: &str, path: &str) -> Option<String> {
    let bytes = ls_snapshot::run_git(root, &["show", &format!("{commit}:./{path}")], &[], GIT_TIMEOUT).ok()?;
    (bytes.len() <= MAX_SCAN_BYTES).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

pub async fn check_session<R: tauri::Runtime>(app: AppHandle<R>, session: &ProjectSession, wait: Duration) -> CompatReport {
    let mut report = CompatReport { ok: true, problems: vec![], notes: vec![], test: None, tree_key: String::new() };
    let marker = session.last_device().and_then(|d| d.marker.clone());
    let mut trees = Vec::new();
    let mut to_test = Vec::new();

    for folder in &session.folders {
        let name = folder_name(folder);
        let root = Path::new(&folder.path).to_path_buf();
        let sent = marker.as_ref().and_then(|m| m.commits.iter().find(|c| c.path == folder.path));
        let base = sent.map(|c| c.commit.clone());
        // A file from outside the project (config the app reads) edited since
        // that device's version: as likely to break the start as a build file.
        let externals_changed = sent.is_some_and(|c| c.externals != crate::externals::externals_fingerprints(&folder.externals));
        let selection = session.new_file_selection(&folder.path);
        let st = match tauri::async_runtime::spawn_blocking(move || folder_state(&root, base, &selection)).await {
            Ok(Ok(st)) => st,
            Ok(Err(e)) => {
                report.problems.push(e);
                continue;
            }
            Err(e) => {
                report.problems.push(format!("reading {name}: {e}"));
                continue;
            }
        };
        trees.push(st.tree.clone());

        let info = crate::compose_wizard::inspect_project(folder.path.clone()).await;
        report.problems.extend(setup_problems(folder, &name, info.war_packaging));

        let root = Path::new(&folder.path).to_path_buf();
        let (scan_folder, current, base, changed) = (folder.clone(), st.current.clone(), st.base.clone(), st.changed.clone());
        let (paths, secrets) = tauri::async_runtime::spawn_blocking(move || {
            let mut paths = Vec::new();
            let mut secrets = Vec::new();
            for file in changed.iter().filter(|f| is_config_file(f)) {
                let Some(new) = file_at(&root, &current, file) else { continue };
                let old = base.as_deref().and_then(|b| file_at(&root, b, file)).unwrap_or_default();
                paths.extend(new_host_paths(&scan_folder, file, &old, &new));
                secrets.extend(new_secrets(file, &old, &new));
            }
            (paths, secrets)
        })
        .await
        .unwrap_or_default();
        report.problems.extend(paths);
        report.notes.extend(secrets);

        if needs_test_run(&st.changed) || externals_changed {
            if folder.compose.is_some() {
                to_test.push((folder.clone(), st.tree.clone()));
            } else {
                report.notes.push(format!("{name} uses its own docker-compose.yml, so it isn't test-run before sending."));
            }
        }
    }
    report.tree_key = trees.join(",");

    // A test run of settings that already fail a quick check would only
    // repeat that failure, slowly.
    if !to_test.is_empty() && report.problems.is_empty() {
        let test = run_tests(app, to_test, wait).await;
        // The run's own Podman gate (`ensure_ready_for_run`) said no: that is
        // this computer, not the update.
        if test.error.as_deref().is_some_and(|e| e.contains(ls_containers::readiness::NOT_READY_PREFIX)) {
            report.notes.push("Podman isn't available, so only the quick checks ran.".to_string());
        } else {
            report.test = Some(test);
        }
    }

    if let Some(t) = &report.test {
        if !t.ok {
            report.problems.push(t.error.clone().unwrap_or_else(|| "The test run failed.".to_string()).lines().next().unwrap_or_default().to_string());
        }
    }
    report.ok = report.problems.is_empty();
    report
}

/// Test-runs each folder (stopping at the first failure), skipping a
/// version that already passed.
async fn run_tests<R: tauri::Runtime>(app: AppHandle<R>, folders: Vec<(FolderPlanDto, String)>, wait: Duration) -> CompatTest {
    let started = Instant::now();
    let mut out = CompatTest { ok: true, error: None, output_tail: String::new(), secs: 0, cached: true };
    for (folder, tree) in folders {
        let key = pass_key(&folder, &tree);
        if passed(&key) {
            continue;
        }
        out.cached = false;
        match crate::compose_wizard::run_compose_test(app.clone(), folder, wait).await {
            Ok(r) if r.ok => remember_pass(key),
            Ok(r) => {
                out.ok = false;
                out.error = r.error;
                out.output_tail = r.output_tail;
                break;
            }
            Err(e) => {
                out.ok = false;
                out.error = Some(e);
                break;
            }
        }
    }
    out.secs = started.elapsed().as_secs();
    out
}

fn folder_name(folder: &FolderPlanDto) -> String {
    Path::new(&folder.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| folder.path.clone())
}

// ---------- the quick checks ----------

/// The saved setup against the folder as it is now: settings still valid,
/// dump file still there, build file and packaging still what the settings
/// say. `war_packaging`: the folder's pom.xml says `<packaging>war`.
fn setup_problems(folder: &FolderPlanDto, name: &str, war_packaging: bool) -> Vec<String> {
    use ls_composegen::{BuildTool, JavaPackaging};
    let root = Path::new(&folder.path);
    let mut problems = Vec::new();
    if let Some(dump) = &folder.dump {
        if !Path::new(&dump.file_path).is_file() {
            problems.push(format!("The database dump {} is missing.", dump.file_path));
        }
    }
    let Some(spec) = &folder.compose else {
        if !root.join("docker-compose.yml").is_file() {
            problems.push(format!("{name} has no docker-compose.yml any more."));
        }
        return problems;
    };
    if let Err(errors) = ls_composegen::validate(spec) {
        let msgs: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        problems.push(format!("The setup for {name} is no longer valid: {}", msgs.join(" ")));
    }
    let build_files: &[&str] = match spec.build_tool {
        BuildTool::Maven => &["pom.xml"],
        BuildTool::Gradle => &["build.gradle", "build.gradle.kts"],
        BuildTool::Npm | BuildTool::Yarn | BuildTool::Pnpm => &["package.json"],
        BuildTool::Pip => &["requirements.txt", "pyproject.toml"],
        BuildTool::GoBuild => &["go.mod"],
    };
    // pip needs no file to build (a plain script runs as is).
    if spec.build_tool != BuildTool::Pip && !build_files.iter().any(|f| root.join(f).is_file()) {
        problems.push(format!("{name} has no {} any more, which its setup builds with.", build_files.join(" or ")));
    }
    if spec.build_tool == BuildTool::Maven && root.join("pom.xml").is_file() {
        match (war_packaging, spec.java_packaging) {
            (true, JavaPackaging::Jar) => problems.push(format!("{name}'s pom.xml now builds a .war, but its setup starts a jar.")),
            (false, JavaPackaging::War) => problems.push(format!("{name}'s pom.xml no longer builds a .war, but its setup deploys one on Tomcat.")),
            _ => {}
        }
    }
    problems
}

fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn extension(path: &str) -> &str {
    let name = base_name(path);
    name.rsplit_once('.').map_or("", |(_, e)| e)
}

/// Files whose change can change how the project builds or starts.
fn is_build_file(path: &str) -> bool {
    let name = base_name(path);
    let in_dir = |d: &str| path.starts_with(&format!("{d}/")) || path.contains(&format!("/{d}/"));
    matches!(name, "pom.xml" | "package.json" | "requirements.txt" | "pyproject.toml" | "go.mod" | "Dockerfile" | "docker-compose.yml")
        || name.starts_with("build.gradle")
        || matches!(extension(path), "properties" | "yml" | "yaml")
        || (extension(path) == "xml" && (in_dir("resources") || in_dir("WEB-INF")))
}

fn needs_test_run(changed: &[String]) -> bool {
    changed.iter().any(|f| is_build_file(f))
}

/// Files scanned for host paths and secrets.
fn is_config_file(path: &str) -> bool {
    base_name(path).starts_with(".env") || matches!(extension(path), "properties" | "yml" | "yaml" | "xml" | "env")
}

fn norm(p: &str) -> String {
    p.trim_start_matches("file:").replace('\\', "/").trim_end_matches('/').to_lowercase()
}

/// Absolute paths outside the project that `new` has and `old` didn't, and
/// that aren't already a file chosen to travel with the project. Uses Agent
/// D's detector, so it finds exactly what Setup finds.
fn new_host_paths(folder: &FolderPlanDto, file: &str, old: &str, new: &str) -> Vec<String> {
    let before: HashSet<(String, String)> =
        crate::externals::find_absolute_paths(file, old).into_iter().map(|h| (h.key, h.value)).collect();
    let project = norm(&folder.path);
    crate::externals::find_absolute_paths(file, new)
        .into_iter()
        .filter(|h| !before.contains(&(h.key.clone(), h.value.clone())))
        .filter(|h| {
            // The path itself, not the value as written (`C:\Users\...` in .properties).
            let v = norm(&crate::externals::hit_path(file, &h.value).unwrap_or_else(|| h.value.clone()));
            v != project && !v.starts_with(&format!("{project}/"))
        })
        .filter(|h| !folder.externals.iter().any(|e| e.value == h.value))
        .map(|h| format!("{file} line {} now points {} at {}, outside the project; the other device won't have it.", h.line, h.key, h.value))
        .collect()
}

const SECRET_WORDS: &[&str] = &["password", "passwd", "secret", "token", "apikey", "api_key", "api-key", "private_key", "privatekey", "access_key"];

/// `key = value` pairs on active lines whose key looks like it holds a secret.
fn secret_pairs(text: &str) -> HashSet<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!') && !l.starts_with("//") && !l.starts_with("<!--"))
        .filter_map(|l| {
            let i = l.find(['=', ':'])?;
            let (k, v) = (l[..i].trim().trim_start_matches("export ").trim(), l[i + 1..].trim().trim_matches(['"', '\'']));
            let lower = k.to_lowercase();
            let secret = SECRET_WORDS.iter().any(|w| lower.contains(w)) && !v.is_empty() && !v.starts_with("${");
            secret.then(|| (k.to_string(), v.to_string()))
        })
        .collect()
}

/// A note per secret-looking value `new` has and `old` didn't (never the value itself).
fn new_secrets(file: &str, old: &str, new: &str) -> Vec<String> {
    let before = secret_pairs(old);
    let mut keys: Vec<String> = secret_pairs(new).into_iter().filter(|p| !before.contains(p)).map(|(k, _)| k).collect();
    keys.sort();
    keys.into_iter().map(|k| format!("{file} now has a value for {k} that looks like a secret; it will be sent as is.")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::DumpPlanDto;
    use ls_composegen::{BuildTool, ComposeSpec, DbEnvPreset, JavaPackaging, Runtime};

    fn spec(runtime: Runtime, tool: BuildTool) -> ComposeSpec {
        ComposeSpec {
            runtime,
            runtime_version: if runtime == Runtime::Java { "21".into() } else { "3.12".into() },
            build_tool: tool,
            run_command: "python app.py".into(),
            port: 8080,
            artifact_path: (runtime == Runtime::Java).then(|| "target/*.jar".into()),
            java_packaging: JavaPackaging::Jar,
            tomcat_version: None,
            database: None,
            db_env_preset: DbEnvPreset::Standard,
            extras: vec![],
            env: vec![],
        }
    }

    fn folder(dir: &Path, compose: Option<ComposeSpec>) -> FolderPlanDto {
        FolderPlanDto { path: dir.display().to_string(), dump: None, compose, externals: vec![] }
    }

    #[test]
    fn a_valid_setup_has_no_problems_and_an_invalid_one_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = folder(dir.path(), Some(spec(Runtime::Python, BuildTool::Pip)));
        assert!(setup_problems(&f, "p", false).is_empty());
        f.compose.as_mut().unwrap().port = 80;
        let p = setup_problems(&f, "p", false);
        assert_eq!(p.len(), 1);
        assert!(p[0].starts_with("The setup for p is no longer valid") && p[0].contains("below 1024"), "{p:?}");
    }

    #[test]
    fn a_missing_dump_file_is_a_problem() {
        let dir = tempfile::tempdir().unwrap();
        let dump = dir.path().join("db.sql");
        let mut f = folder(dir.path(), Some(spec(Runtime::Python, BuildTool::Pip)));
        f.dump = Some(DumpPlanDto { schema: "db".into(), file_path: dump.display().to_string(), engine: "mysql".into() });
        assert_eq!(setup_problems(&f, "p", false), vec![format!("The database dump {} is missing.", dump.display())]);
        std::fs::write(&dump, "-- dump").unwrap();
        assert!(setup_problems(&f, "p", false).is_empty());
    }

    #[test]
    fn the_build_file_and_packaging_must_still_match_the_setup() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = folder(dir.path(), Some(spec(Runtime::Java, BuildTool::Maven)));
        assert_eq!(setup_problems(&f, "p", false), vec!["p has no pom.xml any more, which its setup builds with."]);
        std::fs::write(dir.path().join("pom.xml"), "<project/>").unwrap();
        assert!(setup_problems(&f, "p", false).is_empty());
        assert_eq!(setup_problems(&f, "p", true), vec!["p's pom.xml now builds a .war, but its setup starts a jar."]);
        let s = f.compose.as_mut().unwrap();
        s.java_packaging = JavaPackaging::War;
        s.artifact_path = Some("target/*.war".into());
        s.tomcat_version = Some("10.1".into());
        assert!(setup_problems(&f, "p", true).is_empty());
        assert_eq!(setup_problems(&f, "p", false), vec!["p's pom.xml no longer builds a .war, but its setup deploys one on Tomcat."]);
    }

    #[test]
    fn a_folder_with_its_own_compose_file_needs_it_to_stay() {
        let dir = tempfile::tempdir().unwrap();
        let f = folder(dir.path(), None);
        assert_eq!(setup_problems(&f, "p", false), vec!["p has no docker-compose.yml any more."]);
        std::fs::write(dir.path().join("docker-compose.yml"), "services: {}\n").unwrap();
        assert!(setup_problems(&f, "p", false).is_empty());
    }

    #[test]
    fn only_build_and_config_changes_trigger_a_test_run() {
        for f in [
            "pom.xml", "mod/pom.xml", "build.gradle", "build.gradle.kts", "package.json", "requirements.txt", "pyproject.toml", "go.mod",
            "Dockerfile", "docker-compose.yml", "src/main/resources/application.properties", "config.yml", "a/b.yaml",
            "src/main/resources/logback.xml", "src/main/webapp/WEB-INF/web.xml",
        ] {
            assert!(needs_test_run(&[f.to_string()]), "{f}");
        }
        for f in ["src/app.py", "README.md", "src/main/java/App.java", "docs/diagram.xml", "index.html"] {
            assert!(!needs_test_run(&[f.to_string()]), "{f}");
        }
        assert!(!needs_test_run(&[]));
    }

    /// DEPENDS ON AGENT D: passes only once `externals::find_absolute_paths`
    /// is implemented (the skeleton returns no hits). Documented behaviour:
    /// an active `key=C:/Users/x/y.xml` or `key=/home/u/f` line in a
    /// .properties file is a hit, a `#`-commented one is not.
    #[test]
    fn a_new_absolute_host_path_outside_the_project_is_a_problem() {
        let dir = tempfile::tempdir().unwrap();
        let f = folder(dir.path(), None);
        let file = "src/main/resources/application.properties";
        let old = "server.port=8080\nkeys.file=/home/u/old.jks\n";
        let new = format!(
            "server.port=8080\nkeys.file=/home/u/old.jks\nlog.config=C:/Users/x/y.xml\n#cert=/home/u/f\ndata.dir={}/data\n",
            dir.path().display().to_string().replace('\\', "/")
        );
        let found = new_host_paths(&f, file, old, &new);
        assert_eq!(found.len(), 1, "only the new, active, outside path: {found:?}");
        assert!(found[0].contains("line 3") && found[0].contains("log.config") && found[0].contains("C:/Users/x/y.xml"), "{found:?}");

        // Already chosen in Setup to travel with the project: not a problem.
        let mut chosen = f.clone();
        chosen.externals.push(crate::externals::ExternalRef {
            id: "e1".into(),
            key: "log.config".into(),
            config_file: file.into(),
            value: "C:/Users/x/y.xml".into(),
            path: "C:/Users/x/y.xml".into(),
            kind: crate::externals::ExternalKind::File,
            status: crate::externals::ExternalStatus::OutsideExists,
            size_bytes: 1,
            suggested_relative: None,
            line: 1,
            container_path: String::new(),
            rewritable: true,
        });
        assert!(new_host_paths(&chosen, file, old, &new).is_empty());
    }

    #[test]
    fn an_unchanged_or_commented_path_is_never_a_problem() {
        // Holds with or without Agent D's detector.
        let dir = tempfile::tempdir().unwrap();
        let f = folder(dir.path(), None);
        let text = "keys.file=/home/u/old.jks\n#cert=/home/u/f\n";
        assert!(new_host_paths(&f, "a.properties", text, text).is_empty());
        assert!(new_host_paths(&f, "a.properties", "", "#cert=/home/u/f\n").is_empty());
    }

    #[test]
    fn a_new_secret_looking_value_is_a_note_naming_the_key_not_the_value() {
        let old = "db.user=app\ndb.password=${DB_PASSWORD}\n";
        let new = "db.user=app\ndb.password=hunter2\n# api_key=commented\nAPI_TOKEN: \"abc\"\nempty.secret=\n";
        let notes = new_secrets("app.properties", old, new);
        assert_eq!(
            notes,
            vec![
                "app.properties now has a value for API_TOKEN that looks like a secret; it will be sent as is.",
                "app.properties now has a value for db.password that looks like a secret; it will be sent as is.",
            ]
        );
        assert!(notes.iter().all(|n| !n.contains("hunter2")));
        assert!(new_secrets("app.properties", new, new).is_empty(), "already there last time");
    }

    #[test]
    fn a_pass_is_tied_to_the_tree_and_the_settings() {
        let dir = tempfile::tempdir().unwrap();
        let f = folder(dir.path(), Some(spec(Runtime::Python, BuildTool::Pip)));
        remember_pass(pass_key(&f, "tree-x"));
        assert!(passed(&pass_key(&f, "tree-x")));
        assert!(!passed(&pass_key(&f, "tree-y")), "a pass for tree X doesn't count for tree Y");
        let mut g = f.clone();
        g.compose.as_mut().unwrap().port = 9090;
        assert!(!passed(&pass_key(&g, "tree-x")), "nor for other settings");
    }

    #[test]
    fn editing_an_external_file_makes_a_pass_stale() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("tenants.xml");
        std::fs::write(&outside, "<companies/>").unwrap();
        let mut f = folder(&dir.path().join("proj"), Some(spec(Runtime::Python, BuildTool::Pip)));
        f.externals.push(crate::externals::ExternalRef {
            id: "t1".into(),
            key: "tenants.file".into(),
            config_file: "app.properties".into(),
            value: outside.display().to_string(),
            path: outside.display().to_string(),
            kind: crate::externals::ExternalKind::File,
            status: crate::externals::ExternalStatus::OutsideExists,
            size_bytes: 12,
            suggested_relative: None,
            line: 1,
            container_path: String::new(),
            rewritable: true,
        });
        remember_pass(pass_key(&f, "tree-x"));
        std::fs::write(&outside, "<companies><company/></companies>").unwrap();
        assert!(!passed(&pass_key(&f, "tree-x")), "same tree, but the external file changed");
    }

    #[test]
    fn an_escaped_windows_path_inside_the_project_is_not_a_problem() {
        let f = FolderPlanDto { path: "C:/Users/Some One/proj".into(), dump: None, compose: None, externals: vec![] };
        let new = "data.dir=C:\\\\Users\\\\Some One\\\\proj\\\\data\nlogs=C:\\\\Users\\\\Some One\\\\logs\n";
        let found = new_host_paths(&f, "app.properties", "", new);
        assert_eq!(found.len(), 1, "only the path outside the project: {found:?}");
        assert!(found[0].contains("logs"), "{found:?}");
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} failed");
    }

    #[test]
    fn changed_files_are_relative_to_the_folder_and_include_uncommitted_edits() {
        let repo = tempfile::tempdir().unwrap();
        let sub = repo.path().join("app");
        std::fs::create_dir_all(sub.join("src")).unwrap();
        std::fs::write(sub.join("src/a.py"), "1").unwrap();
        std::fs::write(sub.join("requirements.txt"), "flask\n").unwrap();
        std::fs::write(repo.path().join("outside.txt"), "x").unwrap();
        git(repo.path(), &["init", "-q"]);
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "c"]);
        let base = ls_snapshot::head_commit(&sub).unwrap();
        std::fs::write(sub.join("requirements.txt"), "flask\nrequests\n").unwrap(); // uncommitted
        std::fs::write(repo.path().join("outside.txt"), "y").unwrap();
        let st = folder_state(&sub, Some(base.clone()), &Default::default()).unwrap();
        assert_eq!(st.changed, vec!["requirements.txt"]);
        assert_eq!(file_at(&sub, &st.current, "requirements.txt").as_deref(), Some("flask\nrequests\n"));
        assert_eq!(file_at(&sub, &base, "requirements.txt").as_deref(), Some("flask\n"));
        assert!(folder_state(&sub, None, &Default::default()).unwrap().changed.is_empty(), "no device yet: nothing to compare");
        let gone = folder_state(&sub, Some("0".repeat(40)), &Default::default()).unwrap();
        assert!(gone.changed.contains(&"src/a.py".to_string()), "a missing base counts everything: {:?}", gone.changed);
    }
}
