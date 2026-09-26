// Tests for the pure session model (session-model.js). Run with:
//   node --test apps/desktop/src/test-session-model.js
const test = require("node:test");
const assert = require("node:assert/strict");
const { sendSessionFromView, applyView, displayStatus, sameFolderPlan, deviceKeyFor } = require("./session-model.js");

const view = (over = {}) => ({
  id: "s1",
  title: "xusom-admin",
  folders: [{ path: "/w/xusom-admin", dump: null }],
  created_at: "2026-01-01T00:00:00Z",
  saved: false,
  artifact: { snapshot_id: "xusom-admin@abc", commits: [], size_bytes: 10, built_at: "2026-01-01T00:00:00Z" },
  devices: [],
  ...over,
});

test("a session is created once from the backend's view, with no transfers yet", () => {
  const s = sendSessionFromView(view());
  assert.equal(s.kind, "send");
  assert.equal(s.id, "s1");
  assert.deepEqual(s.transfers, []);
  assert.equal(s.saved, false);
});

test("applying a new view replaces what the backend owns and keeps this side's transfers", () => {
  const s = sendSessionFromView(view());
  s.transfers.push({ id: "t1", status: "done" });
  applyView(s, view({ saved: true, devices: [{ key: "dev-a", name: "Alice", up_to_date: true }] }));
  assert.equal(s.saved, true);
  assert.equal(s.devices.length, 1);
  assert.equal(s.transfers.length, 1, "transfers live only on this side and must survive a view refresh");
});

test("the tab status is derived from the transfers - retry and a second device never need a second session", () => {
  const s = sendSessionFromView(view());
  assert.equal(displayStatus(s), "ready");
  s.transfers.push({ status: "connecting" });
  assert.equal(displayStatus(s), "connecting");
  s.transfers.push({ status: "active" });
  assert.equal(displayStatus(s), "active", "one live transfer sending outranks another still waiting");
  s.transfers = [{ status: "error" }];
  assert.equal(displayStatus(s), "error");
  s.transfers[0].status = "connecting"; // retried in place: same transfer, same session
  assert.equal(displayStatus(s), "connecting");
  s.transfers[0].status = "done";
  assert.equal(displayStatus(s), "ready");
});

test("a receive session keeps its own status", () => {
  assert.equal(displayStatus({ kind: "receive", status: "reviewing" }), "reviewing");
});

test("the same project and database plan is the same workspace, regardless of folder order", () => {
  const a = [
    { path: "/w/a", dump: null },
    { path: "/w/b", dump: { schema: "s", file_path: "/d.sql", engine: "mysql" } },
  ];
  const b = [a[1], a[0]];
  assert.ok(sameFolderPlan(a, b));
});

test("a different database plan, or different folders, is a different workspace", () => {
  const base = [{ path: "/w/a", dump: { schema: "s", file_path: "/d.sql", engine: "mysql" } }];
  assert.ok(!sameFolderPlan(base, [{ path: "/w/a", dump: null }]));
  assert.ok(!sameFolderPlan(base, [{ path: "/w/a", dump: { schema: "s", file_path: "/other.sql", engine: "mysql" } }]));
  assert.ok(!sameFolderPlan(base, [{ path: "/w/b", dump: base[0].dump }]));
});

test("a discovered device is filed under its persistent id, falling back to its name", () => {
  assert.equal(deviceKeyFor({ device_id: "abc123", nickname: "Alice" }), "abc123");
  assert.equal(deviceKeyFor({ device_id: null, nickname: "Alice" }), "name:Alice");
  assert.equal(deviceKeyFor({ nickname: "Alice" }), "name:Alice");
});
