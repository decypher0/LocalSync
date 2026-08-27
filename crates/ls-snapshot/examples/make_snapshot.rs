//! Standalone CLI: bundle a git-repo project directory into a signed
//! snapshot file on disk. Exists so a snapshot can be produced without a
//! live P2P send — `scripts/demo-review-screen.sh` uses this to preload the
//! desktop app straight to its review screen.
//!
//! Usage: cargo run --example make_snapshot -p ls-snapshot -- <project_dir> <output_path>
//!
//! `project_dir` must be a git repo root (same requirement `create_snapshot`
//! itself has — see `crates/ls-snapshot/src/bundle.rs`). It does not need to
//! be its own standalone repo at the filesystem level as long as git
//! resolves HEAD from that directory.

use std::path::PathBuf;
use std::process::exit;

fn main() {
    let mut args = std::env::args().skip(1);
    let (project_dir, output_path) = match (args.next(), args.next()) {
        (Some(p), Some(o)) => (PathBuf::from(p), PathBuf::from(o)),
        _ => {
            eprintln!("usage: make_snapshot <project_dir> <output_path>");
            exit(1);
        }
    };

    let snapshot = match ls_snapshot::create_snapshot(&project_dir, None) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "error: failed to create a snapshot from {} — is it a git repository with a commit? ({e:#})",
                project_dir.display()
            );
            exit(1);
        }
    };

    if let Err(e) = ls_snapshot::save_to_file(&snapshot, &output_path) {
        eprintln!("error: failed to write snapshot to {}: {e:#}", output_path.display());
        exit(1);
    }

    println!(
        "wrote snapshot for {}@{} to {}",
        snapshot.manifest.project_name,
        snapshot.manifest.git_commit,
        output_path.display()
    );
}
