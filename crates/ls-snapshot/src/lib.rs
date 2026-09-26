mod bundle;
mod hash;
mod sign;
mod types;

pub use bundle::head_commit;
pub use types::{DatabaseDumpEntry, DumpSource, FolderInfo, Manifest, PendingDump, ServiceDef, Snapshot};

use anyhow::{Context, Result};
use std::fs::File;
use std::path::Path;

/// Bundles `project_root` (git-tracked files at HEAD, plus a diff against
/// `parent_commit` if given, docker-compose.yml, and db-seed/) into a signed,
/// tar.gz'd `Snapshot` ready to hand to ls-net for transport.
pub fn create_snapshot(project_root: &Path, parent_commit: Option<&str>) -> Result<Snapshot> {
    let bundle = bundle::bundle_project(project_root, parent_commit)
        .context("bundling project")?;

    // A lockfile usually lives next to its service's Dockerfile (e.g.
    // sample-project/app/pom.xml for a compose service built from ./app),
    // not at the compose root, so check each service's build dir too.
    let build_dirs: Vec<std::path::PathBuf> = bundle
        .services
        .iter()
        .filter_map(|s| s.image_or_build.strip_prefix("build:"))
        .map(|rel| project_root.join(rel))
        .collect();
    let dependency_lock_hash = hash::dependency_lock_hash(project_root, &build_dirs)?;
    let db_seed_hash = hash::db_seed_hash(project_root)?;

    let project_name = project_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string());

    log::info!("signing started");
    let identity = sign::load_or_create_identity().context("loading sender identity")?;

    let manifest = Manifest {
        project_name,
        git_commit: bundle.git_commit,
        git_parent_commit: parent_commit.map(str::to_string),
        dependency_lock_hash,
        db_seed_hash,
        services: bundle.services,
        sender_pubkey: identity.verifying_key().to_bytes(),
        created_at: time::OffsetDateTime::now_utc(),
        // Round 17's multi-folder/database-dump breakdown - this function is
        // the original single-folder path, unchanged in every other respect,
        // and never populates either: see Manifest::folders' doc comment.
        folders: Vec::new(),
        database_dumps: Vec::new(),
    };

    let signature = sign::sign_manifest(&identity, &manifest, &bundle.payload)?;
    log::info!("signing done");

    Ok(Snapshot {
        manifest,
        signature,
        payload: bundle.payload,
    })
}

/// One folder the round-17 Send wizard is packaging into a multi-folder
/// snapshot, paired with the same optional "diff against a prior commit"
/// input `create_snapshot` already takes for a single folder — `None` means
/// "everything at HEAD is new", matching a first-time send.
pub struct FolderSpec {
    pub path: std::path::PathBuf,
    pub parent_commit: Option<String>,
}

/// Computes each folder's tar/manifest label: its basename, with a numeric
/// suffix (`-2`, `-3`, ...) appended to any later folder that collides with
/// an earlier one's basename or a still-earlier de-duplicated label — two
/// selected folders can easily share a basename (e.g. sibling checkouts
/// both named `app`), and both the manifest's `FolderInfo.name` and the
/// payload tar's path prefix need to be unique or later folders would
/// silently overwrite earlier ones' entries.
fn unique_folder_labels(folders: &[FolderSpec]) -> Vec<String> {
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut labels = Vec::with_capacity(folders.len());
    for f in folders {
        let base = f
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".to_string());
        let mut candidate = base.clone();
        let mut n = 2;
        while used.contains(&candidate) {
            candidate = format!("{base}-{n}");
            n += 1;
        }
        used.insert(candidate.clone());
        labels.push(candidate);
    }
    labels
}

