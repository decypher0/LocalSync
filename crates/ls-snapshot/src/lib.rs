mod bundle;
mod hash;
mod sign;
mod types;

pub use types::{Manifest, ServiceDef, Snapshot};

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
    };

    let signature = sign::sign_manifest(&identity, &manifest, &bundle.payload)?;
    log::info!("signing done");

    Ok(Snapshot {
        manifest,
        signature,
        payload: bundle.payload,
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
        let decoder = flate2::read::GzDecoder::new(payload);
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
}
