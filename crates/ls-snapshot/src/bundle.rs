use anyhow::{bail, Context, Result};
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

/// What a sender sees when `git` itself isn't installed, instead of a raw
/// "program not found" spawn error.
pub const GIT_MISSING: &str =
    "Git isn't installed - LocalSync needs it to package your project. Install it from git-scm.com and restart LocalSync.";

/// `git --version` runs. The compose wizard's folder check asks this first.
pub fn git_available() -> bool {
    git_command("git")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn spawn_error(e: std::io::Error, args: &[&str]) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        anyhow::anyhow!(GIT_MISSING)
    } else {
        anyhow::Error::new(e).context(format!("spawning git {args:?}"))
    }
}

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

/// Round 36: payload compression codec. Both `bundle_project`'s per-folder
/// tar and `merge_folder_payloads`'s combined tar use this — a real 5GB
/// database dump transferring at ~5MB/sec uncompressed made transfer
/// *size/time* (a separate axis from round 35's transfer *memory* fix) a
/// real, reported problem. zstd at this level is both faster than gzip's
/// `Compression::default()` (6) and compresses same-content-class data
/// (repetitive SQL/text) noticeably smaller — see
/// `crates/ls-snapshot/tests/compression_ratio_test.rs` for the measured
/// numbers this level was picked against. Level 3 is zstd's own documented
/// default: a deliberate speed/ratio balance, not tuned specifically for
/// this project's data.
const ZSTD_LEVEL: i32 = 3;

/// Wraps `w` in a streaming zstd encoder — the direct swap-in replacement
/// for the `GzEncoder::new(w, Compression::default())` this project used
/// before round 36. Like `GzEncoder`, this only ever buffers one bounded
/// internal window's worth of data at a time no matter how much gets
/// written through it — `tar::Builder` (and, for a `DumpSource::FilePath`
/// dump, `append_dump_source`'s `BufReader`) still feed it in the same
/// small, bounded chunks as before, so a multi-GB dump never gets
/// materialized in memory here either — round 35's streaming fix is
/// unaffected, just fed through a different compressor.
fn zstd_encoder<W: Write>(w: W) -> Result<zstd::stream::write::Encoder<'static, W>> {
    zstd::stream::write::Encoder::new(w, ZSTD_LEVEL).context("initializing zstd encoder")
}

/// Wraps a byte slice in a streaming zstd decoder — the direct replacement
/// for the `flate2::read::GzDecoder::new(...)` this project used before
/// round 36.
fn zstd_decoder(bytes: &[u8]) -> Result<zstd::stream::read::Decoder<'static, std::io::BufReader<&[u8]>>> {
    zstd::stream::read::Decoder::new(bytes).context("initializing zstd decoder")
}

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
    bundle_project_with(project_root, parent_commit, None, &NewFileSelection::default())
}

