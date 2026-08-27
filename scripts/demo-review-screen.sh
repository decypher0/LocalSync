#!/usr/bin/env bash
# One-command shortcut to land directly on the "review this diff before you
# run it" screen, without a live P2P send/receive - for eyeballing that
# screen on a real display. Builds a snapshot from sample-project and starts
# the app with LOCALSYNC_PRELOAD_SNAPSHOT set; see apps/desktop/src-tauri/src/main.rs
# (preload_snapshot) and apps/desktop/src/app.js (the "preload-review" listener).
set -euo pipefail
cd "$(dirname "$0")/.."

WORK_DIR="$(mktemp -d)"

# create_snapshot requires its project_dir to be a git repo root of its own.
# sample-project/ is tracked inside this monorepo's repo, not as its own repo
# (same requirement documented in apps/desktop/src-tauri/tests/pipeline_test.rs's
# make_git_project_from) - so copy it out and git-init it before snapshotting.
PROJECT_DIR="$WORK_DIR/sample-project"
cp -r sample-project "$PROJECT_DIR"
git -C "$PROJECT_DIR" init -q
git -C "$PROJECT_DIR" add -A
git -C "$PROJECT_DIR" -c user.name=demo -c user.email=demo@localsync.dev commit -q -m "demo-review-screen snapshot"

SNAPSHOT_PATH="$WORK_DIR/preload-snapshot.json"
echo "Building snapshot from $PROJECT_DIR ..."
cargo run --example make_snapshot -p ls-snapshot -- "$PROJECT_DIR" "$SNAPSHOT_PATH"

echo
echo "Starting LocalSync preloaded straight to the review screen ..."
LOCALSYNC_PRELOAD_SNAPSHOT="$SNAPSHOT_PATH" cargo run -p localsync-desktop
