// Tests for the pure part of the receiver's update flow (update-flow.js).
// Run with:
//   node --test apps/desktop/src/test-update-flow.js
const test = require("node:test");
const assert = require("node:assert/strict");
const UpdateFlow = require("./update-flow.js");

const diff = (n) => ({ entries: Array.from({ length: n }, (_, i) => ({ path: `f${i}` })) });
const session = (over = {}) => ({
  diff: diff(3),
  update: { db_changed: false, previous_commit: "abc", previous_running: true },
  updateDbChoice: null,
  updateBusy: null,
  updateError: "",
  runLogText: "",
  ...over,
});

test("the headline counts the files the update changes", () => {
  assert.equal(UpdateFlow.headline(diff(3)), "Update received (3 files changed)");
  assert.equal(UpdateFlow.headline(diff(1)), "Update received (1 file changed)");
  assert.equal(UpdateFlow.headline(null), "Update received (0 files changed)");
});

test("the database is asked about only when the sender's dump changed, and Keep is the default", () => {
  assert.equal(UpdateFlow.dbChoice({ db_changed: false }, "replace"), null, "an unchanged dump keeps the volume - nothing to ask");
  assert.equal(UpdateFlow.dbChoice({ db_changed: true }, null), "keep");
  assert.equal(UpdateFlow.dbChoice({ db_changed: true }, "keep"), "keep");
  assert.equal(UpdateFlow.dbChoice({ db_changed: true }, "replace"), "replace");
  assert.equal(UpdateFlow.dbChoice(null, "replace"), null);
});

test("no update, no panel", () => {
  assert.deepEqual(UpdateFlow.panelModel(session({ update: null })), { show: false });
  assert.deepEqual(UpdateFlow.panelModel(null), { show: false });
});

test("a running session restarts; a stopped one just runs", () => {
  const running = UpdateFlow.panelModel(session());
  assert.equal(running.show, true);
  assert.equal(running.headline, "Update received (3 files changed)");
  assert.equal(running.action, "Apply & restart");
  assert.match(running.note, /keeps running until you apply/);
  assert.equal(running.askDb, false);
  assert.equal(running.rollback, "Keep the current version", "before an apply, going back means declining it");

  const stopped = UpdateFlow.panelModel(session({ update: { db_changed: false, previous_running: false } }));
  assert.equal(stopped.action, "Run");
  assert.equal(UpdateFlow.panelModel(session({ updateBusy: "apply" })).action, "Restarting…");
  assert.equal(UpdateFlow.panelModel(session({ updateBusy: "apply", update: { previous_running: false } })).action, "Starting…");
});

test("a changed dump asks once, with Keep picked until the person picks Replace", () => {
  const m = UpdateFlow.panelModel(session({ update: { db_changed: true, previous_running: true } }));
  assert.equal(m.askDb, true);
  assert.equal(m.choice, "keep");
  assert.equal(UpdateFlow.panelModel(session({ update: { db_changed: true }, updateDbChoice: "replace" })).choice, "replace");
});

test("a failed apply shows its first line, the log tail and Roll back", () => {
  const err = "The `web` service didn't pass its health check within 180s.\n\nIts log:\nline 1\nline 2";
  const m = UpdateFlow.panelModel(session({ updateError: err }));
  assert.equal(m.error, "The `web` service didn't pass its health check within 180s.");
  assert.equal(m.tail, "Its log:\nline 1\nline 2");
  assert.equal(m.rollback, "Roll back to previous version");
  assert.equal(UpdateFlow.panelModel(session({ updateError: err, updateBusy: "rollback" })).rollback, "Rolling back…");
  // No log in the error itself: the run's own log is the tail.
  const plain = UpdateFlow.panelModel(session({ updateError: "`web` could not be started", runLogText: "a\nb\n" }));
  assert.equal(plain.tail, "a\nb");
  assert.equal(UpdateFlow.panelModel(session()).error, "", "no error before an apply - nothing stale");
  assert.equal(UpdateFlow.panelModel(session()).tail, "");
});

test("the log tail keeps the last lines and drops blank ones", () => {
  const text = Array.from({ length: 30 }, (_, i) => `line ${i}`).join("\n") + "\n\n";
  const tail = UpdateFlow.logTail(text, 5).split("\n");
  assert.deepEqual(tail, ["line 25", "line 26", "line 27", "line 28", "line 29"]);
  assert.equal(UpdateFlow.logTail(""), "");
});

test("busy disables everything, and errors split on their first non-empty line", () => {
  assert.equal(UpdateFlow.panelModel(session({ updateBusy: "rollback" })).busy, true);
  assert.deepEqual(UpdateFlow.splitError("\n first \nrest"), { message: "first", details: "rest" });
  assert.deepEqual(UpdateFlow.splitError(""), { message: "", details: "" });
});
