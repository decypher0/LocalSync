//! Storage view: what LocalSync has put on disk, per session, and removing
//! what no session in the list uses any more.
//!
//! What LocalSync writes, and what this module may delete:
//! - **Received-project folders**: `run_snapshot` unpacks each version into
//!   `<work dir>/<project>-<commit>/`. The work dir is the user's choice (could
//!   be any folder), so only those unpack folders directly inside it are ever
//!   removed - never the work dir itself - and only when they carry
//!   `ls_containers::UNPACK_MARKER` (written since this view existed) or, for
//!   older ones, the exact unpacked-snapshot layout ([`is_localsync_unpack_dir`]).
//! - **Database volumes**: `localsync-db-<seed hash>`, and only when no listed
//!   session's compose file pins it and no container uses it (`podman volume
//!   rm` without `-f`, which Podman refuses for an in-use volume).
//! - **Compose-wizard test runs** left behind by a crash (`test-runs/<pid>-<n>`
//!   of an earlier process) and **exported database dumps** (`db-exports/`)
//!   that no sender session refers to and are a day old.
//! - Never: a sender's project folder (or anything inside / containing one),
//!   tokens, device id, session list, setup state, logs.
//!
//! Work dirs scanned: every listed received session's `work_dir`, every work
//! dir a Run ever used (`work-dirs.json`, appended by `run_received_session`
//! and by every scan, so a closed session's folder is still found), and the
//! UI's default `/tmp/localsync-work`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use crate::received_session::ReceivedSession;
use crate::state::AppState;

const DEFAULT_WORK_DIR: &str = "/tmp/localsync-work";
const WORK_DIRS_FILE: &str = "work-dirs.json";
/// A dump the wizard just exported isn't in any session until Send creates
/// one; this long after, an unreferenced export is a leftover.
/// ponytail: age guess; tie exports to a wizard id if a day ever proves short.
const EXPORT_GRACE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

fn app_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("localsync"))
}

// ---------- safety predicates (pure; unit-tested below) ----------

fn is_commit_hex(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `<sanitized project>-<40 or 64 hex commit>` - `ls_containers::unpack_dir_name`'s shape.
pub fn looks_like_unpack_name(name: &str) -> bool {
    let Some((project, commit)) = name.rsplit_once('-') else { return false };
    !project.is_empty()
        && project.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && is_commit_hex(commit)
}

fn is_real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).map(|m| m.is_dir()).unwrap_or(false)
}

fn is_real_file(p: &Path) -> bool {
    std::fs::symlink_metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

/// The layout one unpacked folder of a snapshot has (see ls-snapshot's bundle).
fn has_snapshot_layout(dir: &Path) -> bool {
    is_real_file(&dir.join("diff_stat.json")) && is_real_file(&dir.join("diff.patch")) && is_real_dir(&dir.join("source"))
}

/// A directory LocalSync unpacked a snapshot into: a real directory (not a
/// symlink/junction) named like one, holding the marker - or, if made before
/// the marker existed, the snapshot layout at its root or one folder down.
pub fn is_localsync_unpack_dir(path: &Path) -> bool {
    if !is_real_dir(path) || !path.file_name().and_then(|n| n.to_str()).is_some_and(looks_like_unpack_name) {
        return false;
    }
    if is_real_file(&path.join(ls_containers::UNPACK_MARKER)) || has_snapshot_layout(path) {
        return true;
    }
    std::fs::read_dir(path)
        .map(|entries| {
            entries.flatten().any(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false) && has_snapshot_layout(&e.path()))
        })
        .unwrap_or(false)
}

/// The unpack folder a compose dir belongs to: itself, or (one folder sent,
/// nested under its label) its parent.
pub fn unpack_root_of(compose_dir: &Path) -> Option<PathBuf> {
    [Some(compose_dir), compose_dir.parent()].into_iter().flatten().find(|p| is_localsync_unpack_dir(p)).map(Path::to_path_buf)
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// `a` is `b`, inside it, or contains it - compared as given and resolved.
fn overlaps(a: &Path, b: &Path) -> bool {
    let (ca, cb) = (canon(a), canon(b));
    a.starts_with(b) || b.starts_with(a) || ca.starts_with(&cb) || cb.starts_with(&ca)
}

/// Every check before an unpack folder is deleted: a real directory (no
/// symlink/junction), directly inside `work_dir` once both are resolved, not a
/// drive root or the home dir, not overlapping any sender project folder in
/// `protected`, and recognizably LocalSync's own ([`is_localsync_unpack_dir`]).
pub fn check_removable_dir(target: &Path, work_dir: &Path, protected: &[PathBuf]) -> Result<(), String> {
    let shown = target.display();
    let meta = std::fs::symlink_metadata(target).map_err(|e| format!("{shown}: {e}"))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(format!("{shown} is not a plain folder"));
    }
    let (t, w) = (canon(target), canon(work_dir));
    if t.parent() != Some(w.as_path()) {
        return Err(format!("{shown} is not directly inside the work folder {}", work_dir.display()));
    }
    if t.parent().is_none() || dirs::home_dir().is_some_and(|h| canon(&h) == t) {
        return Err(format!("{shown} is a drive root or the home folder"));
    }
    if let Some(p) = protected.iter().find(|p| overlaps(target, p)) {
        return Err(format!("{shown} overlaps the sender project folder {}", p.display()));
    }
    if !is_localsync_unpack_dir(target) {
        return Err(format!("{shown} isn't a folder LocalSync unpacked"));
    }
    Ok(())
}

