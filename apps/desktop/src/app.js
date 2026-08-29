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

// ---------- settings ----------
$("settings-toggle").addEventListener("click", () => {
  $("settings-panel").classList.toggle("hidden");
});
$("data-dir-display").value = "(read at launch; not editable here)";

function signalingUrl() {
  return $("signaling-url").value.trim() || "ws://localhost:9090";
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

function randomRoomCode() {
  return Math.floor(1000 + Math.random() * 9000).toString();
}
$("generate-room-code").addEventListener("click", () => {
  $("send-room-code").value = randomRoomCode();
});

// ---------- send ----------
$("browse-project-path").addEventListener("click", async () => {
  const dir = await open({ directory: true, multiple: false });
  if (dir) $("project-path").value = dir;
});

let unlistenSendProgress = null;

$("send-btn").addEventListener("click", async () => {
  const projectPath = $("project-path").value.trim();
  const roomCode = $("send-room-code").value.trim();
  $("send-error").textContent = "";
  $("send-result").textContent = "";

  if (!projectPath || !roomCode) {
    $("send-error").textContent = "Project path and room code are both required.";
    return;
  }

  $("send-btn").disabled = true;
  $("send-progress-wrap").classList.remove("hidden");
  setProgress("send-progress-bar", 0);
  $("send-progress-label").textContent = "Connecting to peer…";

  if (unlistenSendProgress) unlistenSendProgress();
  unlistenSendProgress = await listen("share-progress", (evt) => {
    const { bytes, total } = evt.payload;
    setProgress("send-progress-bar", total ? (bytes / total) * 100 : 0);
    $("send-progress-label").textContent = `Sending… ${formatBytes(bytes)} / ${formatBytes(total)}`;
  });

  try {
    const snapshotId = await invoke("share_snapshot", {
      projectPath,
      roomCode,
      signalingUrl: signalingUrl(),
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
    // This only verifies + diffs. Nothing from the snapshot executes until
    // the user reviews it below and clicks Run.
    const info = await invoke("receive_snapshot", {
      roomCode,
      signalingUrl: signalingUrl(),
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
  $("diff-totals").textContent = `+${diff.total_insertions} / -${diff.total_deletions} across ${diff.entries.length} file(s)`;

  const body = $("diff-body");
  body.innerHTML = "";
  for (const entry of diff.entries) {
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td class="path">${escapeHtml(entry.path)}</td>
      <td><span class="badge ${entry.change_type}">${entry.change_type}</span></td>
      <td class="ins">+${entry.insertions}</td>
      <td class="del">-${entry.deletions}</td>
    `;
    body.appendChild(tr);
  }

  $("receive-idle").classList.add("hidden");
  $("review-panel").classList.remove("hidden");
  $("session-panel").classList.add("hidden");
}

$("run-btn").addEventListener("click", async () => {
  $("run-error").textContent = "";
  const workDir = $("work-dir").value.trim();
  if (!currentSnapshotId || !workDir) return;

  $("run-btn").disabled = true;
  $("run-btn").textContent = "Starting containers…";
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
    $("run-btn").textContent = "Run";
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
