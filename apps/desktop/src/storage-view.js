// Storage view (app menu -> LocalSync -> Storage…): what LocalSync has put on
// disk, per session, and removing what nothing uses any more. Backend:
// storage_commands.rs. Classic script sharing app.js's globals (invoke,
// listen, $, escapeHtml). Every removal shows exactly what goes and how much
// space it frees, and waits for a click, before anything is removed.

(() => {
  let report = null;

  const size = (n) => {
    if (!n) return "0 B";
    const units = ["B", "KB", "MB", "GB", "TB"];
    let i = 0;
    while (n >= 1024 && i < units.length - 1) { n /= 1024; i++; }
    return `${n.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
  };

  function openStorage() {
    for (const id of ["settings-panel", "session-history-panel"]) $(id).classList.add("hidden");
    $("storage-panel").classList.remove("hidden");
    scan();
  }

  async function scan() {
    $("storage-status").textContent = "Scanning…";
    $("storage-error").textContent = "";
    $("storage-refresh-btn").disabled = true;
    hideConfirm();
    try {
      report = await invoke("storage_report");
      render();
      $("storage-status").textContent = `Scanned in ${(report.scan_ms / 1000).toFixed(1)} s.`;
    } catch (err) {
      $("storage-status").textContent = "";
      $("storage-error").textContent = String(err);
    } finally {
      $("storage-refresh-btn").disabled = false;
    }
  }

  function render() {
    const r = report;
    const rows = r.breakdown
      .map((p) => `<tr><td>${escapeHtml(p.label)}${p.note ? `<div class="hint-inline">${escapeHtml(p.note)}</div>` : ""}</td><td class="st-num">${size(p.bytes)}</td></tr>`)
      .join("");
    const sessions = r.sessions.length
      ? `<table class="st-table"><thead><tr><th>Session</th><th>Status</th><th class="st-num">Files</th><th class="st-num">Database</th><th></th></tr></thead><tbody>${r.sessions
          .map((s) => {
            const empty = !s.dirs.length && !s.volumes.length;
            const shared = s.volumes.some((v) => v.shared) ? ` <span class="hint-inline">(shared)</span>` : "";
            return `<tr><td>${escapeHtml(s.name)}</td><td>${s.running ? "Running" : "Stopped"}</td>
              <td class="st-num">${s.dirs.length ? size(s.dirs_bytes) : "—"}</td>
              <td class="st-num">${s.volumes.length ? size(s.volumes_bytes) + shared : "—"}</td>
              <td><button class="ghost-btn" type="button" data-free="${escapeHtml(s.id)}" ${empty ? "disabled" : ""}>Free up disk</button></td></tr>`;
          })
          .join("")}</tbody></table>`
      : `<p class="hint">No received sessions in the list.</p>`;
    const unused = r.unused.length
      ? `<ul class="ports-list st-list">${r.unused.map((u) => `<li><span>${kindLabel(u.kind)}: <code>${escapeHtml(u.label)}</code></span><span class="st-num">${size(u.bytes)}</span></li>`).join("")}</ul>
         <button id="storage-clean-btn" class="ghost-btn" type="button">Clean up unused (${size(r.unused_bytes)})</button>`
      : `<p class="hint">Nothing unused.</p>`;
    const img = r.images;
    const images = img
      ? `<p class="hint">${img.localsync_count} image(s) built by LocalSync, ${size(img.localsync_bytes)} (layers shared with base images are counted in each).
           ${img.unused_count} not used by any container.</p>
         <p class="hint st-warn">Podman's image cache is shared by everything that uses Podman on this computer. This only removes images LocalSync built (localhost/localsync-…) that no container uses; they are rebuilt on the next Run.</p>
         <button id="storage-prune-btn" class="ghost-btn" type="button" ${img.unused_count ? "" : "disabled"}>Remove unused LocalSync images</button>`
      : "";
    $("storage-body").innerHTML = `
      ${r.podman_error ? `<p class="error">Couldn't ask Podman (${escapeHtml(r.podman_error)}), so volume and image sizes are missing.</p>` : ""}
      <h3>Total: ${size(r.total_bytes)}</h3>
      <table class="st-table"><tbody>${rows}</tbody></table>
      <h3>Sessions</h3>${sessions}
      <h3>Unused</h3>
      <p class="hint">Volumes and folders that belong to no session in the list (for example from sessions you closed).</p>
      ${unused}
      ${images ? `<h3>Podman images</h3>${images}` : ""}
      <p class="hint">Work folders scanned: ${r.work_dirs.map((d) => `<code>${escapeHtml(d)}</code>`).join(", ") || "none"}. Only folders LocalSync unpacked inside them are ever removed, never your project folders.</p>
      <p id="storage-result" class="hint"></p>`;
  }

  const kindLabel = (k) => ({ volume: "Database volume", folder: "Received project", "test-run": "Test-run leftover", export: "Exported dump" })[k] || k;

  // ---- confirmation ----
  let pending = null;
  function showConfirm(html, label, action) {
    pending = action;
    $("storage-confirm-text").innerHTML = html;
    $("storage-confirm-go").textContent = label;
    $("storage-confirm").classList.remove("hidden");
    $("storage-confirm").scrollIntoView({ block: "nearest" });
  }
  function hideConfirm() {
    pending = null;
    $("storage-confirm").classList.add("hidden");
  }

  function showResult(res) {
    const kept = res.kept.length ? ` Kept: ${res.kept.map(escapeHtml).join("; ")}.` : "";
    return `Freed ${size(res.freed_bytes)} (${res.removed.length} item(s) removed).${kept}`;
  }

  async function act(fn) {
    $("storage-error").textContent = "";
    $("storage-confirm-go").disabled = true;
    try {
      const res = await fn();
      await scan();
      $("storage-result").innerHTML = showResult(res);
    } catch (err) {
      $("storage-error").textContent = String(err);
    } finally {
      $("storage-confirm-go").disabled = false;
      hideConfirm();
    }
  }

  $("storage-body").addEventListener("click", (e) => {
    if (!report) return;
    const free = e.target.closest("[data-free]");
    if (free) {
      const s = report.sessions.find((x) => x.id === free.dataset.free);
      if (!s) return;
      const items = [...s.dirs.map((d) => ({ ...d, kind: "folder" })), ...s.volumes.map((v) => ({ ...v, kind: "volume" }))];
      const total = items.filter((i) => !i.shared).reduce((n, i) => n + i.bytes, 0);
      const warn = s.running ? `<p class="error">“${escapeHtml(s.name)}” is running. Its containers will be stopped first.</p>` : "";
      showConfirm(
        `${warn}<p>Remove these for “${escapeHtml(s.name)}”? The session stays in the list; running it again needs it to be received again.</p>
         <ul class="ports-list st-list">${items.map((i) => `<li><span>${kindLabel(i.kind)}: <code>${escapeHtml(i.name)}</code>${i.shared ? " — kept, another session uses it" : ""}</span><span class="st-num">${size(i.bytes)}</span></li>`).join("")}</ul>
         <p>Frees about ${size(total)}.</p>`,
        s.running ? "Stop and free up disk" : "Free up disk",
        () => invoke("free_session_disk", { sessionId: s.id }),
      );
      return;
    }
    if (e.target.closest("#storage-clean-btn")) {
      const ids = report.unused.map((u) => u.id);
      showConfirm(
        `<p>Remove these? They belong to no session in the list.</p>
         <ul class="ports-list st-list">${report.unused.map((u) => `<li><span>${kindLabel(u.kind)}: <code>${escapeHtml(u.label)}</code></span><span class="st-num">${size(u.bytes)}</span></li>`).join("")}</ul>
         <p>Frees ${size(report.unused_bytes)}.</p>`,
        "Remove",
        () => invoke("clean_up_unused", { expected: ids }),
      );
      return;
    }
    if (e.target.closest("#storage-prune-btn")) {
      const img = report.images;
      showConfirm(
        `<p class="st-warn">This affects Podman's shared image cache. Only images LocalSync built that no container uses are removed (${img.unused_count}, up to ${size(img.unused_bytes)}); other Podman projects' images are left alone, but layers they share are only freed once nothing uses them.</p>`,
        "Remove images",
        () => invoke("prune_localsync_images"),
      );
    }
  });

  $("storage-confirm-go").addEventListener("click", () => pending && act(pending));
  $("storage-confirm-cancel").addEventListener("click", hideConfirm);
  $("storage-refresh-btn").addEventListener("click", scan);
  $("storage-close-btn").addEventListener("click", () => $("storage-panel").classList.add("hidden"));
  listen("menu-action", (evt) => { if (evt.payload === "menu-storage") openStorage(); });
  window.openStorage = openStorage;
})();
