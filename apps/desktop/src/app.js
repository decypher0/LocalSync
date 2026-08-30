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

// ---------- pull requests (sender-side: a connected receiver asked "anything new?") ----------
// Registered once, globally — a request can arrive on any tab. Rendered as a
// dismissible row per pending peer_id, since more than one receiver could ask
// at once; removed once Accept/Decline is handled.
listen("pull-request", (evt) => {
  const peerId = evt.payload.peer_id;
  const existing = [...$("pull-requests").children].find((el) => el.dataset.peer === peerId);
  if (existing) return; // already showing a pending request for this peer
  const div = document.createElement("div");
  div.dataset.peer = peerId;
  div.className = "peer-banner";
  div.innerHTML = `
    <span>Pull request from <strong>${escapeHtml(peerId)}</strong></span>
    <span class="inline-row">
      <button class="ghost-btn accept-btn" type="button">Accept</button>
      <button class="ghost-btn decline-btn" type="button">Decline</button>
    </span>
    <p class="error"></p>
  `;
  const errorEl = div.querySelector(".error");
  const respond = async (accept) => {
    div.querySelectorAll("button").forEach((b) => (b.disabled = true));
    try {
      await invoke("respond_to_pull_request", { peerId, accept });
      div.remove();
      if (accept) refreshReceivers();
    } catch (err) {
      errorEl.textContent = String(err);
      div.querySelectorAll("button").forEach((b) => (b.disabled = false));
    }
  };
  div.querySelector(".accept-btn").addEventListener("click", () => respond(true));
  div.querySelector(".decline-btn").addEventListener("click", () => respond(false));
  $("pull-requests").appendChild(div);
});

// ---------- snapshot updated (receiver-side: sender pushed a fresh snapshot) ----------
// Same payload shape receive_snapshot resolves with, so this reuses
// renderReview verbatim - a pushed update goes through the exact same
// diff-review-then-Run/Reject screen as a normal receive.
let updateBannerTimeout = null;
listen("snapshot-updated", (evt) => {
  renderReview(evt.payload);
  const banner = $("update-banner");
  banner.classList.remove("hidden");
  clearTimeout(updateBannerTimeout);
  updateBannerTimeout = setTimeout(() => banner.classList.add("hidden"), 4000);
});

// ---------- settings ----------
$("settings-toggle").addEventListener("click", () => {
  $("settings-panel").classList.toggle("hidden");
});
$("data-dir-display").value = "(read at launch; not editable here)";

// ---------- relay mode (persisted in localStorage — set once, survives restarts) ----------
const MODE_KEY = "localsync.relayMode";
const RELAY_URL_KEY = "localsync.relayUrl";

function relayMode() {
  return $("mode-remote").checked ? "remote" : "local";
}

function relayUrl() {
  return $("relay-url").value.trim();
}

function updateModeUi() {
  $("relay-url-wrap").classList.toggle("hidden", relayMode() !== "remote");
}

$("mode-local").addEventListener("change", () => {
  updateModeUi();
  localStorage.setItem(MODE_KEY, relayMode());
});
$("mode-remote").addEventListener("change", () => {
  updateModeUi();
  localStorage.setItem(MODE_KEY, relayMode());
});
$("relay-url").addEventListener("input", () => {
  localStorage.setItem(RELAY_URL_KEY, relayUrl());
});

// Restore persisted mode/URL on load.
if (localStorage.getItem(MODE_KEY) === "remote") $("mode-remote").checked = true;
$("relay-url").value = localStorage.getItem(RELAY_URL_KEY) || "";
updateModeUi();

// ---------- tabs ----------
document.querySelectorAll(".tab-btn").forEach((btn) => {
  btn.addEventListener("click", () => {
    document.querySelectorAll(".tab-btn").forEach((b) => b.classList.remove("active"));
    document.querySelectorAll(".tab-panel").forEach((p) => p.classList.remove("active"));
    btn.classList.add("active");
    $(`tab-${btn.dataset.tab}`).classList.add("active");
    // Show "Previously connected" the moment someone looks at the Send tab,
    // not only after they've just sent something or clicked Refresh by hand.
    if (btn.dataset.tab === "send") refreshReceivers();
  });
});

// ---------- send ----------
$("browse-project-path").addEventListener("click", async () => {
  const dir = await open({ directory: true, multiple: false });
  if (dir) $("project-path").value = dir;
});

let unlistenSendProgress = null;
let codeExpiryInterval = null;