/// [`bundle_project`], optionally with [`GeneratedFiles`] standing in for a
/// compose file the project doesn't have. When `generated` is given its
/// compose YAML is used instead of reading `docker-compose.yml` from disk,
/// and its extra files are added under `source/`. `selection` picks which of
/// the folder's new files go in (see [`snapshot_commit_with`]).
pub fn bundle_project_with(
    project_root: &Path,
    parent_commit: Option<&str>,
    generated: Option<&crate::types::GeneratedFiles>,
    selection: &NewFileSelection,
) -> Result<GitBundle> {
    log::info!("bundling started: project_root={}", project_root.display());
    // HEAD, or a commit of HEAD plus the folder's uncommitted changes and new
    // files - see `snapshot_commit_with`. Everything below (archive, diff,
    // diff stat) is of it.
    let git_commit = snapshot_commit_with(project_root, selection)?;

    let diff_stat = build_diff_stat(project_root, parent_commit, &git_commit)?;
    let diff_stat_json = serde_json::to_vec_pretty(&diff_stat)?;

    let diff_patch = match parent_commit {
        Some(parent) => filter_noise_from_patch(&git_text(project_root, &["diff", "--relative", &format!("{parent}..{git_commit}")])?),
        None => String::new(),
    };

    let compose_path = project_root.join("docker-compose.yml");
    let compose_bytes = if let Some(g) = generated {
        Some(g.compose_yaml.as_bytes().to_vec())
    } else if compose_path.is_file() {
        Some(fs::read(&compose_path).with_context(|| format!("reading {}", compose_path.display()))?)
    } else {
        None
    };
    let services = match &compose_bytes {
        Some(bytes) => parse_compose_services(bytes)?,
        None => Vec::new(),
    };

    let db_seed_dir = resolve_db_seed_dir(project_root);

    let zstd = zstd_encoder(Vec::new())?;
    let mut tb = tar::Builder::new(zstd);

    let archive_bytes = git_bytes(project_root, &["archive", "--format=tar", &git_commit])?;
    append_git_archive(&mut tb, &archive_bytes, "source", generated)?;
    if let Some(g) = generated {
        for (rel, bytes) in &g.files {
            append_bytes(&mut tb, &format!("source/{rel}"), bytes)?;
        }
    }

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

    let zstd = tb.into_inner().context("finalizing tar")?;
    let payload = zstd.finish().context("finalizing zstd stream")?;

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

fn build_diff_stat(project_root: &Path, parent_commit: Option<&str>, commit: &str) -> Result<Vec<DiffStatEntry>> {
    let base = parent_commit.unwrap_or(EMPTY_TREE);

    // --no-renames: diff_stat's change_type is only added/modified/deleted
    // (no "renamed" variant), so a rename is simplest as a delete+add pair
    // rather than teaching the parser `old => new` path syntax.
    // --relative: `project_root` may be a subfolder of a larger repo (`.git`
    // further up). git diff always covers the whole repo; this limits it to
    // the selected folder and makes paths relative to it, matching what `git
    // archive` (which already only takes the current folder) ships. At a repo
    // root it changes nothing.
    let numstat = git_text(project_root, &["diff", "--relative", "--no-renames", "--numstat", base, commit])?;
    let name_status = git_text(project_root, &["diff", "--relative", "--no-renames", "--name-status", base, commit])?;

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
///
/// `externals` are (payload path, path on disk) for files or folders from
/// outside the project (`ExternalFile::bundle_path`); a folder's files go
/// under its payload path.
pub(crate) fn merge_folder_payloads(
    bundles: &[(String, GitBundle)],
    dumps: &[(String, String, &crate::types::DumpSource)],
    externals: &[(&str, &Path)],
) -> Result<Vec<u8>> {
    let zstd = zstd_encoder(Vec::new())?;
    let mut tb = tar::Builder::new(zstd);

    for (label, bundle) in bundles {
        let decoder = zstd_decoder(bundle.payload.as_slice())
            .with_context(|| format!("decoding {label}'s bundle payload"))?;
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

    for (folder, file_name, source) in dumps {
        let tar_path = format!("db-dumps/{folder}/{file_name}");
        append_dump_source(&mut tb, &tar_path, source)?;
    }

    for (tar_path, src) in externals {
        if src.is_dir() {
            for file in walk_files(src)? {
                let rel = file.strip_prefix(src).unwrap().to_string_lossy().replace('\\', "/");
                append_dump_source(&mut tb, &format!("{tar_path}/{rel}"), &crate::types::DumpSource::FilePath(file))?;
            }
        } else {
            append_dump_source(&mut tb, tar_path, &crate::types::DumpSource::FilePath(src.to_path_buf()))
                .with_context(|| format!("reading {}", src.display()))?;
        }
    }

    let zstd = tb.into_inner().context("finalizing merged tar")?;
    zstd.finish().context("finalizing merged zstd stream")
}

fn append_bytes<W: Write>(tb: &mut tar::Builder<W>, path: &str, data: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tb.append_data(&mut header, path, data)
        .with_context(|| format!("adding {path} to payload"))
}

/// Appends one database dump's content to the tar builder, streaming it in
/// bounded chunks (via `tar::Builder::append_data`'s internal buffered copy
/// loop, fed a `BufReader` over the open file) rather than reading the whole
/// dump into a `Vec<u8>` first when it lives on disk (`DumpSource::FilePath`)
/// — the fix for a real `out of memory` failure against a large, real
/// database dump. `Bytes` (a dump already in memory — e.g. built directly by
/// a test) still goes through `append_bytes` unchanged.
fn append_dump_source<W: Write>(tb: &mut tar::Builder<W>, path: &str, source: &crate::types::DumpSource) -> Result<()> {
    match source {
        crate::types::DumpSource::Bytes(bytes) => append_bytes(tb, path, bytes),
        crate::types::DumpSource::FilePath(file_path) => {
            let len = fs::metadata(file_path)
                .with_context(|| format!("stat-ing dump file {}", file_path.display()))?
                .len();
            let file = fs::File::open(file_path)
                .with_context(|| format!("opening dump file {}", file_path.display()))?;
            let mut reader = std::io::BufReader::with_capacity(256 * 1024, file);

            let mut header = tar::Header::new_gnu();
            header.set_size(len);
            header.set_mode(0o644);
            header.set_cksum();
            tb.append_data(&mut header, path, &mut reader)
                .with_context(|| format!("adding {path} to payload"))
        }
    }
}

/// Re-homes every entry from a `git archive --format=tar` output under
/// `prefix/` in our own tar builder, so the caller's payload can combine it
/// with sibling entries (diff_stat.json, docker-compose.yml, ...).
///
/// With `shadow_build_files`, the project's own top-level `Dockerfile` /
/// `Containerfile` (any letter case) are stored as `<name>.original`, because
/// a generated Dockerfile is about to be added and build tools pick a default-
/// named file from the context - some (podman-compose on Windows) ignoring the
/// compose file's `dockerfile:` entirely - so a second candidate could be
/// built silently instead.
///
/// A project file that a generated file replaces (LocalSync's rewritten copy
/// of a config file, say) is left out, so the payload holds one copy of it.
fn append_git_archive<W: Write>(
    tb: &mut tar::Builder<W>,
    archive_bytes: &[u8],
    prefix: &str,
    generated: Option<&crate::types::GeneratedFiles>,
) -> Result<()> {
    let shadow_build_files = generated.is_some();
    let mut archive = tar::Archive::new(archive_bytes);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if has_noise_component(&path) {
            continue;
        }
        let mut new_path = Path::new(prefix).join(&path);
        if shadow_build_files && path.components().count() == 1 {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if matches!(name.to_ascii_lowercase().as_str(), "dockerfile" | "containerfile") {
                    new_path = Path::new(prefix).join(format!("{name}.original"));
                }
            }
        }
        let replaced = generated.is_some_and(|g| g.files.iter().any(|(rel, _)| Path::new(rel) == path));
        if replaced && new_path == Path::new(prefix).join(&path) {
            continue;
        }
        let mut header = entry.header().clone();
        tb.append_data(&mut header, &new_path, &mut entry)
            .with_context(|| format!("adding {} to payload", new_path.display()))?;
    }
    Ok(())
}

/// The commit `project_root` is at right now (`git rev-parse HEAD`), through
/// the same hardened git runner bundling uses (no pager, no stdin, timeout,
/// no console window on Windows). Lets a caller cheaply ask "has this
/// project moved since I built its snapshot?" without bundling it again.
pub fn head_commit(project_root: &Path) -> Result<String> {
    Ok(git_text(project_root, &["rev-parse", "HEAD"])?.trim().to_string())
}

/// Which of a folder's new files - untracked, and not ignored by git - go
/// into its snapshot: every one, except those in `excluded` and secret-looking
/// ones ([`looks_secret`]) that are not in `included`. Paths are relative to
/// the folder, with `/` separators, as git prints them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewFileSelection {
    pub excluded: Vec<String>,
    pub included: Vec<String>,
}

