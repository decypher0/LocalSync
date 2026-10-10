// The pure parts of the dependency-setup checklist, kept out of app.js so they
// can be tested from Node (same split as session-model.js): no DOM, no Tauri.
//
// The backend (setup_state / setup_verify / setup_fix) owns the truth; this
// only turns its results plus the live `setup-step` / `setup-progress` events
// into one view: { os, phase, steps: [{ step, title, status, summary, details,
// consent, needs_admin, manual_instructions }] }.
//   phase:  welcome | checking | fixing | failed | restart | done
//   status: pending | checking | done | failed

const SetupWizard = (() => {
  // Must match ls_containers::readiness::NOT_READY_PREFIX.
  const NOT_READY_PREFIX = "Podman isn't ready to run containers";

  const STATUS_LABEL = { pending: "Not checked yet", checking: "Checking", done: "Done", failed: "Needs attention" };

  function viewFromState(state) {
    return {
      os: state.os,
      phase: state.any_progress ? "checking" : "welcome",
      steps: state.steps.map((s) => ({
        step: s.step,
        title: s.title,
        consent: s.consent || null,
        needs_admin: !!s.needs_admin,
        manual_instructions: s.manual_instructions || "",
        status: s.status === "done" ? "done" : "pending",
        summary: "",
        details: "",
      })),
    };
  }

  const find = (view, step) => view.steps.find((s) => s.step === step);

  /** A (re)check is starting: anything not done goes back to a clean pending. */
  function beginVerify(view) {
    view.phase = "checking";
    for (const s of view.steps) {
      if (s.status !== "done") Object.assign(s, { status: "pending", summary: "", details: "" });
    }
  }

  function beginFix(view, step) {
    beginVerify(view);
    view.phase = "fixing";
    const s = find(view, step);
    if (s) Object.assign(s, { status: "checking", summary: "Setting this up now…" });
  }

  /** A live `setup-step` event. "pending" (waiting for a restart) stays pending. */
  function applyEvent(view, evt) {
    const s = find(view, evt.step);
    if (!s) return;
    s.status = evt.status === "pending" ? "pending" : evt.status;
    if (evt.summary) s.summary = evt.summary;
    if (evt.details) s.details = evt.details;
  }

  /** A live `setup-progress` log line from setup_fix - only ever goes into details. */
  function applyProgress(view, evt) {
    const s = find(view, evt.step);
    if (s) s.details += (s.details ? "\n" : "") + evt.line;
  }

  /** The final result of setup_verify / setup_fix. Returns the new phase. */
  function applyResult(view, res) {
    for (const r of res.steps || []) {
      const s = find(view, r.step);
      if (!s) continue;
      if (r.status === "done") s.status = "done";
      else if (s.status === "checking" || s.status === "done") s.status = "pending";
    }
    const failure = res.failure;
    if (failure && find(view, failure.step)) {
      Object.assign(find(view, failure.step), { status: "failed", summary: failure.summary, details: failure.details || "" });
    }
    if (res.all_done) view.phase = "done";
    else if (res.restart_required) view.phase = "restart";
    else if (failure) view.phase = "failed";
    else {
      // Not done, no restart, no failure reported: still must not look finished.
      const s = find(view, res.first_undone) || view.steps.find((x) => x.status !== "done");
      if (s) Object.assign(s, { status: "failed", summary: s.summary || "This step isn't finished yet." });
      view.phase = s ? "failed" : "done";
    }
    return view.phase;
  }

  /** setup_verify / setup_fix rejected outright. */
  function applyError(view, err, step) {
    const s = (step && find(view, step)) || view.steps.find((x) => x.status !== "done");
    view.phase = "failed";
    if (!s) return view.phase;
    const raw = String(err);
    Object.assign(s, {
      status: "failed",
      summary: step ? "That didn't work. You can try again or do it yourself." : "We couldn't finish checking this step.",
      details: s.details ? `${s.details}\n${raw}` : raw,
    });
    return view.phase;
  }

  /** Which buttons a step shows: retry, fix, manual. Only a failed step has any. */
  function stepActions(s) {
    if (s.status !== "failed") return [];
    const out = ["retry"];
    if (s.consent) out.push("fix");
    if (s.manual_instructions) out.push("manual");
    return out;
  }

  /** The fix button's label. The test container's fix (Windows only) restarts WSL. */
  function fixLabel(s) {
    return s.step === "functional_check" ? "Restart WSL and retry" : "Fix it";
  }

  function consentMessage(s, os) {
    if (!s.needs_admin) return s.consent;
    const ask = os === "windows" ? "Windows will ask for administrator permission." : "You'll be asked for your password.";
    return `${s.consent} ${ask}`;
  }

  function statusLabel(status) {
    return STATUS_LABEL[status] || status;
  }

  function isPodmanNotReady(err) {
    return String((err && err.message) || err || "").trimStart().startsWith(NOT_READY_PREFIX);
  }

  return {
    NOT_READY_PREFIX,
    viewFromState,
    beginVerify,
    beginFix,
    applyEvent,
    applyProgress,
    applyResult,
    applyError,
    stepActions,
    fixLabel,
    consentMessage,
    statusLabel,
    isPodmanNotReady,
  };
})();

if (typeof module !== "undefined" && module.exports) {
  module.exports = SetupWizard;
}