/// Round 18: the file extension a dump's manifest path and its tar entry
/// both use, keyed by engine - a plain `.sql` text dump for MySQL/
/// PostgreSQL (both produce restorable plain SQL, just via different
/// tools - see `ls_dbsource::export`'s own doc comment), `.tar.gz` for
/// MongoDB (a tarred directory of `mongodump`'s own BSON output - not SQL
/// at all, restored with `mongorestore`, never a SQL-style restore).
/// Falls back to `.sql` for anything else (round-17 manifests never
/// recorded an engine at all - see `default_dump_engine` in types.rs - and
/// treating an unrecognized value as SQL-shaped is the safer default over
/// silently misnaming a file receivers otherwise wouldn't be able to open
/// as anything, hence "sql" and not e.g. "dump").
fn dump_file_extension(engine: &str) -> &'static str {
    match engine {
        "mongodb" => "tar.gz",
        _ => "sql",
    }
}

/// Round 17: bundles one or more independent project folders — the
/// developer's real case: several standalone Spring Boot folders with no
/// shared docker-compose.yml — plus any database dumps the Send wizard
/// collected for them, into one signed, multi-folder `Snapshot`.
///
/// Each folder is bundled with the same, unmodified, already-tested
/// `bundle::bundle_project` used by the original single-folder
/// `create_snapshot`, then combined via `bundle::merge_folder_payloads` (see
/// its doc comment for why this project chose re-tar-ing over reworking
/// `bundle_project` itself). `dependency_lock_hash`/`db_seed_hash` become
/// the combined hash (`hash::combine_hex_hashes`) across every folder's own
/// value, in folder order — still a single, deterministic value keying the
/// receiver-side build/DB-volume cache, now covering every folder's lockfile
/// and seed data rather than just one.
///
/// `project_name`/top-level `git_commit`/`git_parent_commit` stay populated
/// (the first folder's values, and folder names joined with "+") purely so
/// anything reading only those three top-level fields keeps working — the
/// authoritative, complete per-folder breakdown is `Manifest::folders`.
///
/// This function does not, and is not meant to, make the receiver able to
/// actually *run* several raw, non-containerized folders together — that's
/// deliberately out of scope for round 17 (see the round's own report);
/// `services` here is simply the concatenation of whatever docker-compose
/// services (if any) each individual folder's own `bundle_project` found,
/// same as it would find for any single folder today.
pub fn create_snapshot_multi(folders: &[FolderSpec], dumps: &[types::PendingDump]) -> Result<Snapshot> {
    anyhow::ensure!(!folders.is_empty(), "create_snapshot_multi requires at least one folder");
    for d in dumps {
        anyhow::ensure!(
            d.folder_index < folders.len(),
            "PendingDump.folder_index {} is out of range for {} folder(s)",
            d.folder_index,
            folders.len()
        );
    }

    let labels = unique_folder_labels(folders);

    let mut bundles = Vec::with_capacity(folders.len());
    let mut folder_infos = Vec::with_capacity(folders.len());
    let mut lock_hashes = Vec::with_capacity(folders.len());
    let mut seed_hashes = Vec::with_capacity(folders.len());
    let mut services = Vec::new();

    for (f, label) in folders.iter().zip(&labels) {
        let bundle = bundle::bundle_project(&f.path, f.parent_commit.as_deref())
            .with_context(|| format!("bundling {}", f.path.display()))?;

        let build_dirs: Vec<std::path::PathBuf> = bundle
            .services
            .iter()
            .filter_map(|s| s.image_or_build.strip_prefix("build:"))
            .map(|rel| f.path.join(rel))
            .collect();
        lock_hashes.push(hash::dependency_lock_hash(&f.path, &build_dirs)?);
        seed_hashes.push(hash::db_seed_hash(&f.path)?);

        folder_infos.push(types::FolderInfo {
            name: label.clone(),
            git_commit: bundle.git_commit.clone(),
            git_parent_commit: f.parent_commit.clone(),
        });
        services.extend(bundle.services.clone());
        bundles.push((label.clone(), bundle));
    }

    let dependency_lock_hash = hash::combine_hex_hashes(&lock_hashes);
    let db_seed_hash = hash::combine_hex_hashes(&seed_hashes);

    // Round: no `.clone()` of the dump content here — `dump_tuples` borrows
    // each `PendingDump`'s `DumpSource` (either already-in-memory `Bytes`, or
    // a `FilePath` left unread until `merge_folder_payloads` streams it
    // straight into the tar builder). Cloning here used to mean two full
    // copies of a real, multi-GB dump alive in memory at once; this is the
    // fix for that specific redundant copy.
    let dump_tuples: Vec<(String, String, &types::DumpSource)> = dumps
        .iter()
        .map(|d| {
            let file_name = format!("{}.{}", d.schema, dump_file_extension(&d.engine));
            (labels[d.folder_index].clone(), file_name, &d.source)
        })
        .collect();
    let mut database_dumps: Vec<types::DatabaseDumpEntry> = Vec::with_capacity(dumps.len());
    for d in dumps {
        // Hashed via `sha256_hex_of_file` for a `FilePath` source, which
        // reads the file in bounded chunks rather than pulling the whole
        // dump into memory just to hash it — the other half of not needing
        // `d.dump_bytes.clone()`'s old in-memory copy.
        let hash = match &d.source {
            types::DumpSource::Bytes(bytes) => {
                use sha2::{Digest, Sha256};
                Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
            }
            types::DumpSource::FilePath(path) => hash::sha256_hex_of_file(path)
                .with_context(|| format!("hashing dump file {}", path.display()))?,
        };
        database_dumps.push(types::DatabaseDumpEntry {
            folder: labels[d.folder_index].clone(),
            schema: d.schema.clone(),
            dump_file: format!(
                "db-dumps/{}/{}.{}",
                labels[d.folder_index],
                d.schema,
                dump_file_extension(&d.engine)
            ),
            hash,
            engine: d.engine.clone(),
        });
    }

    let payload = bundle::merge_folder_payloads(&bundles, &dump_tuples)?;

    let project_name = labels.join("+");

    log::info!("signing started (multi-folder, {} folder(s))", folders.len());
    let identity = sign::load_or_create_identity().context("loading sender identity")?;

    let manifest = Manifest {
        project_name,
        git_commit: folder_infos[0].git_commit.clone(),
        git_parent_commit: folder_infos[0].git_parent_commit.clone(),
        dependency_lock_hash,
        db_seed_hash,
        services,
        sender_pubkey: identity.verifying_key().to_bytes(),
        created_at: time::OffsetDateTime::now_utc(),
        folders: folder_infos,
        database_dumps,
    };

    let signature = sign::sign_manifest(&identity, &manifest, &payload)?;
    log::info!("signing done (multi-folder)");

    Ok(Snapshot {
        manifest,
        signature,
        payload,
    })
}