impl NewFileSelection {
    pub fn includes(&self, path: &str) -> bool {
        !self.excluded.iter().any(|p| p == path) && (!looks_secret(path) || self.included.iter().any(|p| p == path))
    }
}

/// A new (untracked) file bigger than this is listed but never sent: "too
/// large, not included". Commit it (or use Git LFS) to send it.
pub const NEW_FILE_MAX_BYTES: u64 = 25 * 1024 * 1024;

/// One of a folder's new files (see [`new_files`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NewFile {
    /// Relative to the folder, `/`-separated.
    pub path: String,
    /// [`looks_secret`]: left out until the user includes it.
    pub secret: bool,
    /// Over [`NEW_FILE_MAX_BYTES`]: never included, whatever the selection.
    pub too_large: bool,
    /// The snapshot includes it ([`NewFileSelection::includes`], and not too large).
    pub included: bool,
}

/// A file name that usually holds credentials (any letter case): `.env*`,
/// `*.env`, `*.pem`, `*.key`, `*.p12`, `id_rsa*`, `credentials*`,
/// `secrets*`. A new file named like this stays out of the snapshot until
/// the user includes it.
pub fn looks_secret(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name.starts_with(".env")
        || name.ends_with(".env")
        || [".pem", ".key", ".p12"].iter().any(|ext| name.ends_with(ext))
        || ["id_rsa", "credentials", "secrets"].iter().any(|p| name.starts_with(p))
}

