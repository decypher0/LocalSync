// Tests for the pure setup-checklist model (setup-wizard.js). Run with:
//   node --test apps/desktop/src/test-setup-wizard.js
const test = require("node:test");
const assert = require("node:assert/strict");
const W = require("./setup-wizard.js");

const step = (id, status = "not_started", over = {}) => ({
  step: id, title: id, status, consent: null, needs_admin: false, manual_instructions: `do ${id} by hand`, ...over,
});
const winState = (done = []) => {
  const ids = ["podman_installed", "wsl_enabled", "restart_after_wsl", "machine_ready", "functional_check"];
  const steps = ids.map((id) => step(id, done.includes(id) ? "done" : "not_started", id === "machine_ready" ? { consent: "We'll set up Podman's machine.", needs_admin: true } : {}));
  return { os: "windows", steps, any_progress: done.length > 0, all_done: done.length === ids.length, restart_required: false };
};
const statuses = (v) => v.steps.map((s) => s.status);

test("no progress starts on the welcome screen; progress resumes on the checklist with done steps ticked", () => {
  assert.equal(W.viewFromState(winState()).phase, "welcome");
  const v = W.viewFromState(winState(["podman_installed", "wsl_enabled"]));
  assert.equal(v.phase, "checking");
  assert.deepEqual(statuses(v), ["done", "done", "pending", "pending", "pending"]);
});

test("live events fill steps in; a final success marks everything done", () => {
  const v = W.viewFromState(winState(["podman_installed"]));
  W.beginVerify(v);
  W.applyEvent(v, { step: "wsl_enabled", status: "checking", summary: "Checking WSL…" });
  assert.equal(v.steps[1].status, "checking");
  W.applyEvent(v, { step: "wsl_enabled", status: "done", summary: "" });
  assert.equal(v.steps[1].status, "done");
  assert.equal(v.steps[1].summary, "Checking WSL…", "an empty summary does not wipe the last one");
  const all = winState(["podman_installed", "wsl_enabled", "restart_after_wsl", "machine_ready", "functional_check"]);
  assert.equal(W.applyResult(v, { ...all, first_undone: null, failure: null }), "done");
  assert.deepEqual(statuses(v), Array(5).fill("done"));
});

test("a failure marks only that step failed, with plain summary and raw details, and offers retry/fix/manual", () => {
  const v = W.viewFromState(winState(["podman_installed", "wsl_enabled", "restart_after_wsl"]));
  W.beginVerify(v);
  W.applyEvent(v, { step: "machine_ready", status: "checking" });
  const phase = W.applyResult(v, {
    ...winState(["podman_installed", "wsl_enabled", "restart_after_wsl"]),
    first_undone: "machine_ready",
    failure: { step: "machine_ready", summary: "Podman can't apply memory limits.", details: "crun: open `memory.max`" },
  });
  assert.equal(phase, "failed");
  const m = v.steps[3];
  assert.equal(m.status, "failed");
  assert.equal(m.summary, "Podman can't apply memory limits.");
  assert.match(m.details, /crun/);
  assert.equal(v.steps[4].status, "pending");
  assert.deepEqual(W.stepActions(m), ["retry", "fix", "manual"]);
  assert.deepEqual(W.stepActions(v.steps[4]), []);
  assert.deepEqual(W.stepActions({ ...m, consent: null, manual_instructions: "" }), ["retry"]);
});

test("retry clears a failed step back to pending", () => {
  const v = W.viewFromState(winState(["podman_installed"]));
  W.applyError(v, "boom");
  assert.equal(v.steps[1].status, "failed");
  W.beginVerify(v);
  assert.deepEqual(statuses(v), ["done", "pending", "pending", "pending", "pending"]);
  assert.equal(v.steps[1].details, "");
});

test("restart pending leads to the restart phase", () => {
  const v = W.viewFromState(winState(["podman_installed", "wsl_enabled"]));
  W.applyEvent(v, { step: "restart_after_wsl", status: "pending", summary: "Restart needed" });
  assert.equal(v.steps[2].status, "pending");
  assert.equal(W.applyResult(v, { ...winState(["podman_installed", "wsl_enabled"]), restart_required: true, first_undone: "restart_after_wsl", failure: null }), "restart");
});

test("not done with no failure reported still lands on a failed step, never on done", () => {
  const v = W.viewFromState(winState(["podman_installed"]));
  assert.equal(W.applyResult(v, { ...winState(["podman_installed"]), first_undone: "wsl_enabled", failure: null }), "failed");
  assert.equal(v.steps[1].status, "failed");
  assert.ok(v.steps[1].summary);
});

test("fix: progress lines go to details only; a rejected fix keeps the log and adds the error", () => {
  const v = W.viewFromState(winState(["podman_installed", "wsl_enabled", "restart_after_wsl"]));
  W.beginFix(v, "machine_ready");
  assert.equal(v.phase, "fixing");
  assert.equal(v.steps[3].status, "checking");
  W.applyProgress(v, { step: "machine_ready", line: "line 1" });
  W.applyProgress(v, { step: "machine_ready", line: "line 2" });
  assert.equal(v.steps[3].details, "line 1\nline 2");
  W.applyError(v, "exit status 125", "machine_ready");
  assert.equal(v.steps[3].status, "failed");
  assert.equal(v.steps[3].details, "line 1\nline 2\nexit status 125");
  assert.doesNotMatch(v.steps[3].summary, /125/);
});

test("consent text adds the admin sentence only when needed", () => {
  const s = { consent: "We'll enable WSL now.", needs_admin: true };
  assert.equal(W.consentMessage(s, "windows"), "We'll enable WSL now. Windows will ask for administrator permission.");
  assert.equal(W.consentMessage({ ...s, needs_admin: false }, "windows"), "We'll enable WSL now.");
});

test("isPodmanNotReady recognises the backend prefix only", () => {
  assert.ok(W.isPodmanNotReady("Podman isn't ready to run containers: machine stopped"));
  assert.ok(W.isPodmanNotReady(new Error("Podman isn't ready to run containers")));
  assert.ok(!W.isPodmanNotReady("compose failed: port in use"));
  assert.ok(!W.isPodmanNotReady(null));
  assert.ok(!W.isPodmanNotReady(undefined));
});

test("Linux has just two steps", () => {
  const v = W.viewFromState({ os: "linux", steps: [step("podman_installed"), step("functional_check")], any_progress: false });
  assert.equal(v.steps.length, 2);
});
