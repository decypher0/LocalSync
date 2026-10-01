//! Regression test for a real Windows bug: a project shaped like `xusom-admin`
//! (its own `Dockerfile` in the repo, no docker-compose.yml) sent through the
//! compose wizard must build the wizard's GENERATED Dockerfile - never the
//! project's own one.
//!
//! The generated compose names its Dockerfile with `dockerfile:`, but
//! podman-compose 1.6.0 on Windows silently ignores that key: it hands
//! `podman build` an absolute `C:\...` context, its `is_context_git_url`
//! mistakes that for a URL, and it skips its local Dockerfile lookup - so no
//! `-f` is ever passed and podman builds whatever is literally named
//! `Dockerfile`/`Containerfile` in the context. So the fix cannot rely on the
//! key: the generated file IS the default-named one, and the project's own
//! is renamed out of the way. This test runs the real installed Podman stack
//! and fails on any podman-compose that ends up building the wrong file.
//!
//! Needs a real Podman + podman-compose and network (base images); skips with
//! a printed reason where they are absent.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ls_composegen::{BuildTool, ComposeSpec, DbEnvPreset, GenerateContext, Runtime};

const PORT: u16 = 18107;

fn stack_available() -> bool {
    let ok = |bin: &str| Command::new(bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    let available = ok("podman") && ok("podman-compose");
    if !available {
        eprintln!("podman/podman-compose not on PATH - skipping (this test builds real images)");
    }
    available
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// An `xusom-admin`-shaped project: its own Dockerfile (a base image that no
/// longer exists, exactly like the real one), a build file, some source, and
/// no compose file. Also has a `Containerfile`, which podman prefers over
/// `Dockerfile`, to cover that precedence too.
fn project(base: &Path) -> PathBuf {
    let dir = base.join("xusom-admin");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("Dockerfile"), "FROM docker.io/library/openjdk:8-jdk\nRUN echo STALE_OWN_DOCKERFILE\n").unwrap();
    std::fs::write(dir.join("Containerfile"), "FROM docker.io/library/busybox\nRUN echo STALE_OWN_CONTAINERFILE\n").unwrap();
    std::fs::write(dir.join("pom.xml"), "<project/>").unwrap();
    std::fs::write(dir.join("index.html"), "<h1>xusom-admin</h1>").unwrap();
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "initial"]);
    dir
}

fn spec() -> ComposeSpec {
    ComposeSpec {
        runtime: Runtime::Python,
        runtime_version: "3.12".into(),
        build_tool: BuildTool::Pip,
        run_command: format!("python -m http.server {PORT}"),
        port: PORT,
        artifact_path: None,
        database: None,
        db_env_preset: DbEnvPreset::Standard,
        extras: vec![],
        env: vec![],
        java_packaging: Default::default(),
        tomcat_version: None,
    }
}

fn http_200() -> bool {
    http_200_on(PORT)
}

fn http_200_on(port: u16) -> bool {
    let Ok(mut s) = TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse().unwrap(), Duration::from_secs(1)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    if s.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n").is_err() {
        return false;
    }
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    buf.lines().next().is_some_and(|l| l.contains("200"))
}

fn find_dir(root: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == name) {
                return Some(p);
            }
            if let Some(found) = find_dir(&p, name) {
                return Some(found);
            }
        }
    }
    None
}

