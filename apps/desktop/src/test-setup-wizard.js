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

test("a failed test container offers Restart WSL only when the backend gives it a consent (Windows)", () => {
  const failed = { step: "functional_check", status: "failed", consent: "This runs `wsl --shutdown`...", manual_instructions: "m" };
  assert.deepEqual(W.stepActions(failed), ["retry", "fix", "manual"]);
  assert.equal(W.fixLabel(failed), "Restart WSL and retry");
  assert.deepEqual(W.stepActions({ ...failed, consent: null }), ["retry", "manual"]);
  assert.equal(W.fixLabel({ step: "machine_ready" }), "Fix it");
});

const STUCK_SUMMARY = "Podman's virtual machine is running, but the Podman service inside it is stuck.";

test("the stuck Podman service gets 'Fix automatically', never the 'isn't running' wording", () => {
  assert.equal(W.SERVICE_STUCK, STUCK_SUMMARY);
  assert.ok(!W.SERVICE_STUCK.includes("isn't running"));
  const stuck = { step: "functional_check", status: "failed", consent: "wsl --shutdown…", summary: STUCK_SUMMARY };
  assert.equal(W.fixLabel(stuck), "Fix automatically");
  assert.equal(W.fixLabel({ ...stuck, summary: "Podman's virtual machine isn't running." }), "Restart WSL and retry");
  // Run / test-run errors from the readiness gate.
  const gateErr = `${W.NOT_READY_PREFIX}: ${STUCK_SUMMARY}\nOpen Setup in LocalSync to fix it.\nDetails:\nssh: rejected: connect failed`;
  assert.ok(W.isPodmanNotReady(gateErr) && W.isPodmanStuck(gateErr) && W.isPodmanStuck(new Error(gateErr)));
  assert.equal(W.errorFixLabel(gateErr, true), "Fix automatically");
  assert.equal(W.errorFixLabel(gateErr, false), "Fix setup", "the automatic fix is Windows only");
  assert.equal(W.errorFixLabel(`${W.NOT_READY_PREFIX}: Podman isn't installed.`, true), "Fix setup");
});

test("the test container's fix shows five recovery steps, driven by progress events and the final result", () => {
  const v = W.viewFromState(winState(["podman_installed", "wsl_enabled", "restart_after_wsl", "machine_ready"]));
  W.beginFix(v, "functional_check");
  assert.deepEqual(v.recovery.map((r) => r.status), Array(5).fill("pending"));
  assert.deepEqual(v.recovery.map((r) => r.label), W.RECOVERY_STEPS);
  for (const i of [0, 1, 2, 3]) {
    W.applyFixProgress(v, { index: i, label: W.RECOVERY_STEPS[i], status: "running" });
    assert.equal(v.recovery[i].status, "running");
    W.applyFixProgress(v, { index: i, label: W.RECOVERY_STEPS[i], status: "done" });
  }
  W.applyFixProgress(v, { index: 4, label: W.RECOVERY_STEPS[4], status: "running" });
  W.applyFixProgress(v, { index: 9, label: "x", status: "done" }); // out of range: ignored
  const all = winState(["podman_installed", "wsl_enabled", "restart_after_wsl", "machine_ready", "functional_check"]);
  assert.equal(W.applyResult(v, { ...all, first_undone: null, failure: null }), "done");
  assert.deepEqual(v.recovery.map((r) => r.status), Array(5).fill("done"));
  // The re-run test container fails: its row fails, earlier rows stay done.
  const v2 = W.viewFromState(winState(["podman_installed", "wsl_enabled", "restart_after_wsl", "machine_ready"]));
  W.beginFix(v2, "functional_check");
  for (const i of [0, 1, 2, 3]) W.applyFixProgress(v2, { index: i, status: "done" });
  W.applyFixProgress(v2, { index: 4, status: "running" });
  const st = winState(["podman_installed", "wsl_enabled", "restart_after_wsl", "machine_ready"]);
  W.applyResult(v2, { ...st, first_undone: "functional_check", failure: { step: "functional_check", summary: STUCK_SUMMARY, details: "" } });
  assert.deepEqual(v2.recovery.map((r) => r.status), ["done", "done", "done", "done", "failed"]);
  // A fix that errors out marks the running step failed; a retry clears the list.
  const v3 = W.viewFromState(winState(["podman_installed"]));
  W.beginFix(v3, "functional_check");
  W.applyFixProgress(v3, { index: 3, status: "running" });
  W.applyError(v3, "Podman still doesn't answer", "functional_check");
  assert.equal(v3.recovery[3].status, "failed");
  W.beginVerify(v3);
  assert.equal(v3.recovery, null);
  // Other fixes have no recovery list.
  W.beginFix(v3, "machine_ready");
  assert.equal(v3.recovery, null);
});
