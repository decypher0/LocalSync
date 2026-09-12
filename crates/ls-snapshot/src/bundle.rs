use anyhow::{bail, Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

use crate::types::ServiceDef;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Same fix, same reasoning, as `crates/ls-containers/src/podman.rs`'s
/// identical constant: prevents `git` (a console-mode binary) from popping
/// its own visible console window when spawned from this GUI app on
/// Windows. `git_bytes` runs on every Send, including from a Windows
/// sender - round 12's audit found `podman-compose` wasn't the only
/// unsuppressed spawn point in this codebase.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg_attr(not(windows), allow(unused_mut))]
fn git_command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Every `git_bytes` call is a fast, local, read-only operation, even
/// against a large real repo (verified: `git archive --format=tar HEAD`
/// against this repo itself takes well under a second). 30s is generous
/// headroom for an unusually large repo while still failing fast, and
/// firmly finite — see `git_bytes`'s doc comment for what this guards
/// against.
const GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Git's well-known empty-tree object — diffing against it makes "no parent
/// commit" just a regular diff (base = empty tree), instead of a separate
/// code path. Every tracked file comes out as an add, which is exactly the
/// "no parent given" behavior the manifest contract asks for.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Defensive fallback for noise directories that made it into git history
/// anyway — the common accidental-commit case (no `.gitignore` was ever set
/// up, or one was added too late to retroactively untrack an
/// already-committed `node_modules/`). A project whose `.gitignore` is
/// respected needs none of this: git's own tracking already keeps
/// untracked/ignored paths out of `git archive`/`git diff` by construction
/// (an untracked directory was never in a commit to begin with). This list
/// only matters once a noise directory is genuinely *tracked*, in which case
/// git would otherwise faithfully include the whole tree. Reasonable,
/// common build/dependency-directory conventions — not an exhaustive list.
/// Extend freely; any path with one of these as a path component (at any
/// depth) is dropped from the payload and from the diff.
const NOISE_DIR_NAMES: &[&str] = &[
    "node_modules", // npm/yarn/pnpm
    ".git",         // nested/embedded git metadata
    "target",       // cargo/maven/gradle build output
    "build",        // generic build output (gradle, many JS tools, ...)
    "dist",         // generic bundled/distributable output
    "__pycache__",  // Python bytecode cache
    ".venv",        // Python virtualenv
    "venv",         // Python virtualenv (alt name)
    "vendor",       // PHP/Go vendored dependencies
    ".next",        // Next.js build output
    ".nuxt",        // Nuxt build output
];

/// True if any component of `path` is a known noise directory name (see
/// [`NOISE_DIR_NAMES`]) — i.e. the path lives inside one, at any depth.
fn has_noise_component(path: &Path) -> bool {
    path.components().any(|c| match c {
        std::path::Component::Normal(name) => {
            NOISE_DIR_NAMES.iter().any(|noise| name == std::ffi::OsStr::new(noise))
        }
        _ => false,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiffStatEntry {
    pub path: String,
    pub change_type: String,
    pub insertions: u64,
    pub deletions: u64,
}

pub struct GitBundle {
    pub payload: Vec<u8>,
    pub git_commit: String,
    pub services: Vec<ServiceDef>,
}

pub fn bundle_project(project_root: &Path, parent_commit: Option<&str>) -> Result<GitBundle> {
    log::info!("bundling started: project_root={}", project_root.display());
    let git_commit = git_text(project_root, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();

    let diff_stat = build_diff_stat(project_root, parent_commit)?;
    let diff_stat_json = serde_json::to_vec_pretty(&diff_stat)?;

    let diff_patch = match parent_commit {
        Some(parent) => filter_noise_from_patch(&git_text(project_root, &["diff", &format!("{parent}..HEAD")])?),
        None => String::new(),
    };

    let compose_path = project_root.join("docker-compose.yml");
    let compose_bytes = if compose_path.is_file() {
        Some(fs::read(&compose_path).with_context(|| format!("reading {}", compose_path.display()))?)
    } else {
        None
    };
    let services = match &compose_bytes {
        Some(bytes) => parse_compose_services(bytes)?,
        None => Vec::new(),
    };

    let db_seed_dir = resolve_db_seed_dir(project_root);

    let gz = GzEncoder::new(Vec::new(), Compression::default());
    let mut tb = tar::Builder::new(gz);

    let archive_bytes = git_bytes(project_root, &["archive", "--format=tar", "HEAD"])?;
    append_git_archive(&mut tb, &archive_bytes, "source")?;

    append_bytes(&mut tb, "diff_stat.json", &diff_stat_json)?;
    append_bytes(&mut tb, "diff.patch", diff_patch.as_bytes())?;
    if let Some(bytes) = &compose_bytes {
        append_bytes(&mut tb, "docker-compose.yml", bytes)?;
    }
    if let Some(dir) = &db_seed_dir {
        for file in walk_files(dir)? {
            let rel = file.strip_prefix(dir).unwrap();
            let tar_path = Path::new("db-seed").join(rel);
            let mut f = fs::File::open(&file)?;
            tb.append_file(&tar_path, &mut f)
                .with_context(|| format!("adding {} to payload", tar_path.display()))?;
        }
    }

    let gz = tb.into_inner().context("finalizing tar")?;
    let payload = gz.finish().context("finalizing gzip")?;

    log::info!(
        "bundling done: commit={} payload_bytes={}",
        git_commit,
        payload.len()
    );

    Ok(GitBundle {
        payload,
        git_commit,
        services,
    })
}

/// `db-seed/` at the project root, falling back to `db/seed/`. Shared by the
/// bundler (copies the files in) and hash.rs (hashes them).
pub(crate) fn resolve_db_seed_dir(project_root: &Path) -> Option<PathBuf> {
    let a = project_root.join("db-seed");
    if a.is_dir() {
        return Some(a);
    }
    let b = project_root.join("db").join("seed");
    if b.is_dir() {
        return Some(b);
    }
    None
}

/// Recursively lists regular files under `dir`, sorted for determinism.
pub(crate) fn walk_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).with_context(|| format!("reading {}", d.display()))? {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    Ok(out)
}

fn build_diff_stat(project_root: &Path, parent_commit: Option<&str>) -> Result<Vec<DiffStatEntry>> {
    let base = parent_commit.unwrap_or(EMPTY_TREE);

    // --no-renames: diff_stat's change_type is only added/modified/deleted
    // (no "renamed" variant), so a rename is simplest as a delete+add pair
    // rather than teaching the parser `old => new` path syntax.
    let numstat = git_text(project_root, &["diff", "--no-renames", "--numstat", base, "HEAD"])?;
    let name_status = git_text(project_root, &["diff", "--no-renames", "--name-status", base, "HEAD"])?;

    let mut status_map: HashMap<String, &str> = HashMap::new();
    for line in name_status.lines() {
        let mut parts = line.splitn(2, '\t');
        if let (Some(status), Some(path)) = (parts.next(), parts.next()) {
            let change = match status.chars().next() {
                Some('A') => "added",
                Some('D') => "deleted",
                _ => "modified",
            };
            status_map.insert(path.to_string(), change);
        }
    }

    let mut entries = Vec::new();
    for line in numstat.lines() {
        let mut parts = line.splitn(3, '\t');
        let ins = parts.next().unwrap_or("0");
        let del = parts.next().unwrap_or("0");
        let path = parts.next().unwrap_or("").to_string();
        if has_noise_component(Path::new(&path)) {
            continue;
        }
        let change_type = status_map.get(&path).copied().unwrap_or("modified").to_string();
        entries.push(DiffStatEntry {
            path,
            change_type,
            // Binary files report "-" instead of a count; treat as 0.
            insertions: ins.parse().unwrap_or(0),
            deletions: del.parse().unwrap_or(0),
        });
    }
    Ok(entries)
}

/// Drops per-file blocks touching a [`NOISE_DIR_NAMES`] path from a `git
/// diff` text patch. A patch is a sequence of blocks, each starting with a
/// `diff --git a/<path> b/<path>` header line — that header line (not the
/// `--- `/`+++ ` lines, which are absent for pure mode-change blocks) is
/// where every block's path(s) live, so it's what this parses.
fn filter_noise_from_patch(patch: &str) -> String {
    if patch.is_empty() {
        return String::new();
    }
    let mut blocks: Vec<&str> = Vec::new();
    let mut start = 0;
    for (i, _) in patch.match_indices("\ndiff --git ") {
        blocks.push(&patch[start..=i]);
        start = i + 1;
    }
    blocks.push(&patch[start..]);

    blocks.into_iter().filter(|b| !block_touches_noise(b)).collect()
}

/// Parses a patch block's `diff --git a/<old> b/<new>` header line and
/// checks both sides against [`has_noise_component`]. Splitting `a/<old>
/// b/<new>` on the first `" b/"` is a well-known heuristic (ambiguous only
/// for paths that themselves contain the literal substring `" b/"`) — good
/// enough for a denylist fallback, not worth a full patch-header parser.
fn block_touches_noise(block: &str) -> bool {
    let Some(first_line) = block.lines().next() else {
        return false;
    };
    let Some(rest) = first_line.trim_start().strip_prefix("diff --git a/") else {
        return false;
    };
    let Some(idx) = rest.find(" b/") else {
        return false;
    };
    let (old_path, new_path) = (&rest[..idx], &rest[idx + 3..]);
    has_noise_component(Path::new(old_path)) || has_noise_component(Path::new(new_path))
}

#[derive(Deserialize, Default)]
struct ComposeFile {
    #[serde(default)]
    services: std::collections::BTreeMap<String, ComposeService>,
}

#[derive(Deserialize, Default)]
struct ComposeService {
    image: Option<String>,
    build: Option<serde_yaml::Value>,
    #[serde(default)]
    ports: Vec<serde_yaml::Value>,
    #[serde(default)]
    depends_on: serde_yaml::Value,
}

/// Just enough YAML reading to hand `ls-containers` service name / image /
/// build / ports / depends_on. Long-form (mapping-style) ports and the full
/// compose spec (networks, volumes, env...) are out of scope — the raw
/// docker-compose.yml still ships verbatim in the payload for anything that
/// needs more.
fn parse_compose_services(bytes: &[u8]) -> Result<Vec<ServiceDef>> {
    let compose: ComposeFile = serde_yaml::from_slice(bytes).context("parsing docker-compose.yml")?;
    let mut out = Vec::with_capacity(compose.services.len());
    for (name, svc) in compose.services {
        let image_or_build = if let Some(image) = &svc.image {
            format!("image:{image}")
        } else if let Some(build) = &svc.build {
            match build {
                serde_yaml::Value::String(s) => format!("build:{s}"),
                serde_yaml::Value::Mapping(m) => {
                    let ctx = m
                        .get(serde_yaml::Value::String("context".to_string()))
                        .and_then(|v| v.as_str())
                        .unwrap_or(".");
                    format!("build:{ctx}")
                }
                _ => "build:.".to_string(),
            }
        } else {
            String::new()
        };

        let ports = svc.ports.iter().filter_map(port_to_string).collect();

        let depends_on = match &svc.depends_on {
            serde_yaml::Value::Sequence(seq) => seq.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            serde_yaml::Value::Mapping(m) => m.keys().filter_map(|k| k.as_str().map(String::from)).collect(),
            _ => Vec::new(),
        };

        out.push(ServiceDef {
            name,
            image_or_build,
            ports,
            depends_on,
        });
    }
    Ok(out)
}

fn port_to_string(v: &serde_yaml::Value) -> Option<String> {
    match v {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Round 17: combines each folder's already-built tar.gz payload (each one
/// a complete, independently-valid `bundle_project` output — `source/`,
/// `diff_stat.json`, `diff.patch`, `docker-compose.yml`, `db-seed/`) into
/// one tar.gz for a multi-folder snapshot, with every entry re-homed under
/// `<label>/...` so two independent projects sent together (the developer's
/// real case: several standalone Spring Boot folders) never collide on path
/// — without this prefixing, two folders would both try to write
/// `source/README.md` into the same combined tar. Deliberately re-decodes
/// and re-encodes each bundle's tar.gz rather than reworking
/// `bundle_project` itself to take a shared builder + prefix: `bundle_project`
/// is exercised by five existing, passing tests against its exact
/// unprefixed output, and re-plumbing a prefix through every one of its
/// internal helpers risked those tests silently changing behavior for a
/// feature (multi-folder) those tests don't exist to cover. This function
/// is the one new, independently testable seam instead.
///
/// Any `dumps` are appended as one more tar entry each, at
/// `db-dumps/<folder>/<file_name>` — matching `DatabaseDumpEntry.dump_file`
/// exactly, since that's what a receiver needs to actually find the bytes.
/// `file_name` (not just a bare schema name) is the caller's job to build —
/// round 18: different engines' dumps get different extensions (a plain
/// `.sql` text dump for MySQL/PostgreSQL, a `.tar.gz` of MongoDB's own
/// `mongodump` directory output for MongoDB), which this function has no
/// reason to know about; see `create_snapshot_multi`'s `dump_file_extension`.
pub(crate) fn merge_folder_payloads(
    bundles: &[(String, GitBundle)],
    dumps: &[(String, String, Vec<u8>)],
) -> Result<Vec<u8>> {
    let gz = GzEncoder::new(Vec::new(), Compression::default());
    let mut tb = tar::Builder::new(gz);

    for (label, bundle) in bundles {
        let decoder = flate2::read::GzDecoder::new(bundle.payload.as_slice());
        let mut archive = tar::Archive::new(decoder);
        for entry in archive
            .entries()
            .with_context(|| format!("reading {label}'s bundle payload"))?
        {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            let new_path = Path::new(label).join(&path);
            let mut header = entry.header().clone();
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            header.set_size(buf.len() as u64);
            header.set_cksum();
            tb.append_data(&mut header, &new_path, buf.as_slice())
                .with_context(|| format!("adding {} to merged payload", new_path.display()))?;
        }
    }

    for (folder, file_name, dump_bytes) in dumps {
        let tar_path = format!("db-dumps/{folder}/{file_name}");
        append_bytes(&mut tb, &tar_path, dump_bytes)?;
    }

    let gz = tb.into_inner().context("finalizing merged tar")?;
    gz.finish().context("finalizing merged gzip")
}

fn append_bytes<W: Write>(tb: &mut tar::Builder<W>, path: &str, data: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tb.append_data(&mut header, path, data)
        .with_context(|| format!("adding {path} to payload"))
}

/// Re-homes every entry from a `git archive --format=tar` output under
/// `prefix/` in our own tar builder, so the caller's payload can combine it
/// with sibling entries (diff_stat.json, docker-compose.yml, ...).
fn append_git_archive<W: Write>(tb: &mut tar::Builder<W>, archive_bytes: &[u8], prefix: &str) -> Result<()> {
    let mut archive = tar::Archive::new(archive_bytes);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if has_noise_component(&path) {
            continue;
        }
        let new_path = Path::new(prefix).join(&path);
        let mut header = entry.header().clone();
        tb.append_data(&mut header, &new_path, &mut entry)
            .with_context(|| format!("adding {} to payload", new_path.display()))?;
    }
    Ok(())
}

fn git_text(root: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(git_bytes(root, args)?)?)
}

/// Runs one read-only `git` subprocess and returns its stdout.
///
/// Hardened against a real hang a manual two-machine test hit in the field
/// (sender stalled indefinitely before ever producing an offer, with
/// nothing in the network layer to blame): `Command::output()` leaves
/// **stdin inherited from the parent**, which in the real app is a GUI
/// process with no controlling terminal, and applies **no timeout** to the
/// child. If any git invocation ever blocked — an unusual
/// `core.pager`/credential-helper config, or anything else — `bundle_project`,
/// and the whole Send flow behind it (`share_snapshot` awaits this before
/// doing anything else), would hang forever with zero feedback. Three
/// layers, each closing a different door:
///   1. `--no-pager` right after `-C <root>`: belt-and-suspenders against a
///      pager even attempting to start (git only pages when stdout is a
///      tty, which a piped `Command` never is — verified directly against
///      this repo with `core.pager` set — but this makes it structurally
///      impossible rather than "shouldn't happen").
///   2. `.stdin(Stdio::null())`: a child can't block waiting for input that
///      is explicitly closed, removing the most likely hang vector outright.
///   3. A real [`GIT_TIMEOUT`], via the `wait-timeout` crate (`std::process`
///      has no built-in timeout). stdout/stderr are drained on background
///      threads *while* waiting — not after, like `Command::output()`'s
///      approach would suggest — because a large `git archive` can write
///      more than the OS pipe buffer holds; reading only after `wait`
///      returns would deadlock the exact way this function exists to avoid.
fn git_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let start = Instant::now();
    let mut child = git_command("git")
        .arg("-C")
        .arg(root)
        .arg("--no-pager")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning git {args:?}"))?;

    let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });

    let status = match child
        .wait_timeout(GIT_TIMEOUT)
        .with_context(|| format!("waiting on git {args:?}"))?
    {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            log::warn!("git {args:?} in {} timed out after {GIT_TIMEOUT:?}", root.display());
            bail!("git {args:?} did not finish within {GIT_TIMEOUT:?} (root: {})", root.display());
        }
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    let elapsed = start.elapsed();

    if !status.success() {
        log::warn!("git {args:?} failed after {elapsed:?}: {}", String::from_utf8_lossy(&stderr));
        bail!("git {args:?} failed: {}", String::from_utf8_lossy(&stderr));
    }
    log::info!("git {args:?} completed in {elapsed:?} ({} bytes)", stdout.len());
    Ok(stdout)
}