/// Persists a `Snapshot` to disk as JSON. `payload` (the tar.gz bytes) rides
/// along as a JSON byte array — simplest thing that works with the deps
/// already in this crate.
/// ponytail: JSON-encodes payload bytes as a number array (~4-5x size
/// overhead vs raw bytes). Fine for MVP-sized demo snapshots; switch to
/// bincode or base64 if payloads grow past a few MB.
pub fn save_to_file(snapshot: &Snapshot, path: &Path) -> Result<()> {
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    serde_json::to_writer(file, snapshot).context("writing snapshot")
}

pub fn load_from_file(path: &Path) -> Result<Snapshot> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    serde_json::from_reader(file).context("parsing snapshot")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::DiffStatEntry;
    use crate::{hash, sign};
    use std::collections::HashMap;
    use std::fs;
    use std::io::Read;
    use std::process::Command;
    use tempfile::tempdir;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("failed to run git");
        assert!(status.success(), "git {args:?} failed");
    }

    fn git_output(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("failed to run git");
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn commit_all(dir: &Path, message: &str) {
        git(dir, &["add", "-A"]);
        git(
            dir,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                message,
            ],
        );
    }

    fn unpack(payload: &[u8]) -> HashMap<String, Vec<u8>> {
        let decoder = zstd::stream::read::Decoder::new(payload).unwrap();
        let mut archive = tar::Archive::new(decoder);
        let mut out = HashMap::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().replace('\\', "/");
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).unwrap();
            out.insert(path, buf);
        }
        out
    }

    #[test]
    fn create_snapshot_round_trips_and_tracks_a_modification() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        git(root, &["init"]);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("README.md"), "hello\n").unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(
            root.join("docker-compose.yml"),
            "services:\n  web:\n    image: nginx:latest\n    ports:\n      - \"8080:80\"\n    depends_on:\n      - db\n  db:\n    image: postgres:15\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("db-seed")).unwrap();
        fs::write(root.join("db-seed/seed.sql"), "insert into t values (1);\n").unwrap();
        commit_all(root, "init");
        let first_commit = git_output(root, &["rev-parse", "HEAD"]);

        let snap1 = create_snapshot(root, None).unwrap();
        assert_eq!(snap1.manifest.git_commit, first_commit);
        assert!(snap1.manifest.git_parent_commit.is_none());
        assert!(sign::verify_signature(&snap1.manifest, &snap1.payload, &snap1.signature).unwrap());
        assert_eq!(snap1.manifest.dependency_lock_hash, hash::dependency_lock_hash(root, &[]).unwrap());
        assert_eq!(snap1.manifest.db_seed_hash, hash::db_seed_hash(root).unwrap());

        let mut services = snap1.manifest.services.clone();
        services.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(services.len(), 2);
        assert_eq!(services[0].name, "db");
        assert_eq!(services[0].image_or_build, "image:postgres:15");
        assert_eq!(services[1].name, "web");
        assert_eq!(services[1].image_or_build, "image:nginx:latest");
        assert_eq!(services[1].ports, vec!["8080:80".to_string()]);
        assert_eq!(services[1].depends_on, vec!["db".to_string()]);

        let files1 = unpack(&snap1.payload);
        assert_eq!(files1.get("source/README.md").unwrap().as_slice(), b"hello\n");
        assert_eq!(files1.get("source/src/main.rs").unwrap().as_slice(), b"fn main() {}\n");
        assert_eq!(
            files1.get("docker-compose.yml").unwrap(),
            &fs::read(root.join("docker-compose.yml")).unwrap()
        );
        assert_eq!(
            files1.get("db-seed/seed.sql").unwrap(),
            &fs::read(root.join("db-seed/seed.sql")).unwrap()
        );
        assert_eq!(files1.get("diff.patch").unwrap().as_slice(), b"");

        let diff_stat1: Vec<DiffStatEntry> = serde_json::from_slice(files1.get("diff_stat.json").unwrap()).unwrap();
        let mut paths1: Vec<&str> = diff_stat1.iter().map(|e| e.path.as_str()).collect();
        paths1.sort();
        assert_eq!(paths1, vec!["README.md", "db-seed/seed.sql", "docker-compose.yml", "src/main.rs"]);
        assert!(diff_stat1.iter().all(|e| e.change_type == "added"));

        // --- modify one file and re-snapshot against the first commit ---
        fs::write(root.join("README.md"), "hello world\n").unwrap();
        commit_all(root, "update readme");
        let second_commit = git_output(root, &["rev-parse", "HEAD"]);
        assert_ne!(first_commit, second_commit);

        let snap2 = create_snapshot(root, Some(&first_commit)).unwrap();
        assert_eq!(snap2.manifest.git_commit, second_commit);
        assert_eq!(snap2.manifest.git_parent_commit.as_deref(), Some(first_commit.as_str()));
        assert!(sign::verify_signature(&snap2.manifest, &snap2.payload, &snap2.signature).unwrap());

        let files2 = unpack(&snap2.payload);
        assert_eq!(files2.get("source/README.md").unwrap().as_slice(), b"hello world\n");

        let patch = String::from_utf8(files2.get("diff.patch").unwrap().clone()).unwrap();
        assert!(!patch.is_empty());
        assert!(patch.contains("README.md"));

        let diff_stat2: Vec<DiffStatEntry> = serde_json::from_slice(files2.get("diff_stat.json").unwrap()).unwrap();
        assert_eq!(diff_stat2.len(), 1);
        assert_eq!(diff_stat2[0].path, "README.md");
        assert_eq!(diff_stat2[0].change_type, "modified");
        assert!(diff_stat2[0].insertions >= 1);
        assert!(diff_stat2[0].deletions >= 1);

        // save_to_file / load_from_file round-trip.
        let out_path = dir.path().join("snapshot.json");
        save_to_file(&snap2, &out_path).unwrap();
        let loaded = load_from_file(&out_path).unwrap();
        assert_eq!(loaded.manifest.git_commit, snap2.manifest.git_commit);
        assert_eq!(loaded.payload, snap2.payload);
        assert_eq!(loaded.signature, snap2.signature);
    }

    /// Baseline for the noise-directory denylist added below: git's own
    /// tracking already keeps an untracked, `.gitignore`'d `node_modules/`
    /// out of both `git archive HEAD` and a commit-to-commit `git diff` —
    /// it was never in a commit to begin with, regardless of `.gitignore`.
    /// A project with a working `.gitignore` in place before the first
    /// commit needs no denylist at all; this proves that empirically rather
    /// than assuming it.
    #[test]
    fn untracked_gitignored_node_modules_never_reaches_git_at_all() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        git(root, &["init"]);
        fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
        fs::write(root.join("README.md"), "hello\n").unwrap();
        fs::create_dir_all(root.join("node_modules/some-package")).unwrap();
        fs::write(root.join("node_modules/some-package/index.js"), "module.exports = {};\n").unwrap();
        // `git add -A` respects .gitignore: only README.md + .gitignore get staged.
        commit_all(root, "init");

        let snap = create_snapshot(root, None).unwrap();
        let files = unpack(&snap.payload);
        assert!(files.contains_key("source/README.md"));
        assert!(
            files.keys().all(|k| !k.contains("node_modules")),
            "an untracked, gitignored node_modules/ should never reach the payload: {:?}",
            files.keys().collect::<Vec<_>>()
        );

        let diff_stat: Vec<DiffStatEntry> = serde_json::from_slice(files.get("diff_stat.json").unwrap()).unwrap();
        assert!(diff_stat.iter().all(|e| !e.path.contains("node_modules")));
    }

    /// The real gap: a project where node_modules/ (and app/target/) got
    /// genuinely *committed* — no .gitignore was ever set up. Git's own
    /// history offers no protection here; this proves bundle.rs's
    /// NOISE_DIR_NAMES denylist filters them out anyway, from the initial
    /// payload, diff_stat.json, and diff.patch alike.
    #[test]
    fn accidentally_committed_noise_dirs_are_filtered_by_the_denylist() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        git(root, &["init"]);
        fs::write(root.join("README.md"), "hello\n").unwrap();
        fs::create_dir_all(root.join("node_modules/some-package")).unwrap();
        fs::write(root.join("node_modules/some-package/index.js"), "module.exports = {};\n").unwrap();
        fs::create_dir_all(root.join("app/target/classes")).unwrap();
        fs::write(root.join("app/target/classes/Main.class"), [0xCAu8, 0xFE, 0xBA, 0xBE]).unwrap();
        commit_all(root, "init (accidentally includes node_modules and target)");
        let first_commit = git_output(root, &["rev-parse", "HEAD"]);

        let snap1 = create_snapshot(root, None).unwrap();
        let files1 = unpack(&snap1.payload);
        assert!(files1.contains_key("source/README.md"));
        assert!(
            files1.keys().all(|k| !k.contains("node_modules") && !k.contains("/target/")),
            "tracked noise dirs must still be filtered out of the payload: {:?}",
            files1.keys().collect::<Vec<_>>()
        );
        let diff_stat1: Vec<DiffStatEntry> = serde_json::from_slice(files1.get("diff_stat.json").unwrap()).unwrap();
        assert!(diff_stat1.iter().all(|e| !e.path.contains("node_modules") && !e.path.contains("target/")));
        assert!(diff_stat1.iter().any(|e| e.path == "README.md"));

        // Modify a real file *and* a (tracked) noise file, then diff.
        fs::write(root.join("README.md"), "hello world\n").unwrap();
        fs::write(root.join("node_modules/some-package/index.js"), "module.exports = { v: 2 };\n").unwrap();
        commit_all(root, "update readme and node_modules");

        let snap2 = create_snapshot(root, Some(&first_commit)).unwrap();
        let files2 = unpack(&snap2.payload);
        let diff_stat2: Vec<DiffStatEntry> = serde_json::from_slice(files2.get("diff_stat.json").unwrap()).unwrap();
        assert_eq!(
            diff_stat2.len(),
            1,
            "only README.md should show up; the node_modules change must be filtered: {diff_stat2:?}"
        );
        assert_eq!(diff_stat2[0].path, "README.md");

        let patch = String::from_utf8(files2.get("diff.patch").unwrap().clone()).unwrap();
        assert!(patch.contains("README.md"));
        assert!(!patch.contains("node_modules"), "diff.patch must not mention the filtered noise dir");
    }

    /// Real repro of the field report, against the actual sample-project-node
    /// fixture: `npm install` for real (so app/node_modules/ has genuine
    /// content, not a stand-in), then force it into a fresh commit exactly
    /// like an accidental `git add -A` with no .gitignore ever set up would.
    /// The payload/diff must not contain any of it despite it being tracked.
    #[test]
    fn real_sample_project_node_with_committed_node_modules_is_filtered() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sample-project-node");
        if !repo_root.is_dir() {
            eprintln!("sample-project-node not found at {} — skipping", repo_root.display());
            return;
        }

        let dir = tempdir().unwrap();
        let project_dir = dir.path().join("sample-project-node");
        fs::create_dir(&project_dir).unwrap();
        let cp_status = Command::new("cp")
            .args(["-r", &format!("{}/.", repo_root.display()), &project_dir.display().to_string()])
            .status()
            .expect("cp should be available on the test platform (WSL2/macOS/Linux)");
        assert!(cp_status.success());

        let npm_status = Command::new("npm")
            .args(["install", "--no-audit", "--no-fund"])
            .current_dir(project_dir.join("app"))
            .status()
            .expect("npm should be on PATH for this test");
        assert!(npm_status.success(), "npm install failed");
        assert!(project_dir.join("app/node_modules").is_dir(), "npm install should have created app/node_modules");

        git(&project_dir, &["init"]);
        // No .gitignore in this copy at all — same shape as the real bug:
        // node_modules/ gets tracked for real.
        commit_all(&project_dir, "sample-project-node with node_modules committed");

        let snap = create_snapshot(&project_dir, None).unwrap();
        let files = unpack(&snap.payload);
        assert!(files.contains_key("source/app/index.js"));
        let noisy: Vec<&String> = files.keys().filter(|k| k.contains("node_modules")).collect();
        assert!(
            noisy.is_empty(),
            "tracked app/node_modules/ must still be filtered out of the payload ({} noisy entries, e.g. {:?})",
            noisy.len(),
            noisy.iter().take(5).collect::<Vec<_>>()
        );

        let diff_stat: Vec<DiffStatEntry> = serde_json::from_slice(files.get("diff_stat.json").unwrap()).unwrap();
        assert!(diff_stat.iter().all(|e| !e.path.contains("node_modules")));
    }

    /// Same accidental-commit shape as the node_modules test above, against
    /// sample-project's Maven `app/target/` — proves the denylist isn't
    /// node_modules-specific.
    #[test]
    fn real_sample_project_with_committed_build_output_is_filtered() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sample-project");
        if !repo_root.is_dir() {
            eprintln!("sample-project not found at {} — skipping", repo_root.display());
            return;
        }

        let dir = tempdir().unwrap();
        let project_dir = dir.path().join("sample-project");
        fs::create_dir(&project_dir).unwrap();
        let cp_status = Command::new("cp")
            .args(["-r", &format!("{}/.", repo_root.display()), &project_dir.display().to_string()])
            .status()
            .expect("cp should be available on the test platform (WSL2/macOS/Linux)");
        assert!(cp_status.success());

        // Simulate a real Maven build having populated app/target/ before
        // the accidental `git add -A` that first tracked this project.
        fs::create_dir_all(project_dir.join("app/target/classes")).unwrap();
        fs::write(project_dir.join("app/target/classes/Main.class"), [0xCAu8, 0xFE, 0xBA, 0xBE]).unwrap();

        git(&project_dir, &["init"]);
        commit_all(&project_dir, "sample-project with target/ committed");

        let snap = create_snapshot(&project_dir, None).unwrap();
        let files = unpack(&snap.payload);
        assert!(files.contains_key("source/app/pom.xml"));
        let noisy: Vec<&String> = files.keys().filter(|k| k.contains("/target/")).collect();
        assert!(
            noisy.is_empty(),
            "tracked app/target/ must still be filtered out of the payload: {noisy:?}"
        );
    }

    /// Sets up a minimal real git repo at `dir/name`, with one committed
    /// `README.md` line identifying it, for `create_snapshot_multi` tests
    /// that need several independent folders rather than one.
    fn make_git_folder(dir: &Path, name: &str) -> std::path::PathBuf {
        let root = dir.join(name);
        fs::create_dir_all(&root).unwrap();
        git(&root, &["init"]);
        fs::write(root.join("README.md"), format!("{name}\n")).unwrap();
        commit_all(&root, "init");
        root
    }

    #[test]
    fn create_snapshot_multi_bundles_every_folder_under_its_own_prefix() {
        let dir = tempdir().unwrap();
        let a = make_git_folder(dir.path(), "orders-service");
        let b = make_git_folder(dir.path(), "billing-service");
        let a_commit = git_output(&a, &["rev-parse", "HEAD"]);
        let b_commit = git_output(&b, &["rev-parse", "HEAD"]);

        let snap = create_snapshot_multi(
            &[
                FolderSpec { path: a, parent_commit: None },
                FolderSpec { path: b, parent_commit: None },
            ],
            &[],
        )
        .unwrap();

        assert_eq!(snap.manifest.project_name, "orders-service+billing-service");
        // Top-level scalar fields stay populated with the *first* folder's
        // values, for anything that only reads those two.
        assert_eq!(snap.manifest.git_commit, a_commit);
        assert!(snap.manifest.git_parent_commit.is_none());

        assert_eq!(snap.manifest.folders.len(), 2);
        assert_eq!(snap.manifest.folders[0].name, "orders-service");
        assert_eq!(snap.manifest.folders[0].git_commit, a_commit);
        assert_eq!(snap.manifest.folders[1].name, "billing-service");
        assert_eq!(snap.manifest.folders[1].git_commit, b_commit);
        assert!(snap.manifest.database_dumps.is_empty());

        assert!(sign::verify_signature(&snap.manifest, &snap.payload, &snap.signature).unwrap());

        let files = unpack(&snap.payload);
        assert_eq!(
            files.get("orders-service/source/README.md").unwrap().as_slice(),
            b"orders-service\n"
        );
        assert_eq!(
            files.get("billing-service/source/README.md").unwrap().as_slice(),
            b"billing-service\n"
        );
        // Each folder's own diff_stat.json/diff.patch ship too, correctly
        // prefixed rather than one clobbering the other.
        assert!(files.contains_key("orders-service/diff_stat.json"));
        assert!(files.contains_key("billing-service/diff_stat.json"));
    }

    #[test]
    fn create_snapshot_multi_dedupes_colliding_folder_basenames() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("team-a")).unwrap();
        fs::create_dir_all(dir.path().join("team-b")).unwrap();
        let a = make_git_folder(&dir.path().join("team-a"), "app");
        let b = make_git_folder(&dir.path().join("team-b"), "app");

        let snap = create_snapshot_multi(
            &[
                FolderSpec { path: a, parent_commit: None },
                FolderSpec { path: b, parent_commit: None },
            ],
            &[],
        )
        .unwrap();

        assert_eq!(snap.manifest.folders[0].name, "app");
        assert_eq!(snap.manifest.folders[1].name, "app-2");
        assert_eq!(snap.manifest.project_name, "app+app-2");

        let files = unpack(&snap.payload);
        assert!(files.contains_key("app/source/README.md"));
        assert!(files.contains_key("app-2/source/README.md"));
    }

    #[test]
    fn create_snapshot_multi_packages_a_database_dump_for_the_right_folder() {
        let dir = tempdir().unwrap();
        let a = make_git_folder(dir.path(), "orders-service");
        let b = make_git_folder(dir.path(), "billing-service");
        let dump_bytes = b"-- Table: orders\nINSERT INTO `orders` (`id`) VALUES (1);\n".to_vec();

        let snap = create_snapshot_multi(
            &[
                FolderSpec { path: a, parent_commit: None },
                FolderSpec { path: b, parent_commit: None },
            ],
            &[PendingDump {
                folder_index: 0,
                schema: "orders_db".to_string(),
                source: DumpSource::Bytes(dump_bytes.clone()),
                engine: "mysql".to_string(),
            }],
        )
        .unwrap();

        assert_eq!(snap.manifest.database_dumps.len(), 1);
        let entry = &snap.manifest.database_dumps[0];
        assert_eq!(entry.folder, "orders-service");
        assert_eq!(entry.schema, "orders_db");
        assert_eq!(entry.dump_file, "db-dumps/orders-service/orders_db.sql");
        assert_eq!(entry.engine, "mysql");
        let expected_hash = {
            use sha2::{Digest, Sha256};
            Sha256::digest(&dump_bytes).iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        assert_eq!(entry.hash, expected_hash);

        let files = unpack(&snap.payload);
        assert_eq!(
            files.get("db-dumps/orders-service/orders_db.sql").unwrap().as_slice(),
            dump_bytes.as_slice()
        );
        // billing-service got no dump at all - not even an empty entry.
        assert!(!files.keys().any(|k| k.starts_with("db-dumps/billing-service")));
    }

    #[test]
    fn create_snapshot_multi_gives_a_mongodb_dump_a_tar_gz_extension_not_sql() {
        let dir = tempdir().unwrap();
        let a = make_git_folder(dir.path(), "catalog-service");
        let dump_bytes = b"pretend tarred mongodump bytes".to_vec();

        let snap = create_snapshot_multi(
            &[FolderSpec { path: a, parent_commit: None }],
            &[PendingDump {
                folder_index: 0,
                schema: "catalog_db".to_string(),
                source: DumpSource::Bytes(dump_bytes.clone()),
                engine: "mongodb".to_string(),
            }],
        )
        .unwrap();

        let entry = &snap.manifest.database_dumps[0];
        assert_eq!(entry.engine, "mongodb");
        assert_eq!(entry.dump_file, "db-dumps/catalog-service/catalog_db.tar.gz");

        let files = unpack(&snap.payload);
        assert_eq!(
            files.get("db-dumps/catalog-service/catalog_db.tar.gz").unwrap().as_slice(),
            dump_bytes.as_slice()
        );
    }

    #[test]
    fn create_snapshot_multi_rejects_an_empty_folder_list() {
        assert!(create_snapshot_multi(&[], &[]).is_err());
    }

    #[test]
    fn create_snapshot_multi_rejects_an_out_of_range_dump_folder_index() {
        let dir = tempdir().unwrap();
        let a = make_git_folder(dir.path(), "solo");
        let err = create_snapshot_multi(
            &[FolderSpec { path: a, parent_commit: None }],
            &[PendingDump { folder_index: 1, schema: "x".into(), source: DumpSource::Bytes(vec![]), engine: "mysql".into() }],
        )
        .unwrap_err();
        assert!(err.to_string().contains("out of range"));
    }
}