/// The folder's new files: untracked and not ignored (`.gitignore`,
/// `.git/info/exclude`, the global excludes file), each marked with whether
/// `selection` includes it. Left out entirely: an untracked nested git
/// repository (git lists it as `dir/`, not as files), files under a
/// [`NOISE_DIR_NAMES`] directory (never shipped anyway), and names that
/// aren't UTF-8. Files over [`NEW_FILE_MAX_BYTES`] are listed, never included.
pub fn new_files(project_root: &Path, selection: &NewFileSelection) -> Result<Vec<NewFile>> {
    let out = git_bytes(project_root, &["ls-files", "--others", "--exclude-standard", "-z", "--", "."])?;
    Ok(out
        .split(|b| *b == 0)
        .filter_map(|p| std::str::from_utf8(p).ok())
        .filter(|p| !p.is_empty() && !p.ends_with('/') && !has_noise_component(Path::new(p)))
        .map(|p| {
            let too_large = fs::symlink_metadata(project_root.join(p)).is_ok_and(|m| m.len() > NEW_FILE_MAX_BYTES);
            NewFile { path: p.to_string(), secret: looks_secret(p), too_large, included: !too_large && selection.includes(p) }
        })
        .collect())
}

/// The commit a snapshot of `project_root` is built from, with every new
/// file the default [`NewFileSelection`] includes. See [`snapshot_commit_with`].
pub fn snapshot_commit(project_root: &Path) -> Result<String> {
    snapshot_commit_with(project_root, &NewFileSelection::default())
}

/// The commit a snapshot of `project_root` is built from - and what decides
/// whether the project changed since a device last received it.
///
/// HEAD when the folder has no uncommitted changes to tracked files and no
/// new file `selection` includes. Otherwise a commit of HEAD plus those
/// changes and files, made the way `git stash create` does: nothing the
/// developer sees is touched (no branch, ref, index or file changes). Their
/// index is copied to a temporary one, the folder's edits to tracked files
/// and its chosen new files are staged there, and that tree is committed with
/// a fixed author and date, so the same content always gives the same id.
/// Ignored files never go in, and neither does a secret-looking new file
/// (an untracked `.env`) unless the user included it.
///
/// ponytail: the commit is unreachable, so `git gc` may prune it after ~2
/// weeks; a later push diffing against it then sends the full snapshot
/// (`session_commands::ensure_artifact` checks [`commit_exists`] first).
pub fn snapshot_commit_with(project_root: &Path, selection: &NewFileSelection) -> Result<String> {
    let head = head_commit(project_root)?;
    let files = new_files(project_root, selection)?;
    commit_with_new_files(project_root, &head, &files)
}

/// [`snapshot_commit_with`] for a [`new_files`] list the caller already has.
pub fn snapshot_commit_of(project_root: &Path, files: &[NewFile]) -> Result<String> {
    let head = head_commit(project_root)?;
    commit_with_new_files(project_root, &head, files)
}

