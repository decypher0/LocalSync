// The receiver's side of an update: a new version of a project landed on a
// session this device already ran (receiver_session_commands::
// track_received_snapshot matched it by sender and project). Fills the
// receiver page's #sp-update slot (views.js, reviewing layout) with what
// changed, the one database question when the sender's dump changed,
// "Apply & restart" (or "Run", when nothing is running), and - when the new
// version doesn't come up healthy - its log and "Roll back to previous
// version".
//
// Loaded after views.js. `UpdateFlow`'s pure part is tested from Node
// (test-update-flow.js); the rest only draws it and calls
// apply_received_update / rollback_received_update.
const UpdateFlow = (() => {
  // Under "Replace with sender's database": the receiver's data doesn't come back.
  const REPLACE_WARNING = "This can't be undone, even by rolling back.";

  "use strict";

  const filesChanged = (diff) => (diff && diff.entries ? diff.entries.length : 0);

  function headline(diff) {
    const n = filesChanged(diff);
    return `Update received (${n} file${n === 1 ? "" : "s"} changed)`;
  }

  /** What apply_received_update gets as `db_choice`: asked only when the dump changed, Keep by default. */
  function dbChoice(update, picked) {
    if (!update || !update.db_changed) return null;
    return picked === "replace" ? "replace" : "keep";
  }

  /** The last `max` non-empty lines of `text`. */
  function logTail(text, max = 15) {
    const lines = String(text || "")
      .split("\n")
      .map((l) => l.trimEnd())
      .filter((l) => l.trim());
    return lines.slice(-max).join("\n");
  }

  /** A failed apply's error: its first line to read, the rest (the service's log) to look at. */
  function splitError(err) {
    const lines = String(err || "").split("\n");
    const first = lines.findIndex((l) => l.trim());
    if (first === -1) return { message: "", details: "" };
    return { message: lines[first].trim(), details: lines.slice(first + 1).join("\n") };
  }

  /** Everything #sp-update shows for session `s`, or `{ show: false }`. */
  function panelModel(s) {
    const u = s && s.update;
    if (!u) return { show: false };
    const running = !!u.previous_running;
    const failed = splitError(s.updateError);
    return {
      show: true,
      headline: headline(s.diff),
      note: running
        ? "The version that's running now keeps running until you apply this."
        : "Review what changed, then run the new version.",
      askDb: !!u.db_changed,
      choice: dbChoice(u, s.updateDbChoice),
      action: s.updateBusy === "apply" ? (running ? "Restarting…" : "Starting…") : running ? "Apply & restart" : "Run",
      busy: !!s.updateBusy,
      error: failed.message,
      tail: failed.message ? logTail(failed.details || s.runLogText) : "",
      // After a failed apply: go back. Before one: decline it.
      rollback: failed.message
        ? s.updateBusy === "rollback" ? "Rolling back…" : "Roll back to previous version"
        : s.updateBusy === "rollback" ? "Keeping it…" : "Keep the current version",
    };
  }

  return { filesChanged, headline, dbChoice, logTail, splitError, panelModel, REPLACE_WARNING };
})();

if (typeof module !== "undefined" && module.exports) module.exports = UpdateFlow;

