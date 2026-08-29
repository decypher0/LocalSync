// Vanilla JS, no bundler: Tauri serves this directory as-is
// (`build.frontendDist` in tauri.conf.json) and `app.withGlobalTauri` is set
// so `window.__TAURI__` is available without an npm dependency.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open } = window.__TAURI__.dialog;

const $ = (id) => document.getElementById(id);

// ---------- preload (dev shortcut for LOCALSYNC_PRELOAD_SNAPSHOT) ----------
// Fires the same IncomingSnapshotInfo shape receive_snapshot resolves with,
// so this reuses renderReview verbatim — the review screen a developer sees
// this way is the same code path a real P2P receive draws, not a separate
// mockup. See scripts/demo-review-screen.sh.
listen("preload-review", (evt) => renderReview(evt.payload));

// ---------- firewall warning (Linux ufw active at startup) ----------
listen("firewall-warning", (evt) => {
  $("firewall-banner").textContent = evt.payload;
  $("firewall-banner").classList.remove("hidden");
});

// ---------- settings ----------
$("settings-toggle").addEventListener("click", () => {
  $("settings-panel").classList.toggle("hidden");
});
$("data-dir-display").value = "(read at launch; not editable here)";

// The Settings field is now an optional escape hatch (manual/advanced
// setup) rather than the normal path — normal Send/Receive derives
// room_id/signaling_url automatically via start_send_session/decode_room_code
// below. An empty override means "use automatic LAN discovery".
function manualSignalingUrl() {
  return $("signaling-url").value.trim();
}

// ---------- tabs ----------
document.querySelectorAll(".tab-btn").forEach((btn) => {
  btn.addEventListener("click", () => {
    document.querySelectorAll(".tab-btn").forEach((b) => b.classList.remove("active"));
    document.querySelectorAll(".tab-panel").forEach((p) => p.classList.remove("active"));
    btn.classList.add("active");
    $(`tab-${btn.dataset.tab}`).classList.add("active");
  });
});

// ---------- send ----------
$("browse-project-path").addEventListener("click", async () => {
  const dir = await open({ directory: true, multiple: false });
  if (dir) $("project-path").value = dir;
});

let unlistenSendProgress = null;

$("send-btn").addEventListener("click", async () => {
  const projectPath = $("project-path").value.trim();
  $("send-error").textContent = "";
  $("send-result").textContent = "";
  $("send-code-wrap").classList.add("hidden");

  if (!projectPath) {
    $("send-error").textContent = "Project path is required.";
    return;
  }

  $("send-btn").disabled = true;

  try {
    // start_send_session always hosts the relay + generates a room id (it's
    // cheap - one bound port). The Settings override, when set, replaces
    // only the signaling_url handed to share_snapshot below, so the app
    // still connects through the manually run server instead of the one
    // just hosted - the escape hatch this round preserves.
    const info = await invoke("start_send_session");
    const override = manualSignalingUrl();
    const roomId = info.room_id;
    const signalingUrl = override || info.signaling_url;
    $("send-room-code-display").textContent = override ? `${roomId} @ ${override}` : info.room_code;
    $("send-code-wrap").classList.remove("hidden");

    $("send-progress-wrap").classList.remove("hidden");
    setProgress("send-progress-bar", 0);
    $("send-progress-label").textContent = "Connecting to peer…";

    if (unlistenSendProgress) unlistenSendProgress();
    unlistenSendProgress = await listen("share-progress", (evt) => {
      const { bytes, total } = evt.payload;
      setProgress("send-progress-bar", total ? (bytes / total) * 100 : 0);
      $("send-progress-label").textContent = `Sending… ${formatBytes(bytes)} / ${formatBytes(total)}`;
    });

    const snapshotId = await invoke("share_snapshot", {
      projectPath,
      roomCode: roomId,
      signalingUrl,
    });
    $("send-progress-label").textContent = "Sent.";
    $("send-result").textContent = `Sent as ${snapshotId}`;
  } catch (err) {
    $("send-error").textContent = String(err);
  } finally {
    $("send-btn").disabled = false;
  }
});

// ---------- receive ----------
let unlistenReceiveProgress = null;
let currentSnapshotId = null;
let currentSessionId = null;