fn commit_with_new_files(project_root: &Path, head: &str, files: &[NewFile]) -> Result<String> {
    let added: Vec<&str> = files.iter().filter(|f| f.included).map(|f| f.path.as_str()).collect();
    // --no-optional-locks: a plain `git status` refreshes the stat info in
    // the developer's own index and writes it back; this must not touch it.
    let dirty = git_text(project_root, &["--no-optional-locks", "status", "--porcelain", "--untracked-files=no", "--", "."])?;
    if dirty.trim().is_empty() && added.is_empty() {
        return Ok(head.to_string());
    }
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp_index = std::env::temp_dir().join(format!(
        "localsync-index-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let tmp_paths = tmp_index.with_extension("paths");
    let real_index = git_text(project_root, &["rev-parse", "--path-format=absolute", "--git-path", "index"])?;
    let real_index = Path::new(real_index.trim());
    if real_index.is_file() {
        // Starting from the real index keeps git's stat cache: only files
        // that actually changed get re-hashed.
        fs::copy(real_index, &tmp_index).with_context(|| format!("copying {}", real_index.display()))?;
    }
    let index_env = [("GIT_INDEX_FILE", tmp_index.as_os_str())];
    let made = (|| -> Result<String> {
        git_bytes_with(project_root, &["add", "-u", "--", "."], &index_env)?;
        let tree = match stage_new_files(project_root, &added, &index_env, &tmp_paths, true) {
            Ok(tree) => tree,
            // A remembered blob can be gone (`git gc` prunes unreferenced
            // objects): hash everything again, once.
            Err(e) => {
                log::info!("re-hashing the new files of {}: {e:#}", project_root.display());
                forget_hashed(project_root);
                stage_new_files(project_root, &added, &index_env, &tmp_paths, false)?
            }
        };
        let fixed = std::ffi::OsStr::new;
        let author = [
            ("GIT_AUTHOR_NAME", fixed("LocalSync")),
            ("GIT_AUTHOR_EMAIL", fixed("localsync@localhost")),
            ("GIT_AUTHOR_DATE", fixed("1970-01-01T00:00:00Z")),
            ("GIT_COMMITTER_NAME", fixed("LocalSync")),
            ("GIT_COMMITTER_EMAIL", fixed("localsync@localhost")),
            ("GIT_COMMITTER_DATE", fixed("1970-01-01T00:00:00Z")),
        ];
        let commit = git_bytes_with(
            project_root,
            &["commit-tree", "--no-gpg-sign", tree.trim(), "-p", head, "-m", "LocalSync: uncommitted changes"],
            &author,
        )?;
        Ok(String::from_utf8(commit)?.trim().to_string())
    })();
    let _ = fs::remove_file(&tmp_index);
    let _ = fs::remove_file(&tmp_paths);
    made
}

/// What a new file looked like when it was last hashed: unchanged size and
/// modified time mean the same blob, so the 5 s change poll doesn't read every
/// new file again. Keyed by the file's full path.
struct Hashed {
    size: u64,
    modified: std::time::SystemTime,
    mode: String,
    blob: String,
}

/// ponytail: process-wide and never trimmed - one small entry per new file
/// ever hashed; trim it if sessions with huge numbers of new files appear.
static HASHED: std::sync::Mutex<Option<HashMap<PathBuf, Hashed>>> = std::sync::Mutex::new(None);

/// A file changed less than this long ago isn't remembered: another write
/// within the same timestamp tick would leave size and time unchanged (git's
/// "racy" case).
const RACY_WINDOW: Duration = Duration::from_secs(2);

fn forget_hashed(project_root: &Path) {
    if let Ok(mut map) = HASHED.lock() {
        if let Some(map) = map.as_mut() {
            map.retain(|path, _| !path.starts_with(project_root));
        }
    }
}

/// Stages `added` (new files, relative to `project_root`) in the temporary
/// index and writes its tree. With `use_memo`, a file whose size and modified
/// time match what [`HASHED`] remembers goes in as that blob, unread; the rest
/// are hashed by `git add` and remembered.
fn stage_new_files(
    project_root: &Path,
    added: &[&str],
    index_env: &[(&str, &std::ffi::OsStr)],
    tmp_paths: &Path,
    use_memo: bool,
) -> Result<String> {
    // Stat first: a file changed after this and before `git add` reads it is
    // remembered with the old size/time, so it's simply hashed again next time.
    let stats: Vec<Option<(u64, std::time::SystemTime)>> = added
        .iter()
        .map(|p| fs::symlink_metadata(project_root.join(p)).ok().and_then(|m| Some((m.len(), m.modified().ok()?))))
        .collect();
    let mut known = Vec::new();
    let mut fresh = Vec::new();
    {
        let memo = HASHED.lock().map_err(|_| anyhow::anyhow!("hash memo poisoned"))?;
        for (path, stat) in added.iter().zip(&stats) {
            let hit = use_memo
                .then(|| memo.as_ref()?.get(&project_root.join(path)))
                .flatten()
                .filter(|h| stat.is_some_and(|(size, modified)| h.size == size && h.modified == modified));
            match hit {
                Some(h) => known.push(format!("{} {}\t{path}\0", h.mode, h.blob)),
                None => fresh.push(*path),
            }
        }
    }
    if !known.is_empty() {
        run_git_input(project_root, &["update-index", "-z", "--add", "--index-info"], index_env, GIT_TIMEOUT, Some(known.concat().into_bytes()))?;
    }
    if !fresh.is_empty() {
        // From a file, not the command line (which a few thousand paths
        // would overflow on Windows); literal, so a `[` or `*` in a file
        // name is just a character.
        let mut list = Vec::new();
        for path in &fresh {
            list.extend_from_slice(path.as_bytes());
            list.push(0);
        }
        fs::write(tmp_paths, list).with_context(|| format!("writing {}", tmp_paths.display()))?;
        let from_file = format!("--pathspec-from-file={}", tmp_paths.display());
        git_bytes_with(project_root, &["--literal-pathspecs", "add", &from_file, "--pathspec-file-nul"], index_env)?;
        remember_hashed(project_root, &fresh, added, &stats, index_env)?;
    }
    Ok(String::from_utf8(git_bytes_with(project_root, &["write-tree"], index_env)?)?.trim().to_string())
}

/// Records the blob and mode `git add` gave each of `fresh` (read back from
/// the temporary index), with the size/time seen before it was hashed.
fn remember_hashed(
    project_root: &Path,
    fresh: &[&str],
    added: &[&str],
    stats: &[Option<(u64, std::time::SystemTime)>],
    index_env: &[(&str, &std::ffi::OsStr)],
) -> Result<()> {
    let listing = git_bytes_with(project_root, &["ls-files", "-s", "-z", "--", "."], index_env)?;
    let staged: HashMap<&str, (&str, &str)> = listing
        .split(|b| *b == 0)
        .filter_map(|e| std::str::from_utf8(e).ok())
        .filter_map(|e| {
            let (meta, path) = e.split_once('\t')?;
            let mut parts = meta.split(' ');
            Some((path, (parts.next()?, parts.next()?)))
        })
        .collect();
    let now = std::time::SystemTime::now();
    let mut memo = HASHED.lock().map_err(|_| anyhow::anyhow!("hash memo poisoned"))?;
    let memo = memo.get_or_insert_with(HashMap::new);
    for path in fresh {
        let Some(&(mode, blob)) = staged.get(path) else { continue };
        let Some((size, modified)) = added.iter().position(|p| p == path).and_then(|i| stats[i]) else { continue };
        if now.duration_since(modified).unwrap_or_default() < RACY_WINDOW {
            continue;
        }
        memo.insert(project_root.join(path), Hashed { size, modified, mode: mode.to_string(), blob: blob.to_string() });
    }
    Ok(())
}

/// The id of `project_root`'s own tree (just this folder's files) at
/// `commit`: equal for two commits with the same content - e.g. the snapshot
/// commit of an uncommitted edit, and the real commit made of it afterwards.
pub fn folder_tree(project_root: &Path, commit: &str) -> Result<String> {
    Ok(git_text(project_root, &["rev-parse", &format!("{commit}:./")])?.trim().to_string())
}

/// `commit` is still in `project_root`'s repository. An unreachable snapshot
/// commit (see [`snapshot_commit_with`]) can be pruned by `git gc`, and a
/// rewritten history drops commits too - neither can be diffed against.
pub fn commit_exists(project_root: &Path, commit: &str) -> bool {
    git_bytes(project_root, &["cat-file", "-e", &format!("{commit}^{{commit}}")]).is_ok()
}

/// One file that differs between two commits of a folder.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChangedFile {
    /// Relative to the folder, `/`-separated.
    pub path: String,
    /// "added", "modified" or "deleted" - the same words as the review's
    /// `diff_stat.json`.
    pub change_type: String,
}

/// What changed in `project_root` from `from` to `to`, limited to and
/// relative to the folder, with the same noise filtering as the payload.
/// Only names and kinds (no line counts), so it is cheap enough to poll.
pub fn changed_files(project_root: &Path, from: &str, to: &str) -> Result<Vec<ChangedFile>> {
    let out = git_bytes(project_root, &["diff", "--relative", "--no-renames", "--name-status", "-z", from, to])?;
    // `-z`: "<status>\0<path>\0" per file (no renames, so one path each).
    let mut fields = out.split(|b| *b == 0).filter(|f| !f.is_empty());
    let mut files = Vec::new();
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        let path = String::from_utf8_lossy(path).into_owned();
        if has_noise_component(Path::new(&path)) {
            continue;
        }
        let change_type = match status.first().copied() {
            Some(b'A') => "added",
            Some(b'D') => "deleted",
            _ => "modified",
        };
        files.push(ChangedFile { path, change_type: change_type.to_string() });
    }
    Ok(files)
}

