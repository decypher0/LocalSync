// Storage view (app menu -> LocalSync -> Storage…): what LocalSync has put on
// disk, per session, and removing what nothing uses any more. Backend:
// storage_commands.rs. Classic script sharing app.js's globals (invoke,
// listen, $, escapeHtml). The default view is one total, one "Free up space"
// button and the session list; everything else sits behind "Show details".
// Nothing is removed without a one-sentence confirmation first.

(() => {
  let report = null;
  let kept = []; // notes from the last removal, shown in details until the next scan
  let toastTimer = null;

  const size = (n) => {
    if (!n) return "0 B";
    const units = ["B", "KB", "MB", "GB", "TB"];
    let i = 0;
    while (n >= 1024 && i < units.length - 1) { n /= 1024; i++; }
    return `${n.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
  };
  const kindLabel = (k) => ({ volume: "Database volume", folder: "Received project", "test-run": "Test-run leftover", export: "Exported dump" })[k] || k;

  // What "Free up space" removes: every unused item the report listed, plus
  // LocalSync's own unused images (prune_localsync_images). Image sizes count
  // layers shared with base images in each, so their total is "up to".
  const cleanable = (r) => {
    const imgs = r.images && r.images.unused_count ? r.images : null;
    return { ids: r.unused.map((u) => u.id), imgs, bytes: r.unused_bytes + (imgs ? imgs.unused_bytes : 0), upTo: !!imgs };
  };
  const sessionBytes = (s) => s.dirs_bytes + s.volumes_bytes;

  function clearMessages() {
    $("storage-error").textContent = "";
    $("storage-toast").textContent = "";
    clearTimeout(toastTimer);
  }

  function openStorage() {
    for (const id of ["settings-panel", "session-history-panel"]) $(id).classList.add("hidden");
    $("storage-panel").classList.remove("hidden");
    scan();
  }

  async function scan() {
    clearMessages();
    hideConfirm();
    kept = [];
    $("storage-headline").textContent = "Scanning…";
    $("storage-refresh-btn").disabled = true;
    try {
      report = await invoke("storage_report");
      render();
    } catch (err) {
      report = null;
      $("storage-headline").textContent = "Storage";
      $("storage-body").innerHTML = "";
      $("storage-error").textContent = String(err);
    } finally {
      $("storage-refresh-btn").disabled = false;
    }
  }

  function render() {
    const r = report;
    const c = cleanable(r);
    const wasOpen = $("storage-details")?.open;
    $("storage-headline").textContent = `LocalSync is using ${size(r.total_bytes)}`;
    const free = c.ids.length || c.imgs
      ? `Free up space (${c.upTo ? "up to " : ""}${size(c.bytes)})`
      : "Nothing to clean up";
    const sessions = [...r.sessions].sort((a, b) => sessionBytes(b) - sessionBytes(a));
    const list = sessions.length
      ? `<ul class="st-sessions">${sessions
          .map((s) => `<li><span class="st-dot ${s.running ? "is-running" : ""}" title="${s.running ? "Running" : "Stopped"}"></span>
            <span class="st-name">${escapeHtml(s.name)}</span><span class="st-num">${size(sessionBytes(s))}</span>
            <button class="ghost-btn" type="button" data-free="${escapeHtml(s.id)}" ${!s.dirs.length && !s.volumes.length ? "disabled" : ""}>Delete</button></li>`)
          .join("")}</ul>`
      : `<p class="hint">No received sessions.</p>`;

    const item = (label, bytes, note = "") => `<li><span>${label}${note}</span><span class="st-num">${size(bytes)}</span></li>`;
    const img = r.images;
    const toRemove = c.ids.length || c.imgs
      ? `<ul class="ports-list st-list">${r.unused.map((u) => item(`${kindLabel(u.kind)}: <code>${escapeHtml(u.label)}</code>`, u.bytes)).join("")}${
          c.imgs ? item(`${c.imgs.unused_count} unused LocalSync image(s) (up to)`, c.imgs.unused_bytes) : ""}</ul>`
      : `<p class="hint">Nothing.</p>`;
    $("storage-body").innerHTML = `
      ${r.podman_error ? `<p class="error">Couldn't reach Podman, so database and image sizes are missing.</p>` : ""}
      <button id="storage-clean-btn" class="primary-btn st-free" type="button" ${c.ids.length || c.imgs ? "" : "disabled"}>${free}</button>
      ${list}
      <details id="storage-details" ${wasOpen ? "open" : ""}>
        <summary><span class="st-show">Show details</span><span class="st-hide">Hide details</span></summary>
        ${r.podman_error ? `<p class="hint">Podman: ${escapeHtml(r.podman_error)}</p>` : ""}
        <h3>Breakdown</h3>
        <table class="st-table"><tbody>${r.breakdown
          .map((p) => `<tr><td>${escapeHtml(p.label)}${p.note ? `<div class="hint-inline">${escapeHtml(p.note)}</div>` : ""}</td><td class="st-num">${size(p.bytes)}</td></tr>`)
          .join("")}</tbody></table>
        ${sessions.length ? `<h3>Sessions</h3>
        <table class="st-table"><thead><tr><th>Session</th><th class="st-num">Files</th><th class="st-num">Database</th></tr></thead><tbody>${sessions
          .map((s) => `<tr><td>${escapeHtml(s.name)}${[...s.dirs, ...s.volumes]
              .map((i) => `<div class="hint-inline"><code>${escapeHtml(i.name)}</code>${i.shared ? " (kept, another session uses it)" : ""}</div>`)
              .join("")}</td>
            <td class="st-num">${s.dirs.length ? size(s.dirs_bytes) : "—"}</td>
            <td class="st-num">${s.volumes.length ? size(s.volumes_bytes) : "—"}</td></tr>`)
          .join("")}</tbody></table>` : ""}
        <h3>Free up space removes</h3>
        ${toRemove}
        ${img ? `<h3>Podman images</h3>
        <p class="hint">${img.localsync_count} image(s) built by LocalSync, ${size(img.localsync_bytes)}; ${img.unused_count} not used by any container. Layers shared with base images are counted in each.</p>
        <p class="hint">Podman's image cache is shared by everything that uses Podman on this computer. Only images LocalSync built (localhost/localsync-…) that no container uses are removed; they are rebuilt on the next Run.</p>` : ""}
        <h3>Work folders</h3>
        <p class="hint">${r.work_dirs.map((d) => `<code>${escapeHtml(d)}</code>`).join(", ") || "none"}. Only folders LocalSync unpacked inside them are ever removed, never your project folders.</p>
        ${kept.length ? `<h3>Kept by the last cleanup</h3><ul class="ports-list st-list">${kept.map((k) => `<li>${escapeHtml(k)}</li>`).join("")}</ul>` : ""}
        <p class="hint">Scanned in ${(r.scan_ms / 1000).toFixed(1)} s.</p>
      </details>`;
  }

  // ---- confirmation ----
  let pending = null;
  function showConfirm(text, label, action) {
    clearMessages();
    pending = action;
    $("storage-confirm-text").textContent = text;
    $("storage-confirm-go").textContent = label;
    $("storage-confirm").classList.remove("hidden");
  }
  function hideConfirm() {
    pending = null;
    $("storage-confirm").classList.add("hidden");
  }

  async function act(fn) {
    hideConfirm();
    $("storage-confirm-go").disabled = true;
    let res = null, error = null;
    try { res = await fn(); } catch (err) { error = err; }
    await scan(); // a failed removal may still have removed some of it
    $("storage-confirm-go").disabled = false;
    if (error) { $("storage-error").textContent = String(error); return; }
    kept = res.kept;
    if (report && kept.length) render();
    $("storage-toast").textContent = `Freed ${size(res.freed_bytes)}${kept.length ? ` · ${kept.length} kept (see details)` : ""}`;
    toastTimer = setTimeout(() => { $("storage-toast").textContent = ""; }, 5000);
  }

  $("storage-body").addEventListener("click", (e) => {
    if (!report) return;
    const free = e.target.closest("[data-free]");
    if (free) {
      const s = report.sessions.find((x) => x.id === free.dataset.free);
      if (!s) return;
      const frees = [...s.dirs, ...s.volumes].filter((i) => !i.shared).reduce((n, i) => n + i.bytes, 0);
      showConfirm(
        `${s.running ? "Stop and delete" : "Delete"} the data for “${s.name}”? Frees ${size(frees)}.`,
        s.running ? "Stop and delete" : "Delete",
        () => invoke("free_session_disk", { sessionId: s.id }),
      );
      return;
    }
    if (e.target.closest("#storage-clean-btn")) {
      const c = cleanable(report);
      const what = [c.ids.length && "data from sessions you've closed", c.imgs && "unused LocalSync images"].filter(Boolean).join(" and ");
      showConfirm(`Free up ${c.upTo ? "to " : ""}${size(c.bytes)}? Removes ${what}.`, "Free up space", async () => {
        const out = { kept: [], freed_bytes: 0 };
        for (const res of [c.ids.length && (await invoke("clean_up_unused", { expected: c.ids })), c.imgs && (await invoke("prune_localsync_images"))]) {
          if (res) { out.kept.push(...res.kept); out.freed_bytes += res.freed_bytes; }
        }
        return out;
      });
    }
  });

  $("storage-confirm-go").addEventListener("click", () => pending && act(pending));
  $("storage-confirm-cancel").addEventListener("click", hideConfirm);
  $("storage-refresh-btn").addEventListener("click", scan);
  $("storage-close-btn").addEventListener("click", () => $("storage-panel").classList.add("hidden"));
  listen("menu-action", (evt) => { if (evt.payload === "menu-storage") openStorage(); });
  window.openStorage = openStorage;
})();