$("receive-btn").addEventListener("click", async () => {
  const roomCode = $("receive-room-code").value.trim();
  $("receive-error").textContent = "";
  if (!roomCode) {
    $("receive-error").textContent = "Room code is required.";
    return;
  }

  $("receive-btn").disabled = true;
  $("receive-progress-wrap").classList.remove("hidden");
  setProgress("receive-progress-bar", 0);
  $("receive-progress-label").textContent = "Waiting for sender…";

  if (unlistenReceiveProgress) unlistenReceiveProgress();
  unlistenReceiveProgress = await listen("receive-progress", (evt) => {
    const { bytes, total } = evt.payload;
    setProgress("receive-progress-bar", total ? (bytes / total) * 100 : 0);
    $("receive-progress-label").textContent = `Receiving… ${formatBytes(bytes)} / ${formatBytes(total)}`;
  });

  try {
    const override = manualSignalingUrl();
    let roomId;
    let signalingUrl;
    if (override) {
      // Escape hatch, same as today's behavior: the pasted value is the
      // room id itself, paired with the manually run signaling server.
      roomId = roomCode;
      signalingUrl = override;
    } else {
      const decoded = await invoke("decode_room_code", { code: roomCode });
      roomId = decoded.room_id;
      signalingUrl = decoded.signaling_url;
    }

    // This only verifies + diffs. Nothing from the snapshot executes until
    // the user reviews it below and clicks Run.
    const info = await invoke("receive_snapshot", {
      roomCode: roomId,
      signalingUrl,
    });
    renderReview(info);
  } catch (err) {
    $("receive-error").textContent = String(err);
  } finally {
    $("receive-btn").disabled = false;
  }
});

function renderReview(info) {
  currentSnapshotId = info.snapshot_id;
  const m = info.manifest;

  $("m-project").textContent = m.project_name;
  $("m-commit").textContent = m.git_commit;
  $("m-parent").textContent = m.git_parent_commit || "(none — initial snapshot)";
  $("m-services").textContent = m.services.map((s) => `${s.name} (${s.image_or_build})`).join(", ") || "none";

  const diff = info.diff;
  const fileCount = diff.entries.length;
  $("diff-totals").innerHTML =
    `<span class="file-count">${fileCount} file${fileCount === 1 ? "" : "s"} changed</span>` +
    `<span class="ins-del">+${diff.total_insertions} / -${diff.total_deletions}</span>`;

  const body = $("diff-body");
  body.innerHTML = "";

  // Group by directory (everything before the last "/" in the path) so a
  // real project with files spread across many dirs doesn't render as one
  // flat wall. Files with no "/" (repo-root files) get their own bucket.
  const groups = new Map();
  for (const entry of diff.entries) {
    const slash = entry.path.lastIndexOf("/");
    const dir = slash === -1 ? "(root)" : entry.path.slice(0, slash);
    if (!groups.has(dir)) groups.set(dir, []);
    groups.get(dir).push(entry);
  }
  const dirs = [...groups.keys()].sort((a, b) =>
    a === "(root)" ? -1 : b === "(root)" ? 1 : a.localeCompare(b)
  );

  // Small diffs (<=12 files total): open everything, nothing to hide.
  // Larger diffs: only auto-collapse the directories that are themselves
  // large (>5 files) — small groups stay open since they're cheap to scan.
  const openAll = fileCount <= 12;

  for (const dir of dirs) {
    const entries = groups.get(dir);
    const details = document.createElement("details");
    details.className = "diff-group";
    details.open = openAll || entries.length <= 5;

    const summary = document.createElement("summary");
    summary.innerHTML =
      `<span class="diff-group-name">${escapeHtml(dir)}</span>` +
      `<span class="diff-group-count">${entries.length} file${entries.length === 1 ? "" : "s"}</span>`;
    details.appendChild(summary);

    const table = document.createElement("table");
    table.className = "diff-table";
    table.innerHTML = "<thead><tr><th>File</th><th>Change</th><th>+</th><th>-</th></tr></thead><tbody></tbody>";
    const tbody = table.querySelector("tbody");
    for (const entry of entries) {
      // Show the path relative to its group heading — the directory is
      // already shown once, in the summary.
      const name = dir === "(root)" ? entry.path : entry.path.slice(dir.length + 1);
      const tr = document.createElement("tr");
      tr.innerHTML = `
        <td class="path">${escapeHtml(name)}</td>
        <td><span class="badge ${entry.change_type}">${entry.change_type}</span></td>
        <td class="ins">+${entry.insertions}</td>
        <td class="del">-${entry.deletions}</td>
      `;
      tbody.appendChild(tr);
    }
    details.appendChild(table);
    body.appendChild(details);
  }

  $("receive-idle").classList.add("hidden");
  $("review-panel").classList.remove("hidden");
  $("session-panel").classList.add("hidden");
}