pub fn is_localsync_volume(name: &str) -> bool {
    name.strip_prefix("localsync-db-").is_some_and(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// A leftover compose-wizard test-run folder: `<pid>-<nanos>` from another
/// process than this one (this process's own may still be running).
fn is_stale_test_run(name: &str) -> bool {
    let Some((pid, nanos)) = name.split_once('-') else { return false };
    !nanos.is_empty() && nanos.bytes().all(|b| b.is_ascii_digit()) && pid.parse::<u32>().is_ok_and(|p| p != std::process::id())
}

/// `<sha256 hex>.sql` / `.tar.gz` - what `export_db_tables` writes.
fn is_export_name(name: &str) -> bool {
    let stem = name.strip_suffix(".sql").or_else(|| name.strip_suffix(".tar.gz"));
    stem.is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Bytes under `path`, never following symlinks/junctions.
pub fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if meta.file_type().is_symlink() {
        return 0;
    }
    if !meta.is_dir() {
        return meta.len();
    }
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let Ok(t) = e.file_type() else { continue };
            if t.is_symlink() {
                continue;
            } else if t.is_dir() {
                stack.push(e.path());
            } else {
                total += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

// ---------- what's in the session list ----------

/// Everything the session list (memory and session-history.json) says is in
/// use. A history file that can't be read fails the whole scan: deleting
/// anything without knowing what's listed is never safe.
struct Listed {
    received: Vec<ReceivedSession>,
    /// Every sender session's project folders - never touched.
    protected: Vec<PathBuf>,
    /// Dump files sender sessions send.
    export_refs: Vec<PathBuf>,
    /// Compose dirs of runs this process holds (`state.sessions`).
    live_compose_dirs: Vec<PathBuf>,
}

fn listed(state: &AppState) -> Result<Listed, String> {
    let mut received: HashMap<String, ReceivedSession> = state.received_sessions.lock().map_err(|e| e.to_string())?.clone();
    let mut projects = state.project_sessions.lock().map_err(|e| e.to_string())?.clone();
    for entry in crate::session_history::load()? {
        if let Some(r) = entry.received {
            received.entry(r.id.clone()).or_insert(r);
        }
        if let Some(p) = entry.project {
            projects.entry(p.id.clone()).or_insert(p);
        }
    }
    let folders: Vec<_> = projects.values().flat_map(|p| p.folders.clone()).collect();
    let live_compose_dirs = state.sessions.lock().map_err(|e| e.to_string())?.values().map(|r| r.compose_dir.clone()).collect();
    let mut received: Vec<_> = received.into_values().collect();
    received.sort_by(|a, b| b.last_received_at.cmp(&a.last_received_at));
    Ok(Listed {
        received,
        protected: folders.iter().filter(|f| !f.path.trim().is_empty()).map(|f| PathBuf::from(&f.path)).collect(),
        export_refs: folders.iter().filter_map(|f| f.dump.as_ref().map(|d| PathBuf::from(&d.file_path))).collect(),
        live_compose_dirs,
    })
}

fn load_work_dirs(data: &Path) -> Vec<String> {
    std::fs::read(data.join(WORK_DIRS_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Adds `dirs` to `work-dirs.json`, the list of every work dir a Run used -
/// so a session's folder is still found after the session is closed.
pub fn remember_work_dirs<'a>(dirs: impl IntoIterator<Item = &'a str>) {
    let Some(data) = app_data_dir() else { return };
    let mut known = load_work_dirs(&data);
    let before = known.len();
    for d in dirs {
        if !d.trim().is_empty() && !known.iter().any(|k| k == d) {
            known.push(d.to_string());
        }
    }
    if known.len() != before {
        let _ = std::fs::create_dir_all(&data);
        if let Err(e) = std::fs::write(data.join(WORK_DIRS_FILE), serde_json::to_vec_pretty(&known).unwrap_or_default()) {
            log::warn!("couldn't record work dirs: {e}");
        }
    }
}

/// Existing work dirs to scan, deduplicated by resolved path.
fn known_work_dirs(listed: &Listed) -> Vec<PathBuf> {
    let session_dirs: Vec<&str> = listed.received.iter().map(|s| s.work_dir.as_str()).collect();
    remember_work_dirs(session_dirs.iter().copied());
    let recorded = app_data_dir().map(|d| load_work_dirs(&d)).unwrap_or_default();
    let mut seen = HashSet::new();
    recorded
        .iter()
        .map(String::as_str)
        .chain(session_dirs)
        .chain([DEFAULT_WORK_DIR])
        .filter(|d| !d.trim().is_empty())
        .map(PathBuf::from)
        .filter(|d| d.is_dir() && seen.insert(canon(d)))
        .collect()
}

/// Unpack folders inside `work_dir` (direct children only).
fn unpack_dirs_in(work_dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(work_dir)
        .map(|entries| entries.flatten().map(|e| e.path()).filter(|p| is_localsync_unpack_dir(p)).collect())
        .unwrap_or_default()
}

/// The unpack folder names a session's versions use (current, and the one
/// before an update) - matched by name too, so a Run still in progress (no
/// `compose_dir` recorded yet) or one that failed still claims its folder.
fn session_names(s: &ReceivedSession) -> Vec<String> {
    let mut names = vec![ls_containers::unpack_dir_name(&s.title, &s.git_commit)];
    if let Some((_, commit)) = &s.previous_run {
        names.push(ls_containers::unpack_dir_name(&s.title, commit));
    }
    names
}

/// A session's unpack folders on disk: where its compose dirs live, plus any
/// folder named for its versions in a known work dir.
fn session_dirs(s: &ReceivedSession, work_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let compose = std::iter::once(s.compose_dir.as_str()).chain(s.previous_run.as_ref().map(|(d, _)| d.as_str()));
    let by_compose = compose.filter(|d| !d.is_empty()).filter_map(|d| unpack_root_of(Path::new(d)));
    let by_name = session_names(s).into_iter().flat_map(|n| work_dirs.iter().map(move |w| w.join(&n))).filter(|p| is_localsync_unpack_dir(p));
    for d in by_compose.chain(by_name) {
        if !out.iter().any(|o| canon(o) == canon(&d)) {
            out.push(d);
        }
    }
    out
}

/// Database volumes the compose files in `dirs` pin (root or one folder down).
fn volumes_of(dirs: &[PathBuf]) -> Vec<String> {
    let mut vols: Vec<String> = Vec::new();
    for d in dirs {
        let nested = std::fs::read_dir(d).map(|e| e.flatten().map(|e| e.path()).filter(|p| is_real_dir(p)).collect()).unwrap_or_else(|_| Vec::new());
        for c in std::iter::once(d.clone()).chain(nested) {
            for v in ls_containers::pinned_volumes_in(&c) {
                if !vols.contains(&v) {
                    vols.push(v);
                }
            }
        }
    }
    vols
}

/// The compose project an unpack folder runs as (from the marker, else its name).
fn compose_project_of(dir: &Path) -> Option<String> {
    let marker: Option<serde_json::Value> =
        std::fs::read(dir.join(ls_containers::UNPACK_MARKER)).ok().and_then(|b| serde_json::from_slice(&b).ok());
    if let Some(m) = marker {
        if let (Some(p), Some(c)) = (m["project"].as_str(), m["git_commit"].as_str()) {
            return Some(ls_containers::compose_project_name(p, c));
        }
    }
    let (project, commit) = dir.file_name()?.to_str()?.rsplit_once('-')?;
    Some(ls_containers::compose_project_name(project, commit))
}

// ---------- the report ----------

#[derive(Debug, Clone, Serialize)]
pub struct Part {
    pub label: String,
    pub bytes: u64,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SizedItem {
    pub name: String,
    pub bytes: u64,
    /// Volume: other listed sessions use it too, so freeing this one keeps it.
    pub shared: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionRow {
    pub id: String,
    pub name: String,
    pub running: bool,
    pub work_dir: String,
    pub dirs: Vec<SizedItem>,
    pub dirs_bytes: u64,
    pub volumes: Vec<SizedItem>,
    pub volumes_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnusedRow {
    /// What `clean_up_unused` takes back.
    pub id: String,
    /// "volume" | "folder" | "test-run" | "export".
    pub kind: String,
    pub label: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ImagesRow {
    pub localsync_count: usize,
    pub localsync_bytes: u64,
    pub unused_count: usize,
    pub unused_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageReport {
    pub total_bytes: u64,
    pub breakdown: Vec<Part>,
    pub sessions: Vec<SessionRow>,
    pub unused: Vec<UnusedRow>,
    pub unused_bytes: u64,
    pub images: Option<ImagesRow>,
    /// Set when Podman couldn't be asked: volume/image sizes are missing then,
    /// and no volume is offered for clean-up.
    pub podman_error: Option<String>,
    pub work_dirs: Vec<String>,
    pub scan_ms: u64,
}

#[derive(Debug, Clone)]
enum Removal {
    Volume(String),
    Folder { dir: PathBuf, work_dir: PathBuf },
    TestRun { dir: PathBuf, base: PathBuf },
    Export { file: PathBuf, base: PathBuf },
}

struct Scan {
    report: StorageReport,
    removals: HashMap<String, (Removal, u64)>,
    protected: Vec<PathBuf>,
}

struct PodmanFacts {
    totals: ls_containers::DiskTotals,
    volumes: Vec<ls_containers::VolumeUsage>,
    images: Vec<ls_containers::ImageInfo>,
    running: HashSet<String>,
}

async fn podman_facts() -> Result<PodmanFacts, String> {
    let running = tauri::async_runtime::spawn_blocking(ls_containers::running_compose_projects);
    let (totals, volumes, images) = tokio::join!(ls_containers::disk_totals(), ls_containers::volume_usage(), ls_containers::images());
    let running = running.await.map_err(|e| e.to_string())?;
    let e = |e: anyhow::Error| format!("{e:#}");
    Ok(PodmanFacts { totals: totals.map_err(e)?, volumes: volumes.map_err(e)?, images: images.map_err(e)?, running: running.map_err(e)? })
}

fn is_localsync_image(i: &ls_containers::ImageInfo) -> bool {
    i.names.iter().any(|n| n.starts_with("localhost/localsync-"))
}

/// Everything on the disk side, blocking (walks folders).
struct DiskFacts {
    work_dirs: Vec<PathBuf>,
    /// Per listed session (same order): its unpack folders with sizes, and the volumes they pin.
    per_session: Vec<(Vec<(PathBuf, u64)>, Vec<String>)>,
    /// Unpack folders no listed session claims: (dir, work dir, bytes, compose project).
    orphans: Vec<(PathBuf, PathBuf, u64, Option<String>)>,
    unpack_bytes: u64,
    test_runs_bytes: u64,
    stale_test_runs: Vec<(PathBuf, u64)>,
    exports_bytes: u64,
    stale_exports: Vec<(PathBuf, u64)>,
    logs_bytes: u64,
    other_bytes: u64,
    data_dir: Option<PathBuf>,
}

fn disk_facts(listed: &Listed) -> DiskFacts {
    let work_dirs = known_work_dirs(listed);
    let per_session: Vec<_> = listed
        .received
        .iter()
        .map(|s| {
            let dirs = session_dirs(s, &work_dirs);
            let vols = volumes_of(&dirs);
            (dirs.into_iter().map(|d| { let b = dir_size(&d); (d, b) }).collect::<Vec<_>>(), vols)
        })
        .collect();
    let mut claimed: HashSet<PathBuf> = per_session.iter().flat_map(|(d, _)| d.iter().map(|(p, _)| canon(p))).collect();
    claimed.extend(listed.live_compose_dirs.iter().filter_map(|d| unpack_root_of(d)).map(|d| canon(&d)));
    let claimed_names: HashSet<String> = listed.received.iter().flat_map(session_names).collect();

    let sized: HashMap<PathBuf, u64> = per_session.iter().flat_map(|(d, _)| d.iter().map(|(p, b)| (canon(p), *b))).collect();
    let mut unpack_bytes: u64 = sized.values().sum();
    let mut orphans = Vec::new();
    let mut seen = claimed.clone();
    for w in &work_dirs {
        for d in unpack_dirs_in(w) {
            let named = d.file_name().and_then(|n| n.to_str()).is_some_and(|n| claimed_names.contains(n));
            if !seen.insert(canon(&d)) || named {
                continue;
            }
            let bytes = dir_size(&d);
            unpack_bytes += bytes;
            let project = compose_project_of(&d);
            orphans.push((d, w.clone(), bytes, project));
        }
    }

    let data_dir = app_data_dir();
    let (mut test_runs_bytes, mut exports_bytes, mut logs_bytes, mut other_bytes) = (0, 0, 0, 0);
    let (mut stale_test_runs, mut stale_exports) = (Vec::new(), Vec::new());
    if let Some(data) = &data_dir {
        for e in std::fs::read_dir(data).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let path = e.path();
            match name.as_str() {
                "test-runs" => {
                    for t in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                        let b = dir_size(&t.path());
                        test_runs_bytes += b;
                        if is_real_dir(&t.path()) && is_stale_test_run(&t.file_name().to_string_lossy()) {
                            stale_test_runs.push((t.path(), b));
                        }
                    }
                }
                "db-exports" => {
                    let refs: Vec<PathBuf> = listed.export_refs.iter().map(|p| canon(p)).collect();
                    for f in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                        let p = f.path();
                        let b = dir_size(&p);
                        exports_bytes += b;
                        let old = std::fs::symlink_metadata(&p)
                            .and_then(|m| m.modified())
                            .ok()
                            .and_then(|t| t.elapsed().ok())
                            .is_some_and(|age| age > EXPORT_GRACE);
                        if is_real_file(&p) && is_export_name(&f.file_name().to_string_lossy()) && old && !refs.contains(&canon(&p)) {
                            stale_exports.push((p, b));
                        }
                    }
                }
                "logs" => logs_bytes += dir_size(&path),
                _ => other_bytes += dir_size(&path),
            }
        }
    }
    DiskFacts {
        work_dirs,
        per_session,
        orphans,
        unpack_bytes,
        test_runs_bytes,
        stale_test_runs,
        exports_bytes,
        stale_exports,
        logs_bytes,
        other_bytes,
        data_dir,
    }
}

async fn scan(state: &AppState) -> Result<Scan, String> {
    let started = std::time::Instant::now();
    let listed = listed(state)?;
    let received = listed.received.clone();
    let protected = listed.protected.clone();
    let disk = tauri::async_runtime::spawn_blocking(move || disk_facts(&listed));
    let podman = podman_facts().await;
    let disk = disk.await.map_err(|e| format!("scanning folders failed: {e}"))?;

    let (podman, podman_error) = match podman {
        Ok(p) => (Some(p), None),
        Err(e) => (None, Some(e)),
    };
    let volume_size: HashMap<&str, u64> = podman.iter().flat_map(|p| p.volumes.iter().map(|v| (v.name.as_str(), v.bytes))).collect();
    let running = |s: &ReceivedSession| {
        let Some(p) = &podman else { return false };
        std::iter::once(&s.git_commit)
            .chain(s.previous_run.as_ref().map(|(_, c)| c))
            .any(|c| p.running.contains(&ls_containers::compose_project_name(&s.title, c)))
    };

    let mut removals = HashMap::new();
    let mut unused = Vec::new();
    let mut add = |id: String, kind: &str, label: String, bytes: u64, removal: Removal| {
        unused.push(UnusedRow { id: id.clone(), kind: kind.into(), label, bytes });
        removals.insert(id, (removal, bytes));
    };

    let referenced_volumes: HashSet<&String> = disk.per_session.iter().flat_map(|(_, v)| v).collect();
    let sessions: Vec<SessionRow> = received
        .iter()
        .zip(&disk.per_session)
        .map(|(s, (dirs, vols))| {
            let users = |v: &String| disk.per_session.iter().filter(|(_, vs)| vs.contains(v)).count();
            let dirs: Vec<SizedItem> = dirs.iter().map(|(p, b)| SizedItem { name: p.display().to_string(), bytes: *b, shared: false }).collect();
            let volumes: Vec<SizedItem> = vols
                .iter()
                .map(|v| SizedItem { name: v.clone(), bytes: volume_size.get(v.as_str()).copied().unwrap_or(0), shared: users(v) > 1 })
                .collect();
            SessionRow {
                id: s.id.clone(),
                name: if s.name.trim().is_empty() { s.title.clone() } else { s.name.clone() },
                running: running(s),
                work_dir: s.work_dir.clone(),
                dirs_bytes: dirs.iter().map(|d| d.bytes).sum(),
                volumes_bytes: volumes.iter().map(|v| v.bytes).sum(),
                dirs,
                volumes,
            }
        })
        .collect();

    let mut breakdown = Vec::new();
    let mut images_row = None;
    if let Some(p) = &podman {
        // Volumes: only orphans with no container (links) - Podman would
        // refuse the rest anyway.
        for v in p.volumes.iter().filter(|v| is_localsync_volume(&v.name) && v.links == 0 && !referenced_volumes.contains(&v.name)) {
            add(format!("volume:{}", v.name), "volume", v.name.clone(), v.bytes, Removal::Volume(v.name.clone()));
        }
        let ls_volumes: u64 = p.volumes.iter().filter(|v| is_localsync_volume(&v.name)).map(|v| v.bytes).sum();
        let mine: Vec<_> = p.images.iter().filter(|i| is_localsync_image(i)).collect();
        let row = ImagesRow {
            localsync_count: mine.len(),
            localsync_bytes: mine.iter().map(|i| i.bytes).sum(),
            unused_count: mine.iter().filter(|i| i.containers == 0).count(),
            unused_bytes: mine.iter().filter(|i| i.containers == 0).map(|i| i.bytes).sum(),
        };
        breakdown.push(Part {
            label: "Podman images".into(),
            bytes: p.totals.images_bytes,
            note: format!(
                "All images on this computer's Podman - shared with any other Podman use. {} built by LocalSync.",
                row.localsync_count
            ),
        });
        breakdown.push(Part { label: "Database volumes".into(), bytes: ls_volumes, note: "LocalSync's localsync-db-* Podman volumes.".into() });
        images_row = Some(row);
    }
    for (dir, work_dir, bytes, project) in &disk.orphans {
        if project.as_ref().is_some_and(|proj| podman.as_ref().is_some_and(|p| p.running.contains(proj))) {
            continue; // still running: not unused
        }
        add(
            format!("folder:{}", dir.display()),
            "folder",
            dir.display().to_string(),
            *bytes,
            Removal::Folder { dir: dir.clone(), work_dir: work_dir.clone() },
        );
    }
    if let Some(data) = &disk.data_dir {
        for (dir, bytes) in &disk.stale_test_runs {
            add(format!("test-run:{}", dir.display()), "test-run", dir.display().to_string(), *bytes, Removal::TestRun { dir: dir.clone(), base: data.join("test-runs") });
        }
        for (file, bytes) in &disk.stale_exports {
            add(format!("export:{}", file.display()), "export", file.display().to_string(), *bytes, Removal::Export { file: file.clone(), base: data.join("db-exports") });
        }
    }
    breakdown.push(Part { label: "Received projects".into(), bytes: disk.unpack_bytes, note: "Unpacked copies in the work folders.".into() });
    breakdown.push(Part { label: "Compose test runs".into(), bytes: disk.test_runs_bytes, note: String::new() });
    breakdown.push(Part { label: "Exported database dumps".into(), bytes: disk.exports_bytes, note: String::new() });
    breakdown.push(Part { label: "Logs".into(), bytes: disk.logs_bytes, note: String::new() });
    breakdown.push(Part { label: "Settings and session list".into(), bytes: disk.other_bytes, note: String::new() });

    let unused_bytes = unused.iter().map(|u| u.bytes).sum();
    Ok(Scan {
        report: StorageReport {
            total_bytes: breakdown.iter().map(|p| p.bytes).sum(),
            breakdown,
            sessions,
            unused,
            unused_bytes,
            images: images_row,
            podman_error,
            work_dirs: disk.work_dirs.iter().map(|d| d.display().to_string()).collect(),
            scan_ms: started.elapsed().as_millis() as u64,
        },
        removals,
        protected,
    })
}

// ---------- commands ----------

#[tauri::command]
pub async fn storage_report(state: State<'_, AppState>) -> Result<StorageReport, String> {
    Ok(scan(&state).await?.report)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CleanupResult {
    pub removed: Vec<String>,
    /// "what - why" for each thing left in place.
    pub kept: Vec<String>,
    pub freed_bytes: u64,
}

async fn remove(removal: &Removal, protected: &[PathBuf]) -> Result<(), String> {
    match removal {
        Removal::Volume(name) => ls_containers::volume_remove_unused(name).await.map_err(|e| format!("{e:#}")),
        Removal::Folder { dir, work_dir } => {
            check_removable_dir(dir, work_dir, protected)?;
            std::fs::remove_dir_all(dir).map_err(|e| e.to_string())
        }
        Removal::TestRun { dir, base } => {
            let ok = is_real_dir(dir) && canon(dir).parent() == Some(canon(base).as_path())
                && dir.file_name().is_some_and(|n| is_stale_test_run(&n.to_string_lossy()));
            if !ok {
                return Err("not a leftover test-run folder".into());
            }
            std::fs::remove_dir_all(dir).map_err(|e| e.to_string())
        }
        Removal::Export { file, base } => {
            let ok = is_real_file(file) && canon(file).parent() == Some(canon(base).as_path())
                && file.file_name().is_some_and(|n| is_export_name(&n.to_string_lossy()));
            if !ok {
                return Err("not an exported dump".into());
            }
            std::fs::remove_file(file).map_err(|e| e.to_string())
        }
    }
}

/// Removes the unused items the person confirmed (`expected`: ids from a
/// report). Everything is scanned again first and only an item that is
/// *still* unused now is removed - the list is the person's consent, not
/// the source of truth.
#[tauri::command]
pub async fn clean_up_unused(state: State<'_, AppState>, expected: Vec<String>) -> Result<CleanupResult, String> {
    clean_up_unused_inner(&state, expected).await
}

pub async fn clean_up_unused_inner(state: &AppState, expected: Vec<String>) -> Result<CleanupResult, String> {
    let scan = scan(state).await?;
    let mut out = CleanupResult::default();
    for id in expected {
        let Some((removal, bytes)) = scan.removals.get(&id) else {
            out.kept.push(format!("{id} - no longer unused"));
            continue;
        };
        match remove(removal, &scan.protected).await {
            Ok(()) => {
                out.removed.push(id);
                out.freed_bytes += bytes;
            }
            Err(e) => out.kept.push(format!("{id} - {e}")),
        }
    }
    Ok(out)
}

/// "Free up disk" for one received session: stops its containers (the UI
/// warns first when it's running), then removes its database volume(s) and
/// unpacked folders - except any another listed session also uses. The
/// session stays in the list; running it again needs a fresh receive.
#[tauri::command]
pub async fn free_session_disk(state: State<'_, AppState>, session_id: String) -> Result<CleanupResult, String> {
    free_session_disk_inner(&state, &session_id).await
}

pub async fn free_session_disk_inner(state: &AppState, session_id: &str) -> Result<CleanupResult, String> {
    let listed = listed(state)?;
    let session = listed.received.iter().find(|s| s.id == session_id).cloned().ok_or_else(|| format!("no received session with id {session_id}"))?;
    crate::receiver_session_commands::stop_session_containers(state, &session).await?;

    let work_dirs = known_work_dirs(&listed);
    let mine = session_dirs(&session, &work_dirs);
    let my_volumes = volumes_of(&mine);
    let others: Vec<&ReceivedSession> = listed.received.iter().filter(|s| s.id != session_id).collect();
    let other_dirs: Vec<PathBuf> = others.iter().flat_map(|s| session_dirs(s, &work_dirs)).collect();
    let other_names: HashSet<String> = others.iter().flat_map(|s| session_names(s)).collect();
    let other_volumes = volumes_of(&other_dirs);
    let live: Vec<PathBuf> = state.sessions.lock().map_err(|e| e.to_string())?.values().filter_map(|r| unpack_root_of(&r.compose_dir)).collect();
    let sizes: HashMap<String, u64> = ls_containers::volume_usage().await.unwrap_or_default().into_iter().map(|v| (v.name, v.bytes)).collect();

    let mut out = CleanupResult::default();
    for dir in &mine {
        let shown = dir.display().to_string();
        let shared = other_dirs.iter().chain(&live).any(|o| canon(o) == canon(dir))
            || dir.file_name().and_then(|n| n.to_str()).is_some_and(|n| other_names.contains(n));
        if shared {
            out.kept.push(format!("{shown} - another session uses it"));
            continue;
        }
        let Some(work_dir) = dir.parent().filter(|p| work_dirs.iter().any(|w| canon(w) == canon(p))) else {
            out.kept.push(format!("{shown} - not inside a known work folder"));
            continue;
        };
        let bytes = dir_size(dir);
        match check_removable_dir(dir, work_dir, &listed.protected).and_then(|()| std::fs::remove_dir_all(dir).map_err(|e| e.to_string())) {
            Ok(()) => {
                out.removed.push(shown);
                out.freed_bytes += bytes;
            }
            Err(e) => out.kept.push(format!("{shown} - {e}")),
        }
    }
    for v in &my_volumes {
        if other_volumes.contains(v) {
            out.kept.push(format!("{v} - another session uses it"));
            continue;
        }
        match ls_containers::volume_remove_unused(v).await {
            Ok(()) => {
                out.removed.push(v.clone());
                out.freed_bytes += sizes.get(v).copied().unwrap_or(0);
            }
            Err(e) => out.kept.push(format!("{v} - {e:#}")),
        }
    }

    // Its recorded folders are gone: forget them, so nothing tries to run or
    // stop from a folder that no longer exists.
    let gone = |d: &str| !d.is_empty() && !Path::new(d).exists();
    let mut updated = session.clone();
    if gone(&updated.compose_dir) {
        updated.compose_dir.clear();
    }
    if updated.previous_run.as_ref().is_some_and(|(d, _)| gone(d)) {
        updated.previous_run = None;
    }
    if updated != session {
        if let Some(s) = state.received_sessions.lock().map_err(|e| e.to_string())?.get_mut(session_id) {
            *s = updated.clone();
        }
        crate::session_history::save_received(&updated)?;
    }
    Ok(out)
}

/// Removes LocalSync-built images (`localhost/localsync-*`) no container
/// uses - `podman rmi` without `-f`. They're rebuilt on the next Run. The
/// shared base images (python, mysql, ...) are never touched.
#[tauri::command]
pub async fn prune_localsync_images() -> Result<CleanupResult, String> {
    let mut out = CleanupResult::default();
    for img in ls_containers::images().await.map_err(|e| format!("{e:#}"))? {
        if !is_localsync_image(&img) || img.containers > 0 {
            continue;
        }
        let name = img.names.first().cloned().unwrap_or_else(|| img.id.clone());
        match ls_containers::image_remove(&img.id).await {
            Ok(()) => {
                out.removed.push(name);
                out.freed_bytes += img.bytes;
            }
            Err(e) => out.kept.push(format!("{name} - {e:#}")),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const C40: &str = "0123456789abcdef0123456789abcdef01234567";

    /// A pre-marker unpack folder, single-folder (nested) send layout.
    fn legacy_unpack(work: &Path, name: &str) -> PathBuf {
        let d = work.join(name);
        let label = d.join("myapp");
        std::fs::create_dir_all(label.join("source")).unwrap();
        std::fs::write(label.join("diff_stat.json"), "[]").unwrap();
        std::fs::write(label.join("diff.patch"), "").unwrap();
        std::fs::write(label.join("docker-compose.yml"), "services: {}\n").unwrap();
        d
    }

    fn marked_unpack(work: &Path, name: &str) -> PathBuf {
        let d = work.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(ls_containers::UNPACK_MARKER), "{}").unwrap();
        d
    }

    #[test]
    fn unpack_names_need_a_full_commit_hash() {
        assert!(looks_like_unpack_name(&format!("xusom-admin-{C40}")));
        assert!(looks_like_unpack_name(&format!("p-{C40}{}", &C40[..24])), "sha256 repos");
        assert!(!looks_like_unpack_name("xusom-admin-deadbeef"));
        assert!(!looks_like_unpack_name(&format!("-{C40}")));
        assert!(!looks_like_unpack_name(&format!("my project-{C40}")));
        assert!(!looks_like_unpack_name(&format!("p-{}", C40.to_uppercase())));
        assert!(!looks_like_unpack_name("Documents"));
    }

    #[test]
    fn recognizes_marked_and_legacy_unpack_dirs_only() {
        let work = tempfile::tempdir().unwrap();
        assert!(is_localsync_unpack_dir(&marked_unpack(work.path(), &format!("a-{C40}"))));
        assert!(is_localsync_unpack_dir(&legacy_unpack(work.path(), &format!("b-{C40}"))));

        // Right name, wrong contents: someone's own folder.
        let own = work.path().join(format!("c-{C40}"));
        std::fs::create_dir_all(own.join("src")).unwrap();
        std::fs::write(own.join("README.md"), "mine").unwrap();
        assert!(!is_localsync_unpack_dir(&own));

        // Right contents, wrong name.
        let renamed = legacy_unpack(work.path(), "my-copy");
        assert!(!is_localsync_unpack_dir(&renamed));
        let marked_wrong_name = marked_unpack(work.path(), "notes");
        assert!(!is_localsync_unpack_dir(&marked_wrong_name));

        // The marker as a directory doesn't count.
        let fake = work.path().join(format!("d-{C40}"));
        std::fs::create_dir_all(fake.join(ls_containers::UNPACK_MARKER)).unwrap();
        assert!(!is_localsync_unpack_dir(&fake));
    }

    #[test]
    fn unpack_root_of_finds_the_folder_above_a_nested_compose_dir() {
        let work = tempfile::tempdir().unwrap();
        let d = legacy_unpack(work.path(), &format!("a-{C40}"));
        assert_eq!(unpack_root_of(&d.join("myapp")), Some(d.clone()));
        assert_eq!(unpack_root_of(&d), Some(d));
        assert_eq!(unpack_root_of(work.path()), None);
    }

    #[test]
    fn removable_only_directly_inside_the_work_dir() {
        let work = tempfile::tempdir().unwrap();
        let d = marked_unpack(work.path(), &format!("a-{C40}"));
        assert_eq!(check_removable_dir(&d, work.path(), &[]), Ok(()));

        // Deeper than one level, or checked against another work dir.
        let deeper = marked_unpack(&work.path().join("sub"), &format!("b-{C40}"));
        assert!(check_removable_dir(&deeper, work.path(), &[]).is_err());
        let other = tempfile::tempdir().unwrap();
        assert!(check_removable_dir(&d, other.path(), &[]).is_err());
        // The work dir itself is never "inside" itself.
        assert!(check_removable_dir(work.path(), work.path(), &[]).is_err());
        // Not LocalSync's.
        let own = work.path().join(format!("c-{C40}"));
        std::fs::create_dir_all(&own).unwrap();
        assert!(check_removable_dir(&own, work.path(), &[]).is_err());
    }

    #[test]
    fn a_sender_project_folder_is_never_removable_inside_containing_or_equal() {
        let work = tempfile::tempdir().unwrap();
        let d = legacy_unpack(work.path(), &format!("a-{C40}"));
        // The sender's project is the folder itself, is inside it, or contains it.
        for protected in [d.clone(), d.join("myapp"), work.path().to_path_buf()] {
            let err = check_removable_dir(&d, work.path(), &[protected.clone()]).unwrap_err();
            assert!(err.contains("sender project"), "{protected:?}: {err}");
        }
        // An unrelated sender folder doesn't block it.
        let elsewhere = tempfile::tempdir().unwrap();
        assert_eq!(check_removable_dir(&d, work.path(), &[elsewhere.path().to_path_buf()]), Ok(()));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed_for_size_or_removal() {
        let work = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let precious = marked_unpack(outside.path(), &format!("x-{C40}"));
        std::fs::write(precious.join("big.bin"), vec![0u8; 10_000]).unwrap();
        let link = work.path().join(format!("x-{C40}"));
        std::os::unix::fs::symlink(&precious, &link).unwrap();
        assert!(!is_localsync_unpack_dir(&link));
        assert!(check_removable_dir(&link, work.path(), &[]).is_err());

        // A link inside a real unpack folder isn't counted.
        let d = marked_unpack(work.path(), &format!("a-{C40}"));
        std::fs::write(d.join("f"), vec![0u8; 100]).unwrap();
        std::os::unix::fs::symlink(&precious, d.join("link")).unwrap();
        let marker = std::fs::metadata(d.join(ls_containers::UNPACK_MARKER)).unwrap().len();
        assert_eq!(dir_size(&d), 100 + marker);
    }

    #[test]
    fn session_dirs_claim_by_compose_dir_and_by_version_name() {
        let work = tempfile::tempdir().unwrap();
        let mut s = ReceivedSession::new("r1".into(), "myapp".into(), "k".into(), format!("myapp@{C40}"), C40.into(), "t".into());
        // Not run yet, but a Run in progress already unpacked its folder.
        let current = marked_unpack(work.path(), &ls_containers::unpack_dir_name("myapp", C40));
        let work_dirs = vec![work.path().to_path_buf()];
        assert_eq!(session_dirs(&s, &work_dirs), vec![current.clone()]);
        // An older version it ran, elsewhere, by compose dir.
        let old_commit = "1111111111111111111111111111111111111111";
        let elsewhere = tempfile::tempdir().unwrap();
        let old = legacy_unpack(elsewhere.path(), &format!("myapp-{old_commit}"));
        s.previous_run = Some((old.join("myapp").display().to_string(), old_commit.into()));
        let got = session_dirs(&s, &work_dirs);
        assert!(got.contains(&current) && got.contains(&old), "{got:?}");
        // Someone else's project folder of the same name isn't claimed.
        let unrelated = work.path().join(ls_containers::unpack_dir_name("other", C40));
        std::fs::create_dir_all(&unrelated).unwrap();
        assert!(!session_dirs(&s, &work_dirs).contains(&unrelated));
    }

    #[test]
    fn volumes_come_from_the_rewritten_compose_file() {
        let work = tempfile::tempdir().unwrap();
        let d = legacy_unpack(work.path(), &format!("a-{C40}"));
        std::fs::write(
            d.join("myapp/docker-compose.yml"),
            "services:\n  db:\n    image: mysql:8\nvolumes:\n  db-data:\n    name: localsync-db-abc\n  cache:\n    name: someone-elses\n",
        )
        .unwrap();
        assert_eq!(volumes_of(&[d]), vec!["localsync-db-abc".to_string()]);
    }

    #[test]
    fn only_localsync_volumes_and_own_leftovers_qualify() {
        assert!(is_localsync_volume("localsync-db-f61aa5c2"));
        assert!(!is_localsync_volume("localsync-db-"));
        assert!(!is_localsync_volume("mysql-data"));
        assert!(!is_localsync_volume("localsync-db-../x"));
        assert!(is_stale_test_run("1-123"));
        assert!(!is_stale_test_run(&format!("{}-123", std::process::id())), "this process's own run may be live");
        assert!(!is_stale_test_run("abc-123"));
        assert!(is_export_name(&format!("{}.sql", "a".repeat(64))));
        assert!(is_export_name(&format!("{}.tar.gz", "0".repeat(64))));
        assert!(!is_export_name("dump.sql"));
    }
}
