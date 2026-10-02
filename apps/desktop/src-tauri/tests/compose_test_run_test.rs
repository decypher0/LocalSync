//! End-to-end proof of the compose wizard's test run (`compose_wizard::
//! test_run_compose`): a project with no compose file plus a `ComposeSpec` is
//! really booted in Podman, the way the receiver will boot it, before
//! anything is sent - and when it can't boot, the report carries the real
//! reason from the container, not just "exit status 1".
//!
//! These need a real Podman + podman-compose (and network for base images and
//! package registries), so they skip with a printed reason where those are
//! absent. Native Windows can build but not run this crate's test binaries;
//! run them in WSL/Linux.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ls_composegen::{BuildTool, ComposeSpec, DbEnvPreset, JavaPackaging, Runtime};
use localsync_desktop::commands::{self, FolderPlanDto};
use localsync_desktop::compose_wizard::{self, TestRunReport};
use localsync_desktop::state::AppState;
use tauri::{Listener, Manager};

/// `Some(guard)` when Podman is available; the guard serializes the
/// real-container tests in this file. They all stream into the one shared
/// provisioning log, and a test run's report is that log's tail - so tests
/// running at the same time end up with each other's output in their reports.
fn podman_stack() -> Option<std::sync::MutexGuard<'static, ()>> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let ok = |bin: &str| Command::new(bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    if !(ok("podman") && ok("podman-compose")) {
        eprintln!("podman/podman-compose not on PATH - skipping (this test boots real containers)");
        return None;
    }
    // A test that panicked while holding it must not fail the rest.
    Some(SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

/// A committed git repo named `name` (its folder name is the project name)
/// containing `files`, and no compose file.
fn make_project(name: &str, files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join(name);
    std::fs::create_dir(&dir).unwrap();
    for (path, contents) in files {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, contents).unwrap();
    }
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "initial"]);
    (base, dir)
}

fn spec(runtime: Runtime, version: &str, tool: BuildTool, run_command: &str, port: u16) -> ComposeSpec {
    ComposeSpec {
        runtime,
        runtime_version: version.into(),
        build_tool: tool,
        run_command: run_command.into(),
        port,
        artifact_path: None,
        database: None,
        db_env_preset: DbEnvPreset::Standard,
        extras: vec![],
        env: vec![],
        java_packaging: Default::default(),
        tomcat_version: None,
    }
}

fn plan(dir: &Path, spec: ComposeSpec) -> FolderPlanDto {
    FolderPlanDto { path: dir.display().to_string(), dump: None, compose: Some(spec) }
}

async fn test_run(folder: FolderPlanDto) -> (TestRunReport, Vec<String>) {
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    let handle = app.handle().clone();
    let progress = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = progress.clone();
    handle.listen("compose-test-progress", move |event| {
        sink.lock().unwrap().push(event.payload().to_string());
    });
    let report = compose_wizard::test_run_compose(handle.clone(), handle.state::<AppState>(), folder)
        .await
        .expect("the request itself is fine");
    let lines = progress.lock().unwrap().clone();
    (report, lines)
}

/// Names of every container (any state) whose name contains `needle`.
fn containers_matching(needle: &str) -> Vec<String> {
    let out = Command::new("podman").args(["ps", "-a", "--format", "{{.Names}}"]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).lines().filter(|n| n.contains(needle)).map(String::from).collect()
}

fn assert_nothing_left_running(project: &str) {
    let left = containers_matching(project);
    assert!(left.is_empty(), "containers left behind for {project}: {left:?}");
}

fn http_status_line(port: u16) -> Option<String> {
    let mut s = TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse().unwrap(), Duration::from_secs(1)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    s.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n").ok()?;
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    buf.lines().next().map(String::from)
}

