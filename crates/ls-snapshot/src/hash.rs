use anyhow::Result;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

use crate::bundle::{resolve_db_seed_dir, walk_files};

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    to_hex(&Sha256::digest(bytes))
}

/// Lockfile priority tiers for the MVP: Maven, then Gradle, then npm as a
/// generic fallback. First tier with any file present wins (files within a
/// tier are concatenated in sorted order); no cascading to the next tier.
const LOCKFILE_TIERS: &[&[&str]] = &[
    &["pom.xml"],
    &["build.gradle", "build.gradle.kts"],
    &["package-lock.json"],
];

/// sha256 (lowercase hex) over the concatenated bytes of whichever lockfile(s)
/// exist, per `LOCKFILE_TIERS`. Checks `project_root` itself first, then each
/// of `extra_dirs` (a multi-service project's per-service build contexts,
/// e.g. `app/` for a compose service built from `./app` — a lockfile usually
/// lives next to its service's Dockerfile, not at the compose root). First
/// tier+dir with any file present wins. No lockfile anywhere hashes to
/// sha256("") rather than erroring — MVP projects may have none yet.
pub fn dependency_lock_hash(project_root: &Path, extra_dirs: &[std::path::PathBuf]) -> Result<String> {
    let mut search_dirs: Vec<&Path> = vec![project_root];
    search_dirs.extend(extra_dirs.iter().map(|p| p.as_path()));

    for tier in LOCKFILE_TIERS {
        for dir in &search_dirs {
            let mut present: Vec<&str> = tier
                .iter()
                .copied()
                .filter(|name| dir.join(name).is_file())
                .collect();
            if present.is_empty() {
                continue;
            }
            present.sort_unstable();
            let mut buf = Vec::new();
            for name in present {
                buf.extend(fs::read(dir.join(name))?);
            }
            return Ok(sha256_hex(&buf));
        }
    }
    Ok(sha256_hex(&[]))
}

/// sha256 (lowercase hex) over the concatenated bytes of every file under
/// `db-seed/` (or `db/seed/`), sorted by path for determinism. No such
/// directory hashes to sha256("").
pub fn db_seed_hash(project_root: &Path) -> Result<String> {
    let Some(dir) = resolve_db_seed_dir(project_root) else {
        return Ok(sha256_hex(&[]));
    };
    let mut buf = Vec::new();
    for file in walk_files(&dir)? {
        buf.extend(fs::read(&file)?);
    }
    Ok(sha256_hex(&buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn no_lockfile_no_seed_hashes_empty() {
        let dir = tempdir().unwrap();
        let empty = sha256_hex(&[]);
        assert_eq!(dependency_lock_hash(dir.path(), &[]).unwrap(), empty);
        assert_eq!(db_seed_hash(dir.path()).unwrap(), empty);
    }

    #[test]
    fn pom_xml_wins_over_lower_tiers() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pom.xml"), b"<project/>").unwrap();
        fs::write(dir.path().join("package-lock.json"), b"{}").unwrap();
        let expected = sha256_hex(b"<project/>");
        assert_eq!(dependency_lock_hash(dir.path(), &[]).unwrap(), expected);
    }

    #[test]
    fn falls_back_to_a_service_build_dir_for_the_lockfile() {
        let dir = tempdir().unwrap();
        let app_dir = dir.path().join("app");
        fs::create_dir_all(&app_dir).unwrap();
        fs::write(app_dir.join("pom.xml"), b"<project>app</project>").unwrap();
        let expected = sha256_hex(b"<project>app</project>");
        assert_eq!(
            dependency_lock_hash(dir.path(), &[app_dir]).unwrap(),
            expected
        );
    }

    #[test]
    fn db_seed_hash_covers_nested_files() {
        let dir = tempdir().unwrap();
        let seed = dir.path().join("db-seed").join("nested");
        fs::create_dir_all(&seed).unwrap();
        fs::write(seed.join("a.sql"), b"insert a").unwrap();
        fs::write(dir.path().join("db-seed").join("b.sql"), b"insert b").unwrap();
        // sorted order: db-seed/b.sql, db-seed/nested/a.sql
        let expected = sha256_hex(b"insert binsert a");
        assert_eq!(db_seed_hash(dir.path()).unwrap(), expected);
    }
}