#[tokio::test]
async fn a_projects_own_dockerfile_is_never_built_instead_of_the_generated_one() {
    if !stack_available() {
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let dir = project(base.path());
    let spec = spec();

    let folders = vec![ls_snapshot::FolderSpec { path: dir.clone(), parent_commit: None }];
    let label = ls_snapshot::folder_labels(&folders).remove(0);
    let generated =
        ls_composegen::generate(&spec, &GenerateContext { folder_label: label, dump: None, host_port: None }).unwrap();
    let files = ls_snapshot::GeneratedFiles {
        compose_yaml: generated.compose_yaml,
        files: generated.files.into_iter().map(|g| (g.path, g.contents.into_bytes())).collect(),
    };
    let snapshot = ls_snapshot::create_snapshot_multi_with(&folders, &[], &[Some(files)]).unwrap();
    let verified = ls_security::verify(snapshot, &[]).unwrap();

    let work = tempfile::tempdir().unwrap();
    let session = ls_containers::run_snapshot(&verified, work.path()).await.expect("run_snapshot");

    // What the receiver's build context looks like: exactly one default-named
    // build file, and it is the generated one; the project's own are kept
    // under other names.
    let source = find_dir(work.path(), "source").expect("unpacked source dir");
    let dockerfile = std::fs::read_to_string(source.join("Dockerfile")).unwrap_or_default();
    let layout_ok = dockerfile.contains("python:3.12") && !dockerfile.contains("openjdk");
    let own_kept = std::fs::read_to_string(source.join("Dockerfile.original")).map(|s| s.contains("openjdk:8-jdk"));
    let no_containerfile = !source.join("Containerfile").exists();

    // The real proof: the image that actually got built serves the app. If
    // the project's own Dockerfile (or Containerfile) were built instead, its
    // base image can't even be pulled, or it just runs `echo` and exits.
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut served = false;
    while Instant::now() < deadline {
        if http_200() {
            served = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let missing = ls_containers::services_not_started(&session).await;
    ls_containers::stop_session(&session).await.unwrap();

    assert!(layout_ok, "the context's Dockerfile is not the generated one: {dockerfile}");
    assert_eq!(own_kept.ok(), Some(true), "the project's own Dockerfile should be kept as Dockerfile.original");
    assert!(no_containerfile, "the project's own Containerfile must not stay where a build tool picks it up");
    assert!(
        served,
        "the app never answered HTTP 200 - the wrong Dockerfile was probably built (services not started: {missing:?})"
    );
}

/// The real project is a subfolder of a larger repo (`.git` at the parent,
/// `pom.xml` one level down, no `.git` of its own). Selecting the subfolder
/// must produce a snapshot that actually builds and serves - with only the
/// subfolder's files in the build context.
#[tokio::test]
async fn a_subfolder_of_a_larger_repo_builds_and_runs() {
    if !stack_available() {
        return;
    }
    const SUB_PORT: u16 = 18108;
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("monorepo");
    let dir = repo.join("xusom-admin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(repo.join("README.md"), "monorepo").unwrap();
    std::fs::write(dir.join("pom.xml"), "<project/>").unwrap();
    std::fs::write(dir.join("index.html"), "<h1>nested</h1>").unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "initial"]);

    let spec = ComposeSpec { run_command: format!("python -m http.server {SUB_PORT}"), port: SUB_PORT, ..spec() };
    let folders = vec![ls_snapshot::FolderSpec { path: dir.clone(), parent_commit: None }];
    let label = ls_snapshot::folder_labels(&folders).remove(0);
    assert_eq!(label, "xusom-admin");
    let generated =
        ls_composegen::generate(&spec, &GenerateContext { folder_label: label, dump: None, host_port: None }).unwrap();
    let files = ls_snapshot::GeneratedFiles {
        compose_yaml: generated.compose_yaml,
        files: generated.files.into_iter().map(|g| (g.path, g.contents.into_bytes())).collect(),
    };
    let snapshot = ls_snapshot::create_snapshot_multi_with(&folders, &[], &[Some(files)]).unwrap();
    let verified = ls_security::verify(snapshot, &[]).unwrap();

    let work = tempfile::tempdir().unwrap();
    let session = ls_containers::run_snapshot(&verified, work.path()).await.expect("run_snapshot");
    let source = find_dir(work.path(), "source").expect("unpacked source dir");
    let only_the_subfolder = source.join("pom.xml").is_file()
        && source.join("index.html").is_file()
        && !source.join("README.md").exists()
        && !source.join("xusom-admin").exists();

    let deadline = Instant::now() + Duration::from_secs(120);
    let mut served = false;
    while Instant::now() < deadline {
        if http_200_on(SUB_PORT) {
            served = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    ls_containers::stop_session(&session).await.unwrap();

    assert!(only_the_subfolder, "the build context must be exactly the selected subfolder's files");
    assert!(served, "the snapshot of a nested project never served HTTP 200");
}