// Visible countdown instead of a silent background timer (round 12) - the
// room code is only good until the sender's connect_as_sender call (started
// right after this, by share_snapshot below) gives up waiting for a peer.
// See commands::SendSessionInfo's code_expires_in_seconds doc comment.
function startCodeExpiryCountdown(totalSeconds) {
  clearInterval(codeExpiryInterval);
  const el = $("send-code-expiry");
  let remaining = totalSeconds;
  const render = () => {
    if (remaining <= 0) {
      el.textContent = "Code expired — click Send again for a new one.";
      el.className = "hint-inline error-inline";
      clearInterval(codeExpiryInterval);
      return;
    }
    const m = Math.floor(remaining / 60);
    const s = remaining % 60;
    el.textContent = `Expires in ${m}:${String(s).padStart(2, "0")} — share it before then.`;
    el.className = "hint-inline";
  };
  render();
  codeExpiryInterval = setInterval(() => {
    remaining -= 1;
    render();
  }, 1000);
}
function stopCodeExpiryCountdown() {
  clearInterval(codeExpiryInterval);
  $("send-code-expiry").textContent = "";
}

$("send-btn").addEventListener("click", async () => {
  const projectPath = $("project-path").value.trim();
  $("send-error").textContent = "";
  $("send-result").textContent = "";
  $("send-code-wrap").classList.add("hidden");
  stopCodeExpiryCountdown();

  if (!projectPath) {
    $("send-error").textContent = "Project path is required.";
    return;
  }

  const mode = relayMode();
  const url = relayUrl();
  if (mode === "remote" && !url) {
    $("send-error").textContent = "Remote relay URL is required in Settings for Remote relay mode.";
    return;
  }

  $("send-btn").disabled = true;

  try {
    // "local": hosts an embedded relay + derives a LAN-IP-encoded room code
    // (unchanged round-8 behavior). "remote": a relay is already running
    // elsewhere (see README) - only a bare room id is generated, and it IS
    // the whole paste-able code, since both apps already share the relay URL.
    const info = await invoke("start_send_session", { mode, relayUrl: mode === "remote" ? url : null });
    const roomId = info.room_id;
    const signalingUrl = info.signaling_url;
    $("send-room-code-display").textContent = info.room_code;
    $("send-code-wrap").classList.remove("hidden");
    startCodeExpiryCountdown(info.code_expires_in_seconds);

    $("send-progress-wrap").classList.remove("hidden");
    setProgress("send-progress-bar", 0);
    $("send-progress-label").textContent = "Connecting to peer…";

    if (unlistenSendProgress) unlistenSendProgress();
    unlistenSendProgress = await listen("share-progress", (evt) => {
      // First progress event means a peer actually connected and the
      // transfer started - the code did its job, no need to keep
      // frightening the user with a ticking clock.
      stopCodeExpiryCountdown();
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
    refreshReceivers(); // this send may have just added a new roster entry
  } catch (err) {
    $("send-error").textContent = String(err);
    stopCodeExpiryCountdown();
  } finally {
    $("send-btn").disabled = false;
  }
});

// ---------- connected receivers roster (sender-side) ----------
async function refreshReceivers() {
  $("receivers-error").textContent = "";
  try {
    const list = await invoke("list_connected_receivers");
    $("receivers-wrap").classList.remove("hidden");
    const ul = $("receivers-list");
    ul.innerHTML = "";
    for (const r of list) {
      const li = document.createElement("li");
      li.innerHTML = `
        <span class="mono">${escapeHtml(r.peer_id)}</span>
        <span class="hint-inline">connected ${escapeHtml(r.connected_at)}</span>
        <button class="ghost-btn push-btn" type="button" data-peer="${escapeHtml(r.peer_id)}">Push update</button>
        <span class="hint push-status"></span>
      `;
      ul.appendChild(li);
    }
  } catch (err) {
    $("receivers-error").textContent = String(err);
  }
}
$("receivers-refresh-btn").addEventListener("click", refreshReceivers);

// Delegated so newly-rendered rows don't need their own listener wiring.
$("receivers-list").addEventListener("click", async (e) => {
  const btn = e.target.closest(".push-btn");
  if (!btn) return;
  const peerId = btn.dataset.peer;
  const status = btn.nextElementSibling;
  btn.disabled = true;
  status.textContent = "";
  status.className = "hint push-status";
  try {
    const snapshotId = await invoke("push_update", { peerId });
    status.textContent = `Pushed ${snapshotId}`;
    status.className = "result push-status";
  } catch (err) {
    status.textContent = String(err);
    status.className = "error push-status";
  } finally {
    btn.disabled = false;
  }
});

// ---------- receive ----------
let unlistenReceiveProgress = null;
let currentSnapshotId = null;
let currentSenderPubkeyHex = null;
let currentSessionId = null;

$("receive-btn").addEventListener("click", async () => {
  const roomCode = $("receive-room-code").value.trim();
  $("receive-error").textContent = "";
  if (!roomCode) {
    $("receive-error").textContent = "Room code is required.";
    return;
  }

  const mode = relayMode();
  const url = relayUrl();
  if (mode === "remote" && !url) {
    $("receive-error").textContent = "Remote relay URL is required in Settings for Remote relay mode.";
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
    const decoded = await invoke("decode_room_code", { mode, code: roomCode, relayUrl: mode === "remote" ? url : null });
    const roomId = decoded.room_id;
    const signalingUrl = decoded.signaling_url;

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
  currentSenderPubkeyHex = info.sender_pubkey_hex;
  const m = info.manifest;

  $("m-project").textContent = m.project_name;
  $("m-commit").textContent = m.git_commit;
  $("m-parent").textContent = m.git_parent_commit || "(none — initial snapshot)";
  $("m-services").textContent = m.services.map((s) => `${s.name} (${s.image_or_build})`).join(", ") || "none";

  // Identity recognition is purely informational - it never affects what's
  // shown below or what Run/Reject do. See commands::finalize_received_snapshot.
  $("peer-remember-done").classList.add("hidden");
  $("peer-remember-error").textContent = "";
  $("peer-remember-name").value = "";
  if (info.recognized_peer) {
    $("peer-recognized").classList.remove("hidden");
    $("peer-new").classList.add("hidden");
    $("peer-recognized-name").textContent = info.recognized_peer.name;
    $("peer-recognized-since").textContent = `(first seen ${info.recognized_peer.first_seen})`;
  } else {
    $("peer-recognized").classList.add("hidden");
    $("peer-new").classList.remove("hidden");
  }

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

$("peer-remember-btn").addEventListener("click", async () => {
  $("peer-remember-error").textContent = "";
  const name = $("peer-remember-name").value.trim();
  if (!name || !currentSenderPubkeyHex) return;
  $("peer-remember-btn").disabled = true;
  try {
    await invoke("remember_peer", { pubkeyHex: currentSenderPubkeyHex, name });
    $("peer-remember-done").classList.remove("hidden");
  } catch (err) {
    $("peer-remember-done").classList.add("hidden");
    $("peer-remember-error").textContent = String(err);
  } finally {
    $("peer-remember-btn").disabled = false;
  }
});

$("ask-update-btn").addEventListener("click", async () => {
  const status = $("ask-update-status");
  status.textContent = "";
  status.className = "hint";
  $("ask-update-btn").disabled = true;
  try {
    await invoke("send_pull_request");
    status.textContent = "Request sent.";
  } catch (err) {
    status.textContent = String(err);
    status.className = "error";
  } finally {
    $("ask-update-btn").disabled = false;
  }
});

// Connection-level "no" - discards the held snapshot without ever running
// it, independent of (and no shortcut past) the Run button's own gating.
$("reject-btn").addEventListener("click", async () => {
  $("reject-error").textContent = "";
  if (!currentSnapshotId) return;
  $("reject-btn").disabled = true;
  try {
    await invoke("reject_snapshot", { snapshotId: currentSnapshotId });
    currentSnapshotId = null;
    currentSenderPubkeyHex = null;
    $("review-panel").classList.add("hidden");
    $("receive-idle").classList.remove("hidden");
    $("receive-progress-wrap").classList.add("hidden");
  } catch (err) {
    $("reject-error").textContent = String(err);
  } finally {
    $("reject-btn").disabled = false;
  }
});

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
  document.querySelector("#run-progress-wrap .spinner")?.classList.remove("hidden");
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
    // Only hide the progress/details panel on success - renderSession has
    // already moved the user on to the running-session screen, so there's
    // nothing left in it worth keeping visible.
    $("run-progress-wrap").classList.add("hidden");
  } catch (err) {
    $("run-error").textContent = String(err);
    $("run-progress-label").textContent = "Failed — see details below.";
    document.querySelector("#run-progress-wrap .spinner")?.classList.add("hidden");
    // Round 12: deliberately does NOT hide #run-progress-wrap here. This
    // used to happen unconditionally in `finally` below, which meant the
    // one moment the streamed log content was most useful - right after a
    // real failure, when it likely explains why - it vanished instantly,
    // leaving only the bare error string. Left visible (spinner included,
    // which just stops looking meaningful - a cosmetic wart, not worth
    // extra code to also swap it for a static icon) until the next Run
    // attempt clears it at the top of this handler.
  } finally {
    $("run-btn").disabled = false;
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
