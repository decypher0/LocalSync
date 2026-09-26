// Tests for the receive side of the pure session model (session-model.js).
// Run with:
//   node --test apps/desktop/src/test-received-session-model.js
const test = require("node:test");
const assert = require("node:assert/strict");
const { receivedSessionFromView, applyReceivedView } = require("./session-model.js");

const view = (over = {}) => ({
  id: "r1",
  title: "xusom-admin",
  sender_pubkey_hex: "abc123",
  snapshot_id: "xusom-admin@def",
  git_commit: "def456",
  work_dir: "/tmp/localsync-work",
  created_at: "2026-01-01T00:00:00Z",
  last_received_at: "2026-01-02T00:00:00Z",
  saved: true,
  armed: false,
  running: false,
  ...over,
});

test("a reopened saved session resumes rather than re-reviews - no manifest/diff to hold", () => {
  const s = receivedSessionFromView(view());
  assert.equal(s.kind, "receive");
  assert.equal(s.id, "r1");
  assert.equal(s.status, "resuming");
  assert.equal(s.manifest, null);
  assert.equal(s.diff, null);
});

test("a saved session is always treated as already run at least once - that's why there was something to save", () => {
  const s = receivedSessionFromView(view());
  assert.equal(s.hasRunBefore, true);
});

test("the view's own running flag is kept only as a hint, separate from which panel renders", () => {
  const running = receivedSessionFromView(view({ running: true }));
  assert.equal(running.status, "resuming", "still resumes - there is no stoppable id to drive a running panel from");
  assert.equal(running.reportedRunning, true);

  const idle = receivedSessionFromView(view({ running: false }));
  assert.equal(idle.reportedRunning, false);
});

test("applying a new view replaces what the backend owns and keeps this side's runtime state", () => {
  const s = receivedSessionFromView(view());
  s.runLogText = "line one";
  s.busy = true;
  applyReceivedView(s, view({ armed: true, git_commit: "new-commit", snapshot_id: "xusom-admin@new" }));
  assert.equal(s.armed, true);
  assert.equal(s.gitCommit, "new-commit");
  assert.equal(s.snapshotId, "xusom-admin@new");
  assert.equal(s.runLogText, "line one", "runtime log/progress state lives only on this side and must survive a view refresh");
  assert.equal(s.busy, true);
});

test("a fresh push (session-update-available) re-uses applyReceivedView's own field ownership - title/senderPubkeyHex/saved/armed come from the view, nothing else", () => {
  const s = receivedSessionFromView(view());
  const before = { ...s };
  applyReceivedView(s, view({ title: "renamed", saved: false }));
  assert.equal(s.title, "renamed");
  assert.equal(s.saved, false);
  assert.equal(s.id, before.id, "id is never touched by a view refresh");
});