if (typeof window !== "undefined" && window.__TAURI__) {
  (() => {
    "use strict";
    const { invoke } = window.__TAURI__.core;
    const { listen } = window.__TAURI__.event;
    const $id = (id) => document.getElementById(id);

    function rerender(s) {
      renderSessionTabs();
      if (s.id === activeSessionId) renderActiveSession();
    }

    // What the backend knows about the receive that just landed: whether it
    // is an update waiting to be applied, and the session's own record (its
    // name, sender, database) - the tab may be new for a session that was
    // only on disk.
    async function refresh(s) {
      try {
        const view = await invoke("open_saved_received_session", { sessionId: s.id });
        applyReceivedView(s, view);
        if (!s.displayName && view.name !== view.title) s.displayName = view.name;
        s.update = view.update || null;
      } catch (err) {
        console.warn("couldn't check the received session for an update:", err);
        s.update = null;
      } finally {
        s.updateChecking = false;
        rerender(s);
      }
    }

    const origApplyReview = window.applyReviewInfoToSession;
    window.applyReviewInfoToSession = function (session, info) {
      origApplyReview(session, info);
      if (session.kind !== "receive") return;
      // A new version is a new question: nothing from the last one stays.
      Object.assign(session, { update: null, updateChecking: true, updateError: "", updateDbChoice: null, updateBusy: null });
      refresh(session);
    };

    async function apply(s) {
      if (s.updateBusy) return;
      const dbChoice = UpdateFlow.dbChoice(s.update, s.updateDbChoice);
      Object.assign(s, { updateBusy: "apply", updateError: "", runInProgress: true, runLogText: "", runErrorText: "" });
      rerender(s);
      // The Logs panel follows the build and start live (views.js redraws it).
      const unlisten = await listen("run-progress", (evt) => {
        if (evt.payload.session_id !== s.snapshotId) return;
        s.runLogText += (s.runLogText ? "\n" : "") + evt.payload.line;
      });
      try {
        const run = await invoke("apply_received_update", { sessionId: s.id, dbChoice });
        Object.assign(s, {
          runningSessionId: run.session_id,
          servicePorts: run.service_ports,
          dbCacheHit: run.db_cache_hit,
          status: "running",
          endedAt: null,
          update: null,
        });
      } catch (err) {
        s.updateError = String(err);
      } finally {
        unlisten();
        Object.assign(s, { updateBusy: null, runInProgress: false });
        rerender(s);
      }
    }

    async function rollBack(s) {
      if (s.updateBusy) return;
      s.updateBusy = "rollback";
      rerender(s);
      let error = "";
      try {
        await invoke("rollback_received_update", { sessionId: s.id });
      } catch (err) {
        error = String(err);
      }
      try {
        const view = await invoke("open_saved_received_session", { sessionId: s.id });
        applyReceivedView(s, view);
        if (view.update) {
          // Nothing changed: the update is still waiting.
          s.update = view.update;
          s.updateError = error;
        } else {
          // Back on the previous version: its page, running or stopped.
          Object.assign(s, {
            update: null,
            updateError: "",
            manifest: null,
            diff: null,
            runLogText: "",
            runErrorText: error,
            status: view.running ? "running" : "done",
            servicePorts: view.running ? view.service_ports || null : null,
            dbCacheHit: view.db_cache_hit,
            endedAt: view.running ? null : new Date().toISOString(),
          });
        }
      } catch (err) {
        s.updateError = error || String(err);
      } finally {
        s.updateBusy = null;
        rerender(s);
      }
    }

    const button = (id, label, cls = "") => `<button id="${id}" type="button" class="lsv-btn ${cls}"><span>${escapeHtml(label)}</span></button>`;

    function render(s, state) {
      const reviewing = state === "reviewing";
      // While an update is pending, its own buttons below stand in for Run/Reject.
      const pending = reviewing && (s.updateChecking || !!s.update);
      for (const id of ["sp-run", "sp-reject"]) if ($id(id)) $id(id).classList.toggle("hidden", pending);
      const slot = $id("sp-update");
      if (!slot) return;
      const m = UpdateFlow.panelModel(s);
      if (!reviewing || !m.show) {
        slot.innerHTML = "";
        return;
      }
      slot.innerHTML = `<section class="lsv-panel"><div class="lsv-panel-title">${escapeHtml(m.headline)}</div><div class="lsv-armed">
        <p class="lsv-small">${escapeHtml(m.note)}</p>
        ${
          m.askDb
            ? `<fieldset class="lsv-field" id="up-db"><legend class="lsv-small">The sender's database changed.</legend>
                <label class="lsv-small"><input type="radio" name="up-db" value="keep" ${m.choice === "keep" ? "checked" : ""}> Keep my data</label>
                <label class="lsv-small"><input type="radio" name="up-db" value="replace" ${m.choice === "replace" ? "checked" : ""}> Replace with sender's database (my local data is lost). ${escapeHtml(UpdateFlow.REPLACE_WARNING)}</label>
              </fieldset>`
            : ""
        }
        ${button("up-apply", m.action, "lsv-btn-primary")}
        ${m.error ? `<p class="lsv-error">${escapeHtml(m.error)}</p>` : ""}
        ${m.tail ? `<pre class="lsv-logs" aria-label="The new version's log">${escapeHtml(m.tail)}</pre>` : ""}
        ${button("up-rollback", m.rollback, m.error ? "" : "lsv-btn-small")}
      </div></section>`;
      $id("up-apply").disabled = m.busy;
      $id("up-rollback").disabled = m.busy;
      $id("up-apply").addEventListener("click", () => apply(s));
      $id("up-rollback").addEventListener("click", () => rollBack(s));
      for (const r of slot.querySelectorAll('input[name="up-db"]')) {
        r.disabled = m.busy;
        r.addEventListener("change", () => (s.updateDbChoice = r.value));
      }
    }

    UpdateFlow.render = render;
  })();
}
