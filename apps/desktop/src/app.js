// Vanilla JS, no bundler: Tauri serves this directory as-is
// (`build.frontendDist` in tauri.conf.json) and `app.withGlobalTauri` is set
// so `window.__TAURI__` is available without an npm dependency. Plugin
// namespaces (dialog/updater/process below) are real objects here at this
// point in execution - verified directly with a real running app rather
// than assumed, since they're non-enumerable properties (don't show up in
// `Object.keys(window.__TAURI__)`, which only lists the core API - app,
// core, event, ... - misleadingly suggesting they're missing if you check
// that way instead of a direct property/typeof check).
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open } = window.__TAURI__.dialog;
const { check: checkForUpdate } = window.__TAURI__.updater;
const { relaunch } = window.__TAURI__.process;

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

// ---------- round 16: auto-update (checking is silent; installing is never) ----------
// pendingUpdate holds the real Update object check() returned - only
// downloadAndInstall() (called from the Install button below, never
// automatically) actually fetches or applies anything. Checking alone -
// on launch, or via the Settings button - never downloads/installs by
// itself, matching the same explicit-consent shape every other
// state-changing action in this app already holds to (Run, Reject, Push
// update, ...).
let pendingUpdate = null;

async function runUpdateCheck(reportStatus) {
  try {
    const update = await checkForUpdate();
    if (update) {
      pendingUpdate = update;
      $("update-banner-text").textContent = `Update available: v${update.version}`;
      $("app-update-banner").classList.remove("hidden");
      if (reportStatus) $("check-updates-status").textContent = `v${update.version} available.`;
    } else {
      pendingUpdate = null;
      if (reportStatus) $("check-updates-status").textContent = "You're up to date.";
    }
  } catch (err) {
    // Quiet on the launch check (no endpoint configured yet, offline, a
    // dev build with no matching release, etc. shouldn't nag on startup) -
    // only surfaced when the user explicitly asked, via the Settings button.
    if (reportStatus) $("check-updates-status").textContent = String(err);
  }
}

// Launch check: a few seconds after startup, not the very first thing
// competing with initial render, and quiet either way (see reportStatus
// above) - if one's found, the banner appears; if not, or if it fails,
// nothing interrupts anyone.
setTimeout(() => runUpdateCheck(false), 3000);

$("check-updates-btn").addEventListener("click", () => {
  $("check-updates-status").textContent = "Checking…";
  runUpdateCheck(true);
});

$("update-dismiss-btn").addEventListener("click", () => {
  $("app-update-banner").classList.add("hidden");
});

