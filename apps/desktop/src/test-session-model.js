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

// ---------- Cloud drop ----------
const { lastSentDevice, cloudLinkNote, CLOUD_DROP_NOT_LINKED, applyCloudEvent, plainRunError } = require("./session-model.js");

test("a Cloud drop delivery's view makes the receiver the last device, with Last sent filled in", () => {
  const s = sendSessionFromView(view({ devices: [{ key: "dev-a", name: "Alice", marker: { sent_at: "2026-01-01T10:00:00Z", commits: [] } }] }));
  assert.equal(lastSentDevice(s.devices).name, "Alice");
  // cloud-drop-delivered { room_id, session_id, view } -> applyView(session, view)
  applyView(s, view({ devices: [...s.devices, { key: "cloud:bob@example.com", name: "bob@example.com", marker: { sent_at: "2026-01-02T10:00:00Z", commits: [] } }] }));
  const last = lastSentDevice(s.devices);
  assert.equal(last.key, "cloud:bob@example.com");
  assert.equal(last.marker.sent_at, "2026-01-02T10:00:00Z");
});

test("a session never sent anywhere has no last device", () => {
  assert.equal(lastSentDevice([]), null);
  assert.equal(lastSentDevice([{ key: "dev-a", name: "Alice", marker: null }]), null);
  assert.equal(lastSentDevice(undefined), null);
});

test("the receive form offers Link Google account only for errors linking fixes", () => {
  const expired = "Your Google sign-in expired. Link your account again to use Cloud drop.";
  assert.equal(cloudLinkNote(expired), expired, "an expired sign-in (invalid_grant) says so in the panel");
  assert.match(cloudLinkNote(CLOUD_DROP_NOT_LINKED), /No Google account is linked/);
  assert.equal(cloudLinkNote("Couldn't reach the relay at ws://x."), null);
  assert.equal(cloudLinkNote(null), null, "a passing pre-flight hides the panel");
});

test("the sender's Cloud drop card: joined -> requested -> granted -> downloaded", () => {
  const tr = { status: "waiting", cloudStep: "waiting" };
  applyCloudEvent(tr, "joined");
  assert.equal(tr.cloudStep, "joined");
  assert.equal(tr.receiverJoined, true);
  applyCloudEvent(tr, "requested", { email: "bob@example.com" });
  assert.equal(tr.cloudStep, "requested");
  assert.equal(tr.accessRequest.email, "bob@example.com");
  applyCloudEvent(tr, "joined"); // a late duplicate never moves it back
  assert.equal(tr.cloudStep, "requested");
  applyCloudEvent(tr, "granted", { email: "bob@example.com" });
  assert.equal(tr.cloudStep, "approved");
  assert.equal(tr.accessRequest, null);
  assert.equal(tr.grantedEmail, "bob@example.com");
  applyCloudEvent(tr, "downloaded");
  assert.equal(tr.cloudStep, "downloaded");
  assert.equal(tr.status, "done");
  applyCloudEvent(tr, "ended"); // the receiver closing afterwards changes nothing
  assert.equal(tr.status, "done");
});

test("a declined request ends the transfer; a receiver who leaves before asking is an error", () => {
  const declined = applyCloudEvent({ status: "waiting", cloudStep: "requested" }, "declined", { email: "eve@example.com" });
  assert.equal(declined.status, "done");
  assert.equal(declined.declinedEmail, "eve@example.com");
  const left = applyCloudEvent({ status: "waiting", cloudStep: "joined" }, "ended");
  assert.equal(left.status, "error");
  assert.match(left.errorText, /disconnected before getting access/);
  const granted = applyCloudEvent({ status: "waiting", cloudStep: "approved" }, "ended");
  assert.equal(granted.status, "done", "after a grant, the receiver closing is the normal end");
  const failed = applyCloudEvent({ status: "waiting", cloudStep: "waiting" }, "failed", { message: "No one entered this code." });
  assert.equal(failed.status, "error");
  assert.equal(failed.errorText, "No one entered this code.");
});

test("a project without run instructions fails Run with one line for the sender, no raw cause", () => {
  const raw =
    "This project arrived without run instructions (no docker-compose.yml), so LocalSync doesn't know how to build or run it. Ask the sender to finish Setup for this project in LocalSync (it creates the run instructions for them) and send it again.: No such file or directory (os error 2)";
  const line = plainRunError(raw);
  assert.equal(line, "This project arrived without run instructions. Ask the sender to finish Setup for it in LocalSync and send it again.");
  assert.ok(!line.includes("os error"));
  assert.equal(plainRunError("Podman isn't running."), "Podman isn't running.", "other errors are shown as they are");
  assert.equal(plainRunError(""), "");
});