async fn wait_for_http_200(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if http_status_line(port).is_some_and(|l| l.contains("200")) {
            return;
        }
        assert!(Instant::now() < deadline, "port {port} never answered HTTP 200");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// What the receiver gets: the generated files at the spec's own port (no
/// test-run override), packaged and verified like a received snapshot.
fn snapshot_as_sent(dir: &Path, spec: &ComposeSpec) -> ls_security::VerifiedSnapshot {
    let folders = vec![ls_snapshot::FolderSpec { path: dir.to_path_buf(), parent_commit: None }];
    let label = ls_snapshot::folder_labels(&folders).remove(0);
    let ctx = ls_composegen::GenerateContext { folder_label: label, dump: None, host_port: None };
    let generated = ls_composegen::generate(spec, &ctx).expect("generate");
    let files = ls_snapshot::GeneratedFiles {
        compose_yaml: generated.compose_yaml,
        files: generated.files.into_iter().map(|g| (g.path, g.contents.into_bytes())).collect(),
    };
    let snapshot = ls_snapshot::create_snapshot_multi_with(&folders, &[], &[Some(files)]).expect("snapshot");
    ls_security::verify(snapshot, &[]).expect("verify")
}

#[tokio::test]
async fn python_project_boots_and_the_same_snapshot_runs_like_a_receiver_would() {
    let Some(_serial) = podman_stack() else {
        return;
    };
    let (_base, dir) = make_project("tr-py-ok", &[("index.html", "<h1>hello from the test run</h1>")]);
    let spec = spec(Runtime::Python, "3.12", BuildTool::Pip, "python -m http.server 18101", 18101);

    let (report, progress) = test_run(plan(&dir, spec.clone())).await;
    assert!(report.ok, "test run failed: {:?}\n{}", report.error, report.output_tail);
    assert_eq!(report.host_port, 18101);
    assert!(report.notes.is_empty(), "{:?}", report.notes);
    assert!(report.output_tail.contains("Serving HTTP"), "the app's own output should be reported: {}", report.output_tail);
    assert!(progress.iter().any(|l| l.contains("Waiting for the app")), "no live progress events: {progress:?}");
    assert_nothing_left_running("tr-py-ok");
    assert!(http_status_line(18101).is_none(), "the test run must not leave the app serving");

    // Now what the receiver does with what is sent: the very same generated
    // files, through the real Run command.
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    let handle = app.handle().clone();
    let verified = snapshot_as_sent(&dir, &spec);
    let id = format!("{}@{}", verified.snapshot().manifest.project_name, verified.snapshot().manifest.git_commit);
    handle.state::<AppState>().verified.lock().unwrap().insert(id.clone(), verified);
    let work = tempfile::tempdir().unwrap();
    let session = commands::run_snapshot(handle.clone(), handle.state::<AppState>(), id, work.path().display().to_string())
        .await
        .expect("the receiver's Run of the same snapshot should work");
    assert!(session.service_ports.iter().any(|(svc, p)| svc == "app" && p.starts_with("18101:")), "{:?}", session.service_ports);
    wait_for_http_200(18101).await;
    commands::stop_session(handle.state::<AppState>(), session.session_id).await.unwrap();
    assert_nothing_left_running("tr-py-ok");
}

/// The project ships its own `Dockerfile` (but no docker-compose.yml, which
/// is what triggers the wizard). The compose file must name the generated
/// Dockerfile explicitly, or Podman would silently build the project's own
/// file - here one whose container fails on purpose, so building it can't pass.
#[tokio::test]
async fn a_projects_own_dockerfile_is_never_built_instead_of_the_generated_one() {
    let Some(_serial) = podman_stack() else {
        return;
    };
    let stale = "FROM docker.io/library/busybox
CMD [\"sh\", \"-c\", \"echo STALE_DOCKERFILE_WAS_BUILT; exit 1\"]
";
    let (_base, dir) = make_project(
        "tr-own-dockerfile",
        &[("Dockerfile", stale), ("index.html", "<h1>generated one</h1>")],
    );
    let spec = spec(Runtime::Python, "3.12", BuildTool::Pip, "python -m http.server 18106", 18106);

    let (report, _) = test_run(plan(&dir, spec.clone())).await;
    assert!(report.ok, "test run failed: {:?}
{}", report.error, report.output_tail);
    assert!(!report.output_tail.contains("STALE_DOCKERFILE_WAS_BUILT"), "{}", report.output_tail);
    assert!(report.output_tail.contains("Serving HTTP"), "{}", report.output_tail);
    assert_nothing_left_running("tr-own-dockerfile");

    // And what is sent carries the generated Dockerfile plus the project's own one, renamed.
    let verified = snapshot_as_sent(&dir, &spec);
    let payload = &verified.snapshot().payload;
    let decoded = zstd::stream::decode_all(&payload[..]).unwrap();
    let mut names = Vec::new();
    for entry in tar::Archive::new(&decoded[..]).entries().unwrap() {
        names.push(entry.unwrap().path().unwrap().display().to_string());
    }
    assert!(names.iter().any(|n| n.ends_with("source/Dockerfile")), "{names:?}");
    assert!(names.iter().any(|n| n.ends_with("source/Dockerfile.original")), "{names:?}");
}

#[tokio::test]
async fn node_project_boots() {
    let Some(_serial) = podman_stack() else {
        return;
    };
    let (_base, dir) = make_project(
        "tr-node-ok",
        &[
            ("package.json", r#"{"name":"tr-node-ok","version":"1.0.0","scripts":{"start":"node server.js"}}"#),
            (
                "server.js",
                "require('http').createServer((q, r) => r.end('ok')).listen(18103, '0.0.0.0', () => console.log('node listening'));",
            ),
        ],
    );
    let (report, _) = test_run(plan(&dir, spec(Runtime::Node, "20", BuildTool::Npm, "node server.js", 18103))).await;
    assert!(report.ok, "test run failed: {:?}\n{}", report.error, report.output_tail);
    assert!(report.output_tail.contains("node listening"), "{}", report.output_tail);
    assert_nothing_left_running("tr-node-ok");
}

#[tokio::test]
async fn a_crashing_app_reports_the_containers_own_error_not_just_an_exit_status() {
    let Some(_serial) = podman_stack() else {
        return;
    };
    let (_base, dir) = make_project("tr-py-crash", &[("readme.txt", "no server here")]);
    let (report, _) = test_run(plan(&dir, spec(Runtime::Python, "3.12", BuildTool::Pip, "python nosuchfile.py", 18104))).await;
    assert!(!report.ok);
    let error = report.error.clone().expect("a failed run has an error");
    let all = format!("{error}\n{}", report.output_tail);
    eprintln!("--- crash report ---
{all}");
    assert!(all.contains("can't open file") && all.contains("nosuchfile.py"), "the real reason is missing: {all}");
    assert!(all.contains("No such file"), "{all}");
    assert_nothing_left_running("tr-py-crash");
}

#[tokio::test]
async fn a_failed_build_reports_the_real_build_error() {
    let Some(_serial) = podman_stack() else {
        return;
    };
    let (_base, dir) = make_project(
        "tr-node-build-fails",
        &[
            ("package.json", r#"{"name":"x","version":"1.0.0","dependencies":{"localsync-no-such-package-xyz":"1.0.0"}}"#),
            ("server.js", "console.log('never gets here')"),
        ],
    );
    let (report, _) = test_run(plan(&dir, spec(Runtime::Node, "20", BuildTool::Npm, "node server.js", 18105))).await;
    assert!(!report.ok);
    let all = format!("{}\n{}", report.error.clone().unwrap_or_default(), report.output_tail);
    eprintln!("--- build failure report ---
{all}");
    assert!(all.contains("localsync-no-such-package-xyz"), "the npm error naming the package is missing: {all}");
    assert!(all.contains("404") || all.contains("E404") || all.contains("not found"), "npm's real reason is missing: {all}");
    assert_nothing_left_running("tr-node-build-fails");
}

/// A multi-module Maven project: the parent pom has `<modules>`, and the jar
/// lands in the module's own `app/target/`, not the root's `target/`.
fn multi_module_maven(name: &str, port: u16) -> (tempfile::TempDir, PathBuf) {
    let parent_pom = r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>t</groupId><artifactId>parent</artifactId><version>1.0</version><packaging>pom</packaging>
  <properties><maven.compiler.release>21</maven.compiler.release></properties>
  <modules><module>app</module></modules>
</project>"#;
    let app_pom = r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <parent><groupId>t</groupId><artifactId>parent</artifactId><version>1.0</version></parent>
  <artifactId>app</artifactId>
  <build><plugins><plugin>
    <artifactId>maven-jar-plugin</artifactId>
    <configuration><archive><manifest><mainClass>App</mainClass></manifest></archive></configuration>
  </plugin></plugins></build>
</project>"#;
    let app = format!(
        r#"import com.sun.net.httpserver.HttpServer;
import java.net.InetSocketAddress;
public class App {{
  public static void main(String[] a) throws Exception {{
    HttpServer s = HttpServer.create(new InetSocketAddress({port}), 0);
    s.createContext("/", x -> {{ byte[] b = "ok".getBytes(); x.sendResponseHeaders(200, b.length); x.getResponseBody().write(b); x.close(); }});
    s.start();
    System.out.println("listening on {port}");
  }}
}}"#
    );
    make_project(name, &[("pom.xml", parent_pom), ("app/pom.xml", app_pom), ("app/src/main/java/App.java", &app)])
}

/// The configured build-output path points at the root `target/`, but the jar
/// is in `app/target/`: the report must show where it really is, not just
/// the copier's "no such file".
#[tokio::test]
async fn a_wrong_build_output_path_lists_where_the_jar_really_landed() {
    let Some(_serial) = podman_stack() else {
        return;
    };
    let (_base, dir) = multi_module_maven("tr-maven-wrong-path", 18109);
    let mut s = spec(Runtime::Java, "21", BuildTool::Maven, "java -jar /app/app.jar", 18109);
    s.artifact_path = Some("target/*.jar".into());
    let (report, _) = test_run(plan(&dir, s)).await;
    let all = format!("{}
{}", report.error.clone().unwrap_or_default(), report.output_tail);
    eprintln!("--- wrong artifact path report ---
{all}");
    assert!(!report.ok);
    assert!(all.contains("no file matches the build-output path target/*.jar"), "{all}");
    assert!(all.contains("(target/ does not exist)"), "{all}");
    assert!(all.contains("./app/target/app-1.0.jar"), "the real jar's location must be listed: {all}");
    assert_nothing_left_running("tr-maven-wrong-path");
}

/// Same project with the path pointed at the module: it builds and serves.
#[tokio::test]
async fn a_multi_module_maven_project_runs_with_the_modules_build_output_path() {
    let Some(_serial) = podman_stack() else {
        return;
    };
    let (_base, dir) = multi_module_maven("tr-maven-module-path", 18110);
    let mut s = spec(Runtime::Java, "21", BuildTool::Maven, "java -jar /app/app.jar", 18110);
    s.artifact_path = Some("app/target/*.jar".into());
    let (report, _) = test_run(plan(&dir, s)).await;
    assert!(report.ok, "test run failed: {:?}
{}", report.error, report.output_tail);
    assert_nothing_left_running("tr-maven-module-path");
}

fn http_body(port: u16) -> Option<String> {
    let mut s = TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse().unwrap(), Duration::from_secs(1)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    s.write_all(b"GET / HTTP/1.0
Host: localhost

").ok()?;
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    Some(buf)
}

/// A WAR-packaged Maven project (`<packaging>war</packaging>`) with a JSP
/// that Tomcat has to compile at run time - which only works if the
/// read-only-rootfs workaround (CATALINA_BASE under /tmp) does.
fn war_project(name: &str) -> (tempfile::TempDir, PathBuf) {
    let pom = r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>t</groupId><artifactId>webapp</artifactId><version>1.0</version>
  <packaging>war</packaging>
  <build><plugins><plugin>
    <artifactId>maven-war-plugin</artifactId><version>3.4.0</version>
  </plugin></plugins></build>
</project>"#;
    let jsp = r#"<html><body>jsp-ok-<%= 6 * 7 %></body></html>"#;
    let web_xml = r#"<web-app xmlns="http://xmlns.jcp.org/xml/ns/javaee" version="3.1"/>"#;
    make_project(name, &[("pom.xml", pom), ("src/main/webapp/index.jsp", jsp), ("src/main/webapp/WEB-INF/web.xml", web_xml)])
}

async fn war_boots_on_tomcat(name: &str, java: &str, tomcat: &str, port: u16) {
    let Some(_serial) = podman_stack() else {
        return;
    };
    let (_base, dir) = war_project(name);
    let mut s = spec(Runtime::Java, java, BuildTool::Maven, "localsync-tomcat", port);
    s.artifact_path = Some("target/*.war".into());
    s.java_packaging = JavaPackaging::War;
    s.tomcat_version = Some(tomcat.into());

    let (report, _) = test_run(plan(&dir, s.clone())).await;
    assert!(report.ok, "test run failed: {:?}
{}", report.error, report.output_tail);
    assert_nothing_left_running(name);

    // What the receiver does with it: the JSP is compiled and served at /.
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    let handle = app.handle().clone();
    let verified = snapshot_as_sent(&dir, &s);
    let id = format!("{}@{}", verified.snapshot().manifest.project_name, verified.snapshot().manifest.git_commit);
    handle.state::<AppState>().verified.lock().unwrap().insert(id.clone(), verified);
    let work = tempfile::tempdir().unwrap();
    let session = commands::run_snapshot(handle.clone(), handle.state::<AppState>(), id, work.path().display().to_string())
        .await
        .expect("the receiver's Run of the WAR should work");
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut body = String::new();
    while Instant::now() < deadline {
        body = http_body(port).unwrap_or_default();
        if body.contains("jsp-ok-42") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    commands::stop_session(handle.state::<AppState>(), session.session_id).await.unwrap();
    assert!(body.contains("jsp-ok-42"), "Tomcat never served the compiled JSP at /: {body}");
    assert_nothing_left_running(name);
}

#[tokio::test]
async fn a_war_project_builds_and_serves_on_tomcat_10_with_java_17() {
    war_boots_on_tomcat("tr-war-tomcat10", "17", "10.1", 18111).await;
}

#[tokio::test]
async fn a_war_project_builds_and_serves_on_tomcat_9_with_java_8() {
    war_boots_on_tomcat("tr-war-tomcat9", "8", "9.0", 18112).await;
}

#[tokio::test]
async fn a_busy_port_does_not_fail_the_test_run_and_is_explained() {
    let Some(_serial) = podman_stack() else {
        return;
    };
    // The sender's own dev server, already on the app's port.
    let dev_server = TcpListener::bind("0.0.0.0:18106").expect("port 18106 should be free for this test");
    let (_base, dir) = make_project("tr-py-busy", &[("index.html", "hi")]);
    let (report, _) = test_run(plan(&dir, spec(Runtime::Python, "3.12", BuildTool::Pip, "python -m http.server 18106", 18106))).await;
    assert!(report.ok, "test run failed: {:?}\n{}", report.error, report.output_tail);
    assert_ne!(report.host_port, 18106);
    assert!(report.notes.iter().any(|n| n.contains("18106") && n.contains(&report.host_port.to_string())), "{:?}", report.notes);
    drop(dev_server);
    assert_nothing_left_running("tr-py-busy");
}

#[tokio::test]
async fn an_app_that_never_listens_times_out_with_a_useful_message() {
    let Some(_serial) = podman_stack() else {
        return;
    };
    let (_base, dir) = make_project("tr-py-silent", &[("readme.txt", "x")]);
    // Runs fine but never opens its port.
    let folder = plan(&dir, spec(Runtime::Python, "3.12", BuildTool::Pip, "python -c \"import time; print('idle'); time.sleep(600)\"", 18107));
    let app = tauri::test::mock_app();
    let report = compose_wizard::run_compose_test(app.handle().clone(), folder, Duration::from_secs(8)).await.unwrap();
    assert!(!report.ok);
    let error = report.error.unwrap_or_default();
    eprintln!("--- timeout report ---
{error}");
    assert!(error.contains("nothing accepted connections on port") && error.contains("0.0.0.0"), "{error}");
    assert!(error.contains("idle"), "the app's own output should be included: {error}");
    assert_nothing_left_running("tr-py-silent");
}

#[tokio::test]
async fn a_request_without_compose_settings_is_an_error_not_a_failed_boot() {
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    let handle = app.handle().clone();
    let folder = FolderPlanDto { path: "/nowhere".into(), dump: None, compose: None };
    assert!(compose_wizard::test_run_compose(handle.clone(), handle.state::<AppState>(), folder).await.is_err());

    // An invalid spec (privileged port) is rejected before anything runs.
    let bad = plan(Path::new("/nowhere"), spec(Runtime::Python, "3.12", BuildTool::Pip, "python app.py", 80));
    let err = compose_wizard::test_run_compose(handle.clone(), handle.state::<AppState>(), bad).await.unwrap_err();
    assert!(err.contains("port"), "{err}");
}

/// The FK-order dump LocalSync's MySQL exporter produced before the
/// `SET FOREIGN_KEY_CHECKS` fix: `children` references `parents`, which is
/// created later, so a real MySQL 8 import fails with ERROR 1824. With
/// `fixed`, the same dump wrapped the way the exporter now writes it.
fn fk_order_dump(fixed: bool) -> String {
    let body = "-- Table: children\n\
        DROP TABLE IF EXISTS `children`;\n\
        CREATE TABLE `children` (`id` int NOT NULL, `parent_id` int NOT NULL, PRIMARY KEY (`id`), KEY `fk` (`parent_id`), \
        CONSTRAINT `fk` FOREIGN KEY (`parent_id`) REFERENCES `parents` (`id`)) ENGINE=InnoDB;\n\
        INSERT INTO `children` (`id`,`parent_id`) VALUES (10,1);\n\
        -- Table: parents\n\
        DROP TABLE IF EXISTS `parents`;\n\
        CREATE TABLE `parents` (`id` int NOT NULL, PRIMARY KEY (`id`)) ENGINE=InnoDB;\n\
        INSERT INTO `parents` (`id`) VALUES (1);\n";
    if fixed {
        format!("SET FOREIGN_KEY_CHECKS=0;\n{body}SET FOREIGN_KEY_CHECKS=1;\n")
    } else {
        body.to_string()
    }
}

/// A Python app that never touches the database (so it serves either way)
/// plus MySQL 8.0 seeded with `dump` - the app answering must not count as
/// success unless the database really started and imported its data.
async fn test_run_with_mysql_dump(name: &str, port: u16, dump: &str) -> TestRunReport {
    let (base, dir) = make_project(name, &[("index.html", "<h1>app</h1>")]);
    let dump_path = base.path().join("appdb.sql");
    std::fs::write(&dump_path, dump).unwrap();
    let mut s = spec(Runtime::Python, "3.12", BuildTool::Pip, &format!("python -m http.server {port}"), port);
    s.database = Some(ls_composegen::DatabaseSpec { engine: ls_composegen::DbEngine::Mysql, version: "8.0".into(), database: "appdb".into() });
    let mut folder = plan(&dir, s);
    folder.dump = Some(commands::DumpPlanDto {
        schema: "appdb".into(),
        file_path: dump_path.display().to_string(),
        engine: "mysql".into(),
    });
    let (report, _) = test_run(folder).await;
    report
}

#[tokio::test]
async fn a_database_dump_that_fails_to_import_fails_the_test_run_with_the_real_mysql_error() {
    let Some(_serial) = podman_stack() else {
        return;
    };
    let report = test_run_with_mysql_dump("tr-db-import-fails", 18121, &fk_order_dump(false)).await;
    let all = format!("{}\n{}", report.error.clone().unwrap_or_default(), report.output_tail);
    eprintln!("--- failed import report ---\n{all}");
    assert!(!report.ok, "a database that crashed importing its dump must not pass: {all}");
    assert!(all.contains("The database (`mysql`) stopped"), "the failure is attributed to the database: {all}");
    assert!(all.contains("Failed to open the referenced table"), "MySQL's real error is surfaced: {all}");
    assert_nothing_left_running("tr-db-import-fails");
}

#[tokio::test]
async fn a_database_dump_that_imports_passes_the_test_run() {
    let Some(_serial) = podman_stack() else {
        return;
    };
    let report = test_run_with_mysql_dump("tr-db-import-ok", 18122, &fk_order_dump(true)).await;
    assert!(report.ok, "test run failed: {:?}\n{}", report.error, report.output_tail);
    assert_nothing_left_running("tr-db-import-ok");
}