fn git_text(root: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(git_bytes(root, args)?)?)
}

fn git_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    git_bytes_with(root, args, &[])
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
fn git_bytes_with(root: &Path, args: &[&str], envs: &[(&str, &std::ffi::OsStr)]) -> Result<Vec<u8>> {
    run_git(root, args, envs, GIT_TIMEOUT)
}

/// [`git_bytes_with`] with the caller's own timeout, for the few git calls
/// that write (the desktop app's "set up git for this folder": `git add -A`
/// on a large folder can take longer than a read). Same hardening.
pub fn run_git(root: &Path, args: &[&str], envs: &[(&str, &std::ffi::OsStr)], timeout: Duration) -> Result<Vec<u8>> {
    run_git_input(root, args, envs, timeout, None)
}

/// [`run_git`], writing `input` to git's stdin (and closing it) instead of
/// leaving stdin closed - for `update-index --index-info`.
fn run_git_input(root: &Path, args: &[&str], envs: &[(&str, &std::ffi::OsStr)], timeout: Duration, input: Option<Vec<u8>>) -> Result<Vec<u8>> {
    let start = Instant::now();
    let mut child = git_command("git")
        .arg("-C")
        .arg(root)
        .arg("--no-pager")
        .args(args)
        .envs(envs.iter().copied())
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| spawn_error(e, args))?;
    // Written from its own thread, so a git that answers while it reads can't
    // fill the stdout pipe and stall both sides.
    let stdin_writer = input.and_then(|bytes| {
        let mut pipe = child.stdin.take()?;
        Some(std::thread::spawn(move || {
            let _ = pipe.write_all(&bytes);
        }))
    });

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
        .wait_timeout(timeout)
        .with_context(|| format!("waiting on git {args:?}"))?
    {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            log::warn!("git {args:?} in {} timed out after {timeout:?}", root.display());
            bail!("git {args:?} did not finish within {timeout:?} (root: {})", root.display());
        }
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    if let Some(writer) = stdin_writer {
        let _ = writer.join();
    }
    let elapsed = start.elapsed();

    if !status.success() {
        log::warn!("git {args:?} failed after {elapsed:?}: {}", String::from_utf8_lossy(&stderr));
        bail!("git {args:?} failed: {}", String::from_utf8_lossy(&stderr));
    }
    log::info!("git {args:?} completed in {elapsed:?} ({} bytes)", stdout.len());
    Ok(stdout)
}

