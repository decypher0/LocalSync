// The sender session page's "Changes since last send" panel (#sp-changes):
// per folder, what was added, modified or deleted since the last send to
// Push update's device, with a tick box for each new file. Fed by views.js
// with each project_changes result - the same poll and focus refresh that
// enables Push update, so the panel and the button always agree. Loaded by a
// plain <script> before views.js; the pure part (model) is tested by
// test-changes-panel.js under Node.

const ChangesPanel = (() => {
  // A long list (a first send of a big folder) is cut here, with a count.
  const MAX_LINES = 50;
  const KIND = { added: "Added", modified: "Modified", deleted: "Deleted" };

  const esc = (s) =>
    String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
  const folderName = (path) => String(path).split(/[\\/]/).filter(Boolean).pop() || path;
  const plural = (n, word) => `${n} ${word}${n === 1 ? "" : "s"}`;

  /**
   * What the panel shows for one project_changes result (or { pending } /
   * { error }): { title, message, folders: [{ name, path, notes, lines, more }] }.
   * A line is { path, kind, tickable, checked, secret, tooLarge }.
   */
  function model(changes) {
    const title = changes && changes.last_device_name ? `Changes since the last send to ${changes.last_device_name}` : "Changes since the last send";
    if (!changes || changes.pending) return { title, message: "Checking for changes…", folders: [] };
    if (changes.error) return { title, message: `Couldn't check for changes: ${changes.error}`, folders: [] };
    const sentBefore = !!changes.last_device_key;
    const folders = (changes.folders || []).map((f) => {
      const notes = [];
      if (f.base_gone) notes.push("The version last sent is no longer in git - Push update sends this folder in full.");
      else if (f.not_sent && sentBefore) notes.push("Not sent to this device yet.");
      if (f.dump_changed) notes.push("Database dump exported again.");
      for (const x of f.externals || []) notes.push(`external: ${x.name} (${x.change_type})`);
      const all = (f.files || []).map((x) => ({
        path: x.path,
        kind: KIND[x.change_type] || x.change_type,
        tickable: !!x.new_file && !x.too_large,
        checked: !!x.included,
        secret: !!x.secret,
        tooLarge: !!x.too_large,
      }));
      return { name: folderName(f.path), path: f.path, notes, lines: all.slice(0, MAX_LINES), more: Math.max(0, all.length - MAX_LINES) };
    });
    const shown = folders.filter((f) => f.notes.length || f.lines.length);
    let message = "";
    if (!sentBefore) message = "Not sent to this device yet.";
    else if (changes.changed && !shown.length) message = "Committed since the last send - no file changes.";
    else if (!changes.changed) message = "No changes since the last send.";
    return { title, message, folders: folders.length > 1 ? shown : shown.map((f) => ({ ...f, name: "" })) };
  }

  function html(m, errorText) {
    const lines = (f) =>
      f.lines
        .map((l) => {
          const box = l.tickable
            ? `<input type="checkbox" data-change-file="${esc(l.path)}" data-folder="${esc(f.path)}" ${l.checked ? "checked" : ""}> `
            : "";
          const hint = l.tooLarge ? " · too large, not included" : l.tickable && l.secret && !l.checked ? " · may hold secrets, tick to send" : "";
          return `<label class="lsv-row"><span class="lsv-label">${box}${esc(l.kind)}${esc(hint)}</span><span class="lsv-val">${esc(l.path)}</span></label>`;
        })
        .join("") + (f.more ? `<p class="lsv-muted lsv-small">and ${plural(f.more, "more file")}</p>` : "");
    const body = m.folders
      .map((f) => (f.name ? `<div class="lsv-subhead">${esc(f.name)}</div>` : "") + f.notes.map((n) => `<p class="lsv-muted lsv-small">${esc(n)}</p>`).join("") + lines(f))
      .join("");
    return `<section class="lsv-panel"><div class="lsv-panel-title">${esc(m.title)}</div>${
      m.message ? `<p class="lsv-muted lsv-small">${esc(m.message)}</p>` : ""
    }${body}${errorText ? `<p class="lsv-error">${esc(errorText)}</p>` : ""}</section>`;
  }

  let error = { sessionId: null, text: "" };
  const drawn = new WeakMap(); // slot -> the html last drawn into it

  /**
   * Draws `changes` (a project_changes result for `sessionId`) into `slot`.
   * Ticking a new file in or out saves the choice, then calls `refresh`
   * (which fetches project_changes again and draws it back here).
   */
  function render(slot, sessionId, changes, refresh) {
    if (!slot) return;
    if (error.sessionId !== sessionId) error = { sessionId, text: "" };
    const h = html(model(changes), error.text);
    if (drawn.get(slot) === h) return; // unchanged: keep scroll position and focus
    drawn.set(slot, h);
    slot.innerHTML = h;
    slot.onchange = async (e) => {
      const box = e.target.closest("[data-change-file]");
      if (!box) return;
      box.disabled = true;
      try {
        await window.__TAURI__.core.invoke("set_project_change_selection", {
          sessionId,
          folder: box.dataset.folder,
          path: box.dataset.changeFile,
          include: box.checked,
        });
        error = { sessionId, text: "" };
      } catch (err) {
        error = { sessionId, text: `Couldn't change what is sent: ${err}` };
      }
      if (refresh) refresh();
    };
  }

  return { model, html, render, MAX_LINES };
})();

if (typeof module !== "undefined" && module.exports) {
  module.exports = { ChangesPanel };
}