let unlistenRunProgress = null;

// Collapsed by default — toggling only shows/hides the log already
// accumulated in #run-log, doesn't (re)fetch anything.
$("run-details-toggle").addEventListener("click", () => {
  const expanded = !$("run-log").classList.contains("hidden");
  $("run-log").classList.toggle("hidden");
  $("run-details-toggle").textContent = expanded ? "Show details ▾" : "Hide details ▲";
  $("run-details-toggle").setAttribute("aria-expanded", String(!expanded));
});

$("run-btn").addEventListener("click", async () => {
  $("run-error").textContent = "";
  const workDir = $("work-dir").value.trim();
  if (!currentSnapshotId || !workDir) return;

  $("run-btn").disabled = true;
  $("run-progress-wrap").classList.remove("hidden");
  $("run-progress-label").textContent = "Starting containers…";
  // Fresh per attempt — retrying after a fixed environment problem
  // shouldn't show last attempt's log lines glued onto this one.
  $("run-log").textContent = "";
  $("run-log").classList.add("hidden");
  $("run-details-toggle").textContent = "Show details ▾";
  $("run-details-toggle").setAttribute("aria-expanded", "false");

  // Registered before invoke so no early line from the backend's tailer is
  // missed.
  if (unlistenRunProgress) unlistenRunProgress();
  unlistenRunProgress = await listen("run-progress", (evt) => {
    const log = $("run-log");
    log.textContent += (log.textContent ? "\n" : "") + evt.payload.line;
    log.scrollTop = log.scrollHeight;
  });

  try {
    // The one call in this app that executes received code — only reachable
    // from this explicit click, after the diff above has been shown.
    const session = await invoke("run_snapshot", {
      snapshotId: currentSnapshotId,
      workDir,
    });
    renderSession(session);
  } catch (err) {
    $("run-error").textContent = String(err);
  } finally {
    $("run-btn").disabled = false;
    $("run-progress-wrap").classList.add("hidden");
    if (unlistenRunProgress) {
      unlistenRunProgress();
      unlistenRunProgress = null;
    }
  }
});

function renderSession(session) {
  currentSessionId = session.session_id;
  $("s-project").textContent = session.project_name;

  const cacheEl = $("s-cache");
  if (session.db_cache_hit) {
    cacheEl.textContent = "Hit — reused seeded volume (fast start)";
    cacheEl.className = "v cache-hit";
  } else {
    cacheEl.textContent = "Miss — cold start, seeded from scratch";
    cacheEl.className = "v cache-cold";
  }

  const list = $("s-ports");
  list.innerHTML = "";
  for (const [service, ports] of session.service_ports) {
    const hostPort = ports.split(":")[0];
    const li = document.createElement("li");
    li.innerHTML = `${escapeHtml(service)} — <a href="#" data-url="http://localhost:${hostPort}">http://localhost:${hostPort}</a> (${escapeHtml(ports)})`;
    list.appendChild(li);
  }

  $("review-panel").classList.add("hidden");
  $("session-panel").classList.remove("hidden");
}

$("stop-btn").addEventListener("click", async () => {
  $("stop-error").textContent = "";
  if (!currentSessionId) return;
  $("stop-btn").disabled = true;
  try {
    await invoke("stop_session", { sessionId: currentSessionId });
    currentSessionId = null;
    currentSnapshotId = null;
    $("session-panel").classList.add("hidden");
    $("receive-idle").classList.remove("hidden");
    $("receive-progress-wrap").classList.add("hidden");
  } catch (err) {
    $("stop-error").textContent = String(err);
  } finally {
    $("stop-btn").disabled = false;
  }
});

// ---------- helpers ----------
function setProgress(id, pct) {
  $(id).style.width = `${Math.max(0, Math.min(100, pct))}%`;
}

function formatBytes(n) {
  if (!n && n !== 0) return "0 B";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(2)} MB`;
}

function escapeHtml(s) {
  const div = document.createElement("div");
  div.textContent = s;
  return div.innerHTML;
}