#[cfg(test)]
mod git_missing_tests {
    use super::*;

    #[test]
    fn missing_git_gets_the_plain_message_other_spawn_errors_keep_theirs() {
        let e = spawn_error(std::io::Error::from(std::io::ErrorKind::NotFound), &["rev-parse", "HEAD"]);
        assert_eq!(format!("{e:#}"), GIT_MISSING);
        assert!(GIT_MISSING.contains("git-scm.com"));
        let e = spawn_error(std::io::Error::from(std::io::ErrorKind::PermissionDenied), &["archive"]);
        assert!(format!("{e:#}").starts_with("spawning git [\"archive\"]"), "{e:#}");
    }
}

#[cfg(test)]
mod new_file_hash_tests {
    use super::*;
    use std::time::SystemTime;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git").args(["-c", "user.name=T", "-c", "user.email=t@e.com"]).args(args).current_dir(dir.path()).status().unwrap();
            assert!(ok.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        fs::write(dir.path().join("tracked.txt"), "tracked\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "init"]);
        dir
    }

    /// Writes `text` to `path` with a modified time well outside the racy window.
    fn write_at(path: &Path, text: &str, modified: SystemTime) {
        fs::write(path, text).unwrap();
        fs::File::options().write(true).open(path).unwrap().set_modified(modified).unwrap();
    }

    #[test]
    fn an_unchanged_new_file_is_not_read_again_on_the_next_poll() {
        let dir = repo();
        let root = dir.path();
        let file = root.join("notes.txt");
        let old = SystemTime::now() - Duration::from_secs(600);
        write_at(&file, "aaaa\n", old);
        let first = snapshot_commit(root).unwrap();
        assert_ne!(first, head_commit(root).unwrap(), "the new file is in the snapshot");

        // Same size, same modified time, different bytes: only a re-read would
        // notice, and the poll must not re-read it.
        write_at(&file, "bbbb\n", old);
        assert_eq!(snapshot_commit(root).unwrap(), first, "taken from what was remembered, not hashed again");

        // A real change (new size) is hashed.
        write_at(&file, "bbbbbb\n", old);
        let changed = snapshot_commit(root).unwrap();
        assert_ne!(changed, first);
        let shown = Command::new("git").args(["show", &format!("{changed}:notes.txt")]).current_dir(root).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&shown.stdout), "bbbbbb\n");
    }

    #[test]
    fn a_remembered_blob_that_is_gone_is_hashed_again() {
        let dir = repo();
        let root = dir.path();
        let file = root.join("data.txt");
        let old = SystemTime::now() - Duration::from_secs(600);
        write_at(&file, "real content\n", old);
        HASHED.lock().unwrap().get_or_insert_with(HashMap::new).insert(
            root.join("data.txt"),
            Hashed { size: 13, modified: old, mode: "100644".into(), blob: "0123456789012345678901234567890123456789".into() },
        );
        let commit = snapshot_commit(root).unwrap();
        let shown = Command::new("git").args(["show", &format!("{commit}:data.txt")]).current_dir(root).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&shown.stdout), "real content\n");
    }

    #[test]
    fn a_new_file_over_25_mb_is_listed_but_never_included() {
        let dir = repo();
        let root = dir.path();
        let big = fs::File::create(root.join("dump.bin")).unwrap();
        big.set_len(NEW_FILE_MAX_BYTES + 1).unwrap();
        drop(big);
        fs::write(root.join("small.txt"), "small\n").unwrap();
        let everything = NewFileSelection { excluded: vec![], included: vec!["dump.bin".into()] };
        let files = new_files(root, &everything).unwrap();
        let big = files.iter().find(|f| f.path == "dump.bin").unwrap();
        assert!(big.too_large && !big.included, "even when ticked in");
        assert!(files.iter().any(|f| f.path == "small.txt" && f.included && !f.too_large));
        let commit = snapshot_commit_with(root, &everything).unwrap();
        let listed = Command::new("git").args(["ls-tree", "--name-only", &commit]).current_dir(root).output().unwrap();
        let names = String::from_utf8_lossy(&listed.stdout);
        assert!(names.contains("small.txt") && !names.contains("dump.bin"), "{names}");
    }
}