$("update-install-btn").addEventListener("click", async () => {
  if (!pendingUpdate) return;
  $("update-install-btn").disabled = true;
  $("update-dismiss-btn").disabled = true;
  $("update-error").textContent = "";
  $("update-progress-wrap").classList.remove("hidden");
  let total = 0;
  let downloaded = 0;
  try {
    // The one call in this whole flow that actually fetches/applies
    // anything - only reachable from this explicit click.
    await pendingUpdate.downloadAndInstall((event) => {
      switch (event.event) {
        case "Started":
          total = event.data.contentLength || 0;
          $("update-progress-label").textContent = "Downloading update…";
          break;
        case "Progress":
          downloaded += event.data.chunkLength;
          setProgress("update-progress-bar", total ? (downloaded / total) * 100 : 0);
          $("update-progress-label").textContent = `Downloading… ${formatBytes(downloaded)}${total ? ` / ${formatBytes(total)}` : ""}`;
          break;
        case "Finished":
          setProgress("update-progress-bar", 100);
          $("update-progress-label").textContent = "Installing…";
          break;
      }
    });
    $("update-progress-label").textContent = "Update installed — restarting…";
    await relaunch();
  } catch (err) {
    $("update-error").textContent = String(err);
    $("update-install-btn").disabled = false;
    $("update-dismiss-btn").disabled = false;
  }
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

// ---------- send: round 17 database-source wizard ----------
//
// wizardFolders is the one source of truth for the whole wizard: each entry
// is { path, needsDb, details, sourceLabel, dump } where `dump`, once set,
// is either { schema, filePath } from a developer-supplied file or from a
// real export — share_snapshot_wizard treats both identically, so nothing
// downstream needs to know which one it was.
let wizardFolders = [];
let wizardFolderIndex = 0;
let wizardDbSubState = null; // tracks the per-folder sub-panel for Back
let wizardSelectedTables = [];

function wizFolderLabel(path) {
  return path.split(/[\\/]/).filter(Boolean).pop() || path;
}

function showWizardStep(id) {
  document.querySelectorAll("#send-wizard .wizard-step").forEach((el) => el.classList.add("hidden"));
  $(id).classList.remove("hidden");
}

function hideAllDbSubPanels() {
  ["wiz-db-detecting", "wiz-db-detected-info", "wiz-db-manual", "wiz-db-ask-has-dump", "wiz-db-pick-dump", "wiz-db-connecting", "wiz-db-tables"].forEach(
    (id) => $(id).classList.add("hidden")
  );
  $("wiz-db-connect-error").textContent = "";
}

function renderWizardFolderList() {
  const ul = $("wiz-folder-list");
  ul.innerHTML = "";
  for (const [i, f] of wizardFolders.entries()) {
    const li = document.createElement("li");
    li.innerHTML = `<span class="mono">${escapeHtml(f.path)}</span>`;
    const removeBtn = document.createElement("button");
    removeBtn.className = "ghost-btn remove-folder-btn";
    removeBtn.type = "button";
    removeBtn.textContent = "Remove";
    removeBtn.addEventListener("click", () => {
      wizardFolders.splice(i, 1);
      renderWizardFolderList();
    });
    li.appendChild(removeBtn);
    ul.appendChild(li);
  }
}

$("wiz-add-folders-btn").addEventListener("click", async () => {
  const dirs = await open({ directory: true, multiple: true });
  if (!dirs) return;
  const picked = Array.isArray(dirs) ? dirs : [dirs];
  for (const p of picked) {
    if (!wizardFolders.some((f) => f.path === p)) {
      wizardFolders.push({ path: p, needsDb: null, details: null, sourceLabel: null, dump: null });
    }
  }
  renderWizardFolderList();
});

$("wiz-folders-next-btn").addEventListener("click", () => {
  if (wizardFolders.length === 0) {
    $("wiz-folders-error").textContent = "Select at least one project folder.";
    return;
  }
  $("wiz-folders-error").textContent = "";
  showWizardStep("wiz-step-needs-db");
});

$("wiz-needs-db-back-btn").addEventListener("click", () => showWizardStep("wiz-step-folders"));

$("wiz-needs-db-no-btn").addEventListener("click", () => {
  for (const f of wizardFolders) f.needsDb = false;
  renderWizardReadyStep();
});

$("wiz-needs-db-yes-btn").addEventListener("click", () => {
  for (const f of wizardFolders) f.needsDb = true;
  wizardFolderIndex = 0;
  advanceDbWizard();
});

// ---------- per-folder database detection / connection / export ----------

async function advanceDbWizard() {
  if (wizardFolderIndex >= wizardFolders.length) {
    renderWizardReadyStep();
    return;
  }
  const folder = wizardFolders[wizardFolderIndex];
  showWizardStep("wiz-step-db-folder");
  hideAllDbSubPanels();
  $("wiz-db-folder-title").textContent = wizFolderLabel(folder.path);
  $("wiz-db-folder-progress").textContent = `Folder ${wizardFolderIndex + 1} of ${wizardFolders.length}`;
  wizardDbSubState = "detecting";
  $("wiz-db-detecting").classList.remove("hidden");

  let detected = null;
  try {
    detected = await invoke("detect_db_connection", { folderPath: folder.path });
  } catch (err) {
    // Detection is pure local file parsing - a thrown error here means
    // something unexpected (e.g. an unreadable path), not "no config
    // found". Either way, manual entry is always the safe fallback.
    detected = null;
  }

  hideAllDbSubPanels();
  if (detected) {
    folder.details = detected.details;
    folder.sourceLabel = detected.source_file;
    showDetectedInfo(detected);
    showAskHasDump();
  } else {
    showManualEntry(folder);
  }
}

function showDetectedInfo(detected) {
  wizardDbSubState = "detected";
  $("wiz-db-detected-source").textContent = detected.source_file;
  $("wiz-db-detected-engine").textContent = detected.details.engine;
  $("wiz-db-detected-host").textContent = detected.details.host;
  $("wiz-db-detected-port").textContent = String(detected.details.port);
  $("wiz-db-detected-database").textContent = detected.details.database;
  $("wiz-db-detected-username").textContent = detected.details.username;
  $("wiz-db-detected-info").classList.remove("hidden");
}

function showAskHasDump() {
  wizardDbSubState = "ask";
  $("wiz-db-ask-has-dump").classList.remove("hidden");
}

// Round 18: real per-engine defaults, not a hardcoded mysql/3306 - a
// developer picking Postgres or MongoDB from the dropdown shouldn't have
// to remember (or worse, leave wrong) another engine's standard port.
const DEFAULT_PORT_BY_ENGINE = { mysql: 3306, postgres: 5432, mongodb: 27017 };

function dbNounFor(engine) {
  return engine === "mongodb" ? "collections" : "tables";
}

$("wiz-manual-engine").addEventListener("change", () => {
  const engine = $("wiz-manual-engine").value;
  const portField = $("wiz-manual-port");
  // Only overwrite if it's still at some *other* engine's default - never
  // clobber a port the developer already typed on purpose.
  if (Object.values(DEFAULT_PORT_BY_ENGINE).includes(parseInt(portField.value, 10))) {
    portField.value = DEFAULT_PORT_BY_ENGINE[engine];
  }
});

function showManualEntry(folder) {
  wizardDbSubState = "manual";
  $("wiz-manual-engine").value = folder.details?.engine || "mysql";
  $("wiz-manual-host").value = folder.details?.host || "";
  $("wiz-manual-port").value = folder.details?.port || DEFAULT_PORT_BY_ENGINE[$("wiz-manual-engine").value];
  $("wiz-manual-database").value = folder.details?.database || "";
  $("wiz-manual-username").value = folder.details?.username || "";
  $("wiz-manual-password").value = folder.details?.password || "";
  $("wiz-manual-status").textContent = "";
  $("wiz-manual-error").textContent = "";
  $("wiz-db-manual").classList.remove("hidden");
}

$("wiz-ask-edit-btn").addEventListener("click", () => {
  hideAllDbSubPanels();
  showManualEntry(wizardFolders[wizardFolderIndex]);
});

$("wiz-manual-continue-btn").addEventListener("click", async () => {
  const folder = wizardFolders[wizardFolderIndex];
  const details = {
    engine: $("wiz-manual-engine").value,
    host: $("wiz-manual-host").value.trim(),
    port: parseInt($("wiz-manual-port").value, 10) || DEFAULT_PORT_BY_ENGINE[$("wiz-manual-engine").value],
    database: $("wiz-manual-database").value.trim(),
    username: $("wiz-manual-username").value.trim(),
    password: $("wiz-manual-password").value,
  };
  if (!details.host || !details.database || !details.username) {
    $("wiz-manual-error").textContent = "Host, database, and username are required.";
    return;
  }
  $("wiz-manual-error").textContent = "";
  $("wiz-manual-status").textContent = "Testing connection…";
  $("wiz-manual-continue-btn").disabled = true;
  try {
    await invoke("test_db_connection", { details });
    folder.details = details;
    folder.sourceLabel = "Manually entered";
    $("wiz-manual-status").textContent = "";
    hideAllDbSubPanels();
    showAskHasDump();
  } catch (err) {
    $("wiz-manual-status").textContent = "";
    $("wiz-manual-error").textContent = String(err);
  } finally {
    $("wiz-manual-continue-btn").disabled = false;
  }
});

$("wiz-has-dump-yes-btn").addEventListener("click", () => {
  const folder = wizardFolders[wizardFolderIndex];
  hideAllDbSubPanels();
  wizardDbSubState = "pick-dump";
  $("wiz-dump-schema").value = folder.details?.database || "";
  $("wiz-dump-file-path").value = "";
  $("wiz-dump-error").textContent = "";
  $("wiz-db-pick-dump").classList.remove("hidden");
});

$("wiz-dump-browse-btn").addEventListener("click", async () => {
  const file = await open({ directory: false, multiple: false });
  if (file) $("wiz-dump-file-path").value = file;
});

$("wiz-dump-confirm-btn").addEventListener("click", () => {
  const schema = $("wiz-dump-schema").value.trim();
  const filePath = $("wiz-dump-file-path").value.trim();
  if (!schema || !filePath) {
    $("wiz-dump-error").textContent = "A schema name and a dump file are both required.";
    return;
  }
  wizardFolders[wizardFolderIndex].dump = { schema, filePath, engine: wizardFolders[wizardFolderIndex].details.engine };
  wizardFolderIndex += 1;
  advanceDbWizard();
});

$("wiz-has-dump-no-btn").addEventListener("click", async () => {
  const folder = wizardFolders[wizardFolderIndex];
  hideAllDbSubPanels();
  wizardDbSubState = "connecting";
  $("wiz-db-connecting").classList.remove("hidden");
  try {
    await invoke("test_db_connection", { details: folder.details });
    const tables = await invoke("list_db_tables", { details: folder.details });
    hideAllDbSubPanels();
    wizardDbSubState = "tables";
    const noun = dbNounFor(folder.details.engine);
    $("wiz-tables-database").textContent = folder.details.database;
    $("wiz-tables-noun").textContent = noun;
    $("wiz-tables-noun-2").textContent = noun;
    const ul = $("wiz-tables-list");
    ul.innerHTML = "";
    for (const t of tables) {
      const li = document.createElement("li");
      const label = document.createElement("label");
      const cb = document.createElement("input");
      cb.type = "checkbox";
      cb.checked = true;
      cb.dataset.table = t.name;
      const rowCount = t.approx_row_count == null ? "" : ` (~${t.approx_row_count} rows)`;
      label.appendChild(cb);
      label.appendChild(document.createTextNode(`${t.name}${rowCount}`));
      li.appendChild(label);
      ul.appendChild(li);
    }
    $("wiz-export-status").textContent = "";
    $("wiz-tables-error").textContent = "";
    $("wiz-db-tables").classList.remove("hidden");
  } catch (err) {
    hideAllDbSubPanels();
    wizardDbSubState = "ask";
    $("wiz-db-connect-error").textContent = String(err);
    showAskHasDump();
  }
});

$("wiz-export-btn").addEventListener("click", async () => {
  const folder = wizardFolders[wizardFolderIndex];
  const checked = Array.from($("wiz-tables-list").querySelectorAll("input[type=checkbox]:checked")).map(
    (cb) => cb.dataset.table
  );
  if (checked.length === 0) {
    $("wiz-tables-error").textContent = `Select at least one ${dbNounFor(folder.details.engine).replace(/s$/, "")}.`;
    return;
  }
  $("wiz-tables-error").textContent = "";
  $("wiz-export-status").textContent = `Exporting full ${dbNounFor(folder.details.engine)} content…`;
  $("wiz-export-btn").disabled = true;
  try {
    const result = await invoke("export_db_tables", { details: folder.details, tables: checked });
    folder.dump = { schema: folder.details.database, filePath: result.file_path, engine: folder.details.engine };
    $("wiz-export-status").textContent = `Exported ${formatBytes(result.size_bytes)}.`;
    wizardFolderIndex += 1;
    advanceDbWizard();
  } catch (err) {
    $("wiz-export-status").textContent = "";
    $("wiz-tables-error").textContent = String(err);
  } finally {
    $("wiz-export-btn").disabled = false;
  }
});

$("wiz-db-back-btn").addEventListener("click", () => {
  if (wizardFolderIndex === 0) {
    showWizardStep("wiz-step-needs-db");
    return;
  }
  wizardFolderIndex -= 1;
  advanceDbWizard();
});

// ---------- final step: summary + real Send ----------

function renderWizardReadyStep() {
  const ul = $("wiz-ready-summary");
  ul.innerHTML = "";
  for (const f of wizardFolders) {
    const li = document.createElement("li");
    let status;
    if (!f.needsDb) status = "no database";
    else if (f.dump) status = `database: ${escapeHtml(f.dump.schema)} (${escapeHtml(f.dump.filePath)})`;
    else status = "database: none selected";
    li.innerHTML = `<span class="mono">${escapeHtml(wizFolderLabel(f.path))}</span> — ${status}`;
    ul.appendChild(li);
  }
  showWizardStep("wiz-step-ready");
}

$("wiz-ready-back-btn").addEventListener("click", () => {
  const anyDb = wizardFolders.some((f) => f.needsDb);
  if (anyDb) {
    wizardFolderIndex = wizardFolders.length - 1;
    advanceDbWizard();
  } else {
    showWizardStep("wiz-step-needs-db");
  }
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

// Resets the wizard back to step 1, for the next send after this one
// finishes (or after a failure the developer wants to redo from scratch).
function resetSendWizard() {
  wizardFolders = [];
  wizardFolderIndex = 0;
  renderWizardFolderList();
  $("wiz-folders-error").textContent = "";
  showWizardStep("wiz-step-folders");
}

$("send-btn").addEventListener("click", async () => {
  $("send-error").textContent = "";
  $("send-result").textContent = "";
  $("send-code-wrap").classList.add("hidden");
  stopCodeExpiryCountdown();

  if (wizardFolders.length === 0) {
    $("send-error").textContent = "Select at least one project folder.";
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

    const folders = wizardFolders.map((f) => ({
      path: f.path,
      dump: f.needsDb && f.dump ? { schema: f.dump.schema, filePath: f.dump.filePath } : null,
    }));
    const snapshotId = await invoke("share_snapshot_wizard", {
      folders,
      roomCode: roomId,
      signalingUrl,
    });
    $("send-progress-label").textContent = "Sent.";
    $("send-result").textContent = `Sent as ${snapshotId}`;
    refreshReceivers(); // this send may have just added a new roster entry
    resetSendWizard();
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
