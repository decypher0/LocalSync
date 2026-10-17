// The compatibility check before a Push update / Send again: shows
// "Checking this update..." in #sp-compat (with the test run's live
// compose-test-progress lines), runs the backend's check_update_compat, then
// either sends (compatible) or stops with the reason, the last log lines,
// "Fix and re-check" and "Send anyway". Loaded by a plain <script> after
// app.js; the pure part (view) is tested by test-compat-check.js under Node.

const CompatCheck = (() => {
  const LOG_LINES = 12;
  const ANYWAY_WARNING = "Sends this version even though it didn't pass; the other device may not be able to run it.";

  const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
  const tail = (text, n) => String(text || "").split("\n").filter((l) => l.trim()).slice(-n).join("\n");

  /** What #sp-compat shows for a state: { kind: "checking" | "ok" | "fail", lines? } or a check_update_compat report. */
  function view(st) {
    if (!st) return null;
    if (st.kind === "checking") return { kind: "checking", title: "Checking this update...", log: tail((st.lines || []).join("\n"), LOG_LINES), notes: [], problems: [], actions: [] };
    const r = st.report;
    const t = r.test;
    if (r.ok) {
      const title = !t ? "Compatible - quick checks passed" : t.cached ? "Compatible - already built and started for this version" : `Compatible - built and started in ${t.secs} s`;
      return { kind: "ok", title, log: "", notes: r.notes || [], problems: [], actions: [] };
    }
    return {
      kind: "fail",
      title: "Not compatible",
      problems: r.problems || [],
      notes: r.notes || [],
      log: t && !t.ok ? tail(t.output_tail || t.error, LOG_LINES) : "",
      actions: ["recheck", "anyway"],
      warning: ANYWAY_WARNING,
    };
  }

  function html(v) {
    if (!v) return "";
    const color = v.kind === "ok" ? "var(--green)" : v.kind === "fail" ? "var(--danger-text)" : "inherit";
    return (
      `<p class="lsv-small" data-compat-title style="color:${color};font-weight:600">${esc(v.title)}</p>` +
      v.problems.map((p) => `<p class="lsv-error lsv-small">${esc(p)}</p>`).join("") +
      v.notes.map((n) => `<p class="lsv-muted lsv-small">${esc(n)}</p>`).join("") +
      (v.log ? `<pre class="lsv-logs" style="min-height:0;max-height:180px;margin:6px 0">${esc(v.log)}</pre>` : "") +
      (v.actions.length
        ? `<div class="lsv-inline-row" style="display:flex;gap:8px;margin-top:6px">
             <button type="button" class="lsv-btn lsv-btn-small lsv-btn-primary" data-compat="recheck">Fix and re-check</button>
             <button type="button" class="lsv-btn lsv-btn-small" data-compat="anyway">Send anyway</button></div>
           <p class="lsv-muted lsv-small">${esc(v.warning)}</p>`
        : "")
    );
  }

  // ----- DOM -----
  let el = null;
  let current = null; // { sessionId, proceed, st }

  function render() {
    if (!el) return;
    const v = current && view(current.st);
    el.classList.toggle("hidden", !v);
    el.innerHTML = html(v);
  }

  /** The #sp-compat element (one, reused across page rebuilds). Given a session id, drops another session's check. */
  function slot(sessionId) {
    if (!el) {
      el = document.createElement("div");
      el.id = "sp-compat";
      el.style.margin = "8px 0";
      el.addEventListener("click", (e) => {
        const b = e.target.closest("[data-compat]");
        if (!b || !current) return;
        if (b.dataset.compat === "recheck") run();
        else {
          const go = current.proceed;
          current = null;
          render();
          go();
        }
      });
    }
    if (sessionId !== undefined && current && current.sessionId !== sessionId) current = null;
    render();
    return el;
  }

  async function run() {
    const c = current;
    c.st = { kind: "checking", lines: [] };
    render();
    let report;
    try {
      report = await window.__TAURI__.core.invoke("check_update_compat", { sessionId: c.sessionId });
    } catch (err) {
      report = { ok: false, problems: [String(err)], notes: [], test: null };
    }
    if (current !== c) return; // another check (or session) took over
    c.st = { kind: "done", report };
    render();
    if (report.ok) c.proceed();
  }

  /** Checks `sessionId`'s update, then calls `proceed()` when it's compatible (or on Send anyway). */
  function gate(sessionId, proceed) {
    if (current && current.sessionId === sessionId && current.st && current.st.kind === "checking") return;
    current = { sessionId, proceed, st: null };
    slot(sessionId);
    return run();
  }

  if (typeof window !== "undefined" && window.__TAURI__) {
    window.__TAURI__.event.listen("compose-test-progress", (evt) => {
      if (!current || !current.st || current.st.kind !== "checking") return;
      current.st.lines.push((evt.payload && evt.payload.line) || "");
      if (current.st.lines.length > 200) current.st.lines.shift();
      render();
    });
  }

  return { view, html, gate, slot, ANYWAY_WARNING };
})();

if (typeof module !== "undefined" && module.exports) {
  module.exports = { CompatCheck };
}
