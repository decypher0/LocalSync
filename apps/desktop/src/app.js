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
const { writeText: writeClipboardText } = window.__TAURI__.clipboardManager;

const $ = (id) => document.getElementById(id);

// ---------- preload (dev shortcut for LOCALSYNC_PRELOAD_SNAPSHOT) ----------
// Fires the same IncomingSnapshotInfo shape receive_snapshot resolves with,
// so this reuses startReceiveSessionFromInfo verbatim — the review screen a
// developer sees this way is the same code path a real P2P receive draws,
// not a separate mockup. See scripts/demo-review-screen.sh.
listen("preload-review", (evt) => startReceiveSessionFromInfo(evt.payload));

// ---------- firewall warning (Linux ufw active at startup) ----------
listen("firewall-warning", (evt) => {
  $("firewall-banner").textContent = evt.payload;
  $("firewall-banner").classList.remove("hidden");
});

// ---------- round 24: magic-link deep-link handoff (localsync://receive?code=...) ----------
// Only ever pre-fills the Receive tab's own code input and switches to it -
// reuses the exact existing code-entry path rather than a parallel one, and
// never itself calls receive_snapshot. Accepting a P2P connection always
// still needs the same explicit Receive click a person typing the code by
// hand would make.
function handleDeepLinkUrls(urls) {
  if (!urls) return;
  for (const raw of urls) {
    let url;
    try {
      url = new URL(raw);
    } catch {
      continue; // not a parseable URL at all - ignore rather than throw
    }
    const code = url.searchParams.get("code");
    if (!code) continue;
    switchToTab("receive");
    $("receive-room-code").value = code;
    $("receive-start-error").textContent = "";
    break; // only one link is ever meaningful per launch/event
  }
}

// Two separate entry points, matching how the plugin itself splits this:
// getCurrent() covers "this process was just launched by clicking a link"
// (a fresh Windows/Linux process's own CLI argument, or macOS's equivalent -
// both already parsed into plugin state by the time this JS runs); onOpenUrl
// covers "a link was clicked again while this process is already running"
// (macOS's native re-open event, or a second Windows/Linux process
// redirected here by tauri-plugin-single-instance's "deep-link" feature -
// see main.rs for why that plugin exists at all).
window.__TAURI__.deepLink
  .getCurrent()
  .then((urls) => handleDeepLinkUrls(urls))
  .catch((err) => console.error("deep-link getCurrent failed:", err));
window.__TAURI__.deepLink.onOpenUrl((urls) => handleDeepLinkUrls(urls));

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

// (Removed in the session-model refactor: the sender-side "pull request"
// banner. A pull request was a receiver asking, over a connection the sender
// kept open, "anything new?" - senders no longer keep connections open, so
// updates are pushed by the sender from the session instead. See
// pushUpdateToDevices.)

// ---------- round 23: cloud-access requests (sender-side: a receiver asked
// to be granted Drive access) - same rendering/dedup pattern as the
// pull-request banner above, reusing the same container since both are
// "an incoming request from a connected peer, shown until acted on". ----------
listen("cloud-access-request", (evt) => {
  const { peer_id: peerId, google_email: googleEmail } = evt.payload;
  const existing = [...$("pull-requests").children].find((el) => el.dataset.peer === peerId);
  if (existing) return;
  const div = document.createElement("div");
  div.dataset.peer = peerId;
  div.className = "peer-banner";
  div.innerHTML = `
    <span>Cloud drop access request from <strong>${escapeHtml(googleEmail)}</strong></span>
    <span class="inline-row">
      <button class="ghost-btn accept-btn" type="button">Grant access</button>
      <button class="ghost-btn decline-btn" type="button">Decline</button>
    </span>
    <p class="error"></p>
  `;
  const errorEl = div.querySelector(".error");
  const respond = async (accept) => {
    div.querySelectorAll("button").forEach((b) => (b.disabled = true));
    try {
      await invoke("respond_to_cloud_access_request", { peerId, accept });
      div.remove();
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
// applyReviewInfoToSession verbatim - a pushed update goes through the
// exact same diff-review-then-Run/Reject screen as a normal receive.
// Round 29: attaches to whichever receive session most recently connected
// to a sender (lastReceiveSessionId) rather than a single global "the
// receive" - matches the backend's own single-outgoing-connection model
// (see lastReceiveSessionId's own doc comment above) rather than pretending
// this app can already receive pushed updates from more than one sender at
// once. Falls back to a fresh tab if that session is gone (e.g. closed, or
// this is a restart) so the update is never silently dropped.
listen("snapshot-updated", (evt) => {
  const session = lastReceiveSessionId && sessions.get(lastReceiveSessionId);
  if (!session) {
    startReceiveSessionFromInfo(evt.payload);
    return;
  }
  applyReviewInfoToSession(session, evt.payload);
  renderSessionTabs();
  if (session.id === activeSessionId) renderActiveSession();
});

// ---------- settings ----------
$("settings-toggle").addEventListener("click", () => {
  $("settings-panel").classList.toggle("hidden");
});
$("data-dir-display").value = "(read at launch; not editable here)";

// ---------- theme: system default, with a persisted manual override ----------
// Three real states, not a boolean: "system" (default) tracks
// prefers-color-scheme live, for as long as the developer never overrides
// it; "light"/"dark" are explicit, persisted choices that win regardless
// of the OS setting. The actual color values for each theme live entirely
// in styles.css's :root/[data-theme] blocks - this only ever decides which
// one applies, never touches a color itself.
const THEME_KEY = "localsync.theme";

function applyTheme(choice) {
  if (choice === "light" || choice === "dark") {
    document.documentElement.dataset.theme = choice;
  } else {
    delete document.documentElement.dataset.theme;
  }
}

function currentThemeChoice() {
  const saved = localStorage.getItem(THEME_KEY);
  return saved === "light" || saved === "dark" ? saved : "system";
}

// index.html's own inline bootstrap script already applied a saved
// light/dark override before first paint (avoiding a flash of the wrong
// theme) - this just brings the radios themselves in sync with it, and
// re-applies for the "system" case too (a no-op today since the
// bootstrap script only ever sets light/dark, but keeps this function the
// single source of truth rather than splitting theme-application logic
// across two files).
const savedTheme = currentThemeChoice();
$(`theme-${savedTheme}`).checked = true;
applyTheme(savedTheme);

// Shared by Settings' own radios and the round 29 app menu's Theme submenu -
// one source of truth for "choosing a theme", not two divergent code paths
// that happen to agree today.
function setThemeChoice(choice) {
  $(`theme-${choice}`).checked = true;
  localStorage.setItem(THEME_KEY, choice);
  applyTheme(choice);
}

["system", "light", "dark"].forEach((choice) => {
  $(`theme-${choice}`).addEventListener("change", () => setThemeChoice(choice));
});

// ---------- round 29 goal B2: real application-level menu ----------
// One handler for every item build_menu (main.rs) creates - Quit is
// handled directly in Rust (see main.rs's own comment) and never reaches
// here. Every other item just reuses whatever existing, already-correct UI
// already does the same thing (Settings' own theme radios, the Settings
// panel, the Check-for-updates button) rather than duplicating that logic.
listen("menu-action", (evt) => {
  switch (evt.payload) {
    case "menu-check-updates":
      $("settings-panel").classList.remove("hidden");
      $("check-updates-status").textContent = "Checking…";
      runUpdateCheck(true);
      break;
    case "menu-settings":
      $("settings-panel").classList.remove("hidden");
      break;
    case "menu-theme-system":
    case "menu-theme-light":
    case "menu-theme-dark":
      setThemeChoice(evt.payload.slice("menu-theme-".length));
      break;
    case "menu-session-history":
      openSessionHistory();
      break;
  }
});

// ---------- saved sessions (the old "session history" panel) ----------
// Nothing is listed here unless the user chose "Save this session" - see
// promptSaveSession. A saved session can be opened again (its project,
// database plan and per-device history come back); entries from before
// saving was opt-in have no project to reopen and are listed read-only.
async function openSessionHistory() {
  $("settings-panel").classList.add("hidden");
  $("session-history-panel").classList.remove("hidden");
  $("session-history-error").textContent = "";
  try {
    renderSessionHistoryList(await invoke("load_session_history"));
  } catch (err) {
    $("session-history-error").textContent = String(err);
  }
}

function renderSessionHistoryList(entries) {
  const list = $("session-history-list");
  list.innerHTML = "";
  // Newest first - a long-lived install's oldest entries are the least
  // relevant to see first.
  const sorted = [...entries].sort((a, b) => new Date(b.started_at) - new Date(a.started_at));
  $("session-history-empty").classList.toggle("hidden", sorted.length > 0);
  for (const entry of sorted) {
    const li = document.createElement("li");
    const saved = entry.project;
    const detail = saved
      ? `${saved.devices.length} device${saved.devices.length === 1 ? "" : "s"} � saved project`
      : "older activity (details weren't saved)";
    li.innerHTML = `
      <span class="inline-row receivers-header">
        <span><svg class="icon"><use href="#${entry.kind === "send" ? "icon-send" : "icon-download"}"></use></svg>
          ${escapeHtml(entry.title)} <span class="hint-inline">${escapeHtml(detail)}</span></span>
        ${saved ? `<span class="inline-row">
          <button class="ghost-btn" type="button" data-open-saved="${escapeHtml(entry.id)}">Open</button>
          <button class="ghost-btn" type="button" data-delete-saved="${escapeHtml(entry.id)}">Delete</button>
        </span>` : ""}
      </span>
    `;
    list.appendChild(li);
  }
}

$("session-history-list").addEventListener("click", async (e) => {
  const open = e.target.closest("[data-open-saved]");
  const del = e.target.closest("[data-delete-saved]");
  if (!open && !del) return;
  $("session-history-error").textContent = "";
  try {
    if (open) {
      await openSavedSession(open.dataset.openSaved);
      $("session-history-panel").classList.add("hidden");
    } else {
      await invoke("delete_saved_project_session", { sessionId: del.dataset.deleteSaved });
      const open = sessions.get(del.dataset.deleteSaved);
      if (open) open.saved = false;
      renderSessionHistoryList(await invoke("load_session_history"));
    }
  } catch (err) {
    $("session-history-error").textContent = String(err);
  }
});

$("session-history-close-btn").addEventListener("click", () => {
  $("session-history-panel").classList.add("hidden");
});

// ---------- round 23: linked Google account (Cloud drop) ----------
async function refreshGoogleAccountStatus() {
  try {
    const linked = await invoke("google_account_status");
    $("google-account-status").textContent = linked ? `Linked as ${linked.email}` : "Not linked.";
    $("unlink-google-btn").classList.toggle("hidden", !linked);
  } catch (err) {
    $("google-account-status").textContent = String(err);
  }
}
refreshGoogleAccountStatus();

$("link-google-btn").addEventListener("click", async () => {
  $("google-account-error").textContent = "";
  $("link-google-btn").disabled = true;
  $("google-account-status").textContent = "Opening your browser for Google sign-in…";
  try {
    // Blocks until the browser redirect completes (or times out) - see
    // commands::link_google_account. The system browser opens itself; there's
    // nothing further to do here until this resolves.
    await invoke("link_google_account");
    await refreshGoogleAccountStatus();
  } catch (err) {
    $("google-account-error").textContent = String(err);
    await refreshGoogleAccountStatus();
  } finally {
    $("link-google-btn").disabled = false;
  }
});

$("unlink-google-btn").addEventListener("click", async () => {
  $("google-account-error").textContent = "";
  try {
    await invoke("unlink_google_account");
    await refreshGoogleAccountStatus();
  } catch (err) {
    $("google-account-error").textContent = String(err);
  }
});

// ---------- relay mode (persisted in localStorage — set once, survives restarts) ----------
const MODE_KEY = "localsync.relayMode";
const RELAY_URL_KEY = "localsync.relayUrl";

// The one place the persisted send mode ("local" | "remote") and relay URL
// are read and written. Settings' radios, the wizard's radios, a push
// update's fallback code, and Receive all go through this instead of each
// touching localStorage themselves. Cloud drop is never stored here - it's a
// per-send choice, not a default (see wizRelayMode).
const modeStore = {
  get mode() {
    return localStorage.getItem(MODE_KEY) === "remote" ? "remote" : "local";
  },
  set mode(value) {
    localStorage.setItem(MODE_KEY, value === "remote" ? "remote" : "local");
  },
  get url() {
    return localStorage.getItem(RELAY_URL_KEY) || "";
  },
  set url(value) {
    localStorage.setItem(RELAY_URL_KEY, value);
  },
};

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
  modeStore.mode = relayMode();
});
$("mode-remote").addEventListener("change", () => {
  updateModeUi();
  modeStore.mode = relayMode();
});
$("relay-url").addEventListener("input", () => {
  modeStore.url = relayUrl();
});

// Restore persisted mode/URL on load.
if (modeStore.mode === "remote") $("mode-remote").checked = true;
$("relay-url").value = modeStore.url;
updateModeUi();

// ---------- round 37: device name (one identity, used both when this
// device announces itself as discoverable and when it's the one initiating
// a connection to a discovered device) ----------
const DEVICE_NAME_KEY = "localsync.deviceName";

function deviceName() {
  return $("device-name").value.trim();
}
// Saved name if someone already chose one, else the OS hostname - so the
// field is never empty and discoverability works with zero setup.
async function fillDefaultDeviceName() {
  try {
    $("device-name").value = await invoke("default_device_name");
  } catch (err) {
    console.error("default_device_name failed:", err);
  }
}
$("device-name").value = localStorage.getItem(DEVICE_NAME_KEY) || "";
if (!deviceName()) fillDefaultDeviceName();
$("device-name").addEventListener("input", () => {
  localStorage.setItem(DEVICE_NAME_KEY, deviceName());
});
// "change" fires on blur/Enter, not every keystroke - so an edit made while
// discoverable is already on re-announces once, with the final name.
$("device-name").addEventListener("change", async () => {
  if (!deviceName()) {
    localStorage.removeItem(DEVICE_NAME_KEY);
    await fillDefaultDeviceName();
  }
  if ($("discoverable-toggle").checked) $("discoverable-toggle").dispatchEvent(new Event("change"));
});

// ---------- round 37 goal 1: opt-in discoverability (receiving side) ----------
$("discoverable-toggle").addEventListener("change", async () => {
  const enabled = $("discoverable-toggle").checked;
  $("discoverable-error").textContent = "";
  $("discoverable-status").textContent = enabled ? "Turning on…" : "";
  $("discoverable-toggle").disabled = true;
  try {
    await invoke("set_discoverable", { enabled, nickname: deviceName() });
    $("discoverable-status").textContent = enabled ? `Discoverable as "${deviceName()}".` : "";
  } catch (err) {
    $("discoverable-error").textContent = String(err);
    $("discoverable-status").textContent = "";
    $("discoverable-toggle").checked = false; // the backend call failed - never show "on" for a toggle that isn't
  } finally {
    $("discoverable-toggle").disabled = false;
  }
});

// ---------- round 37 goal 3: incoming connection request (receiving side) ----------
// Same rendering/dedup pattern as the pull-request/cloud-access-request
// banners above (reusing #pull-requests, not a new container) - "an
// incoming request from a peer, shown until acted on" is exactly what this
// is too. This is the actual consent gate discovery adds: nothing is ever
// received without this Accept explicitly happening first.
listen("connection-request", (evt) => {
  const peerId = evt.payload.peer_id;
  const existing = [...$("pull-requests").children].find((el) => el.dataset.peer === peerId);
  if (existing) return;
  const div = document.createElement("div");
  div.dataset.peer = peerId;
  div.className = "peer-banner";
  div.innerHTML = `
    <span><strong>${escapeHtml(evt.payload.sender_name)}</strong> wants to send you a project (found via Local network discovery)</span>
    <span class="inline-row">
      <button class="ghost-btn accept-btn" type="button"><svg class="icon"><use href="#icon-check"></use></svg> Accept</button>
      <button class="ghost-btn decline-btn" type="button"><svg class="icon"><use href="#icon-x"></use></svg> Decline</button>
    </span>
    <p class="error"></p>
  `;
  const errorEl = div.querySelector(".error");
  // ponytail: on Accept, this awaits the *entire* transfer before resolving
  // (respond_to_connection_request receives the whole payload before
  // returning - see its own doc comment) - so unlike a normal receive,
  // there's no session tab/progress bar until it's already done; the
  // banner's buttons just stay disabled for the duration. Acceptable for a
  // discovery-initiated receive (this round's actual scope is finding and
  // selecting a peer, not this path's progress UI specifically); upgrade
  // path if a large discovery-initiated project makes this feel stuck: split
  // accept-and-start-transfer from completion, same as receive_snapshot's
  // own two-phase (progress event, then resolve) shape.
  const respond = async (accept) => {
    div.querySelectorAll("button").forEach((b) => (b.disabled = true));
    try {
      const info = await invoke("respond_to_connection_request", { peerId, accept });
      div.remove();
      if (info) startReceiveSessionFromInfo(info);
    } catch (err) {
      errorEl.textContent = String(err);
      div.querySelectorAll("button").forEach((b) => (b.disabled = false));
    }
  };
  div.querySelector(".accept-btn").addEventListener("click", () => respond(true));
  div.querySelector(".decline-btn").addEventListener("click", () => respond(false));
  $("pull-requests").appendChild(div);
});

// ---------- round 37 goal 2: live nearby-devices list (sending side) ----------
//
// Browsing only runs while it's actually useful: while the Send wizard's
// mode step is showing *and* Local network is the selected mode - started/
// stopped from showWizardStep/the mode radios' change handlers below, and
// always stopped on wizard close/cancel, so an idle wizard never leaves a
// browse daemon running. `wizardSelectedDevice` is the one piece of state
// that changes what the final Send button actually does - see send-btn's
// own handler further down.
let wizardSelectedDevice = null;
let nearbyDevicesPollInterval = null;

async function startNearbyDevicesBrowsing() {
  $("wiz-nearby-wrap").classList.remove("hidden");
  $("wiz-nearby-empty").textContent = "Looking for discoverable devices on this network…";
  $("wiz-nearby-empty").classList.remove("hidden");
  $("wiz-nearby-list").classList.add("hidden");
  try {
    await invoke("start_discovery_browsing");
  } catch (err) {
    $("wiz-nearby-empty").textContent = `Couldn't start device discovery: ${err}`;
    return;
  }
  clearInterval(nearbyDevicesPollInterval);
  refreshNearbyDevices();
  nearbyDevicesPollInterval = setInterval(refreshNearbyDevices, 2000);
}

function stopNearbyDevicesBrowsing() {
  clearInterval(nearbyDevicesPollInterval);
  nearbyDevicesPollInterval = null;
  $("wiz-nearby-wrap").classList.add("hidden");
  invoke("stop_discovery_browsing").catch((err) => console.error("stop_discovery_browsing failed:", err));
}

async function refreshNearbyDevices() {
  let devices;
  try {
    devices = await invoke("list_nearby_devices");
  } catch (err) {
    $("wiz-nearby-empty").textContent = `Couldn't list nearby devices: ${err}`;
    $("wiz-nearby-empty").classList.remove("hidden");
    $("wiz-nearby-list").classList.add("hidden");
    return;
  }
  // A device that was selected but has since gone away (e.g. its
  // discoverability was turned off) needs its own, more specific message -
  // silently falling back to manual code entry without saying why would
  // look like the click simply didn't work.
  if (wizardSelectedDevice && !devices.some((d) => d.fullname === wizardSelectedDevice.fullname)) {
    deselectNearbyDevice();
    $("wiz-nearby-empty").textContent = "That device is no longer discoverable — pick another, or enter a code manually.";
  }
  renderNearbyDevicesList(devices);
}

function renderNearbyDevicesList(devices) {
  const empty = $("wiz-nearby-empty");
  const list = $("wiz-nearby-list");
  if (devices.length === 0) {
    // Goal 3: a clear, non-alarming empty state - looking for devices is
    // the normal first few seconds of every browse, and "found nothing" is
    // an expected, ordinary outcome on networks that block multicast, not
    // a failure this UI should imply.
    if (!empty.textContent.startsWith("That device")) {
      empty.textContent = "No devices found yet — you can still enter a code manually on the final step.";
    }
    empty.classList.remove("hidden");
    list.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  list.classList.remove("hidden");
  list.innerHTML = "";
  for (const d of devices) {
    const li = document.createElement("li");
    const isSelected = wizardSelectedDevice?.fullname === d.fullname;
    li.innerHTML = `
      <span class="inline-row receivers-header">
        <span><svg class="icon"><use href="#icon-wifi"></use></svg> ${escapeHtml(d.nickname)}</span>
        <button class="${isSelected ? "primary-btn" : "ghost-btn"} select-nearby-btn" type="button" data-fullname="${escapeHtml(d.fullname)}">
          ${isSelected ? "Selected" : "Select"}
        </button>
      </span>
    `;
    list.appendChild(li);
  }
}

$("wiz-nearby-list").addEventListener("click", async (e) => {
  const btn = e.target.closest(".select-nearby-btn");
  if (!btn) return;
  let devices;
  try {
    devices = await invoke("list_nearby_devices");
  } catch {
    return;
  }
  const device = devices.find((d) => d.fullname === btn.dataset.fullname);
  if (device) selectNearbyDevice(device);
});

function selectNearbyDevice(device) {
  wizardSelectedDevice = device;
  $("wiz-nearby-selected").classList.remove("hidden");
  $("wiz-nearby-selected-name").textContent = device.nickname;
  refreshNearbyDevices();
}

function deselectNearbyDevice() {
  wizardSelectedDevice = null;
  $("wiz-nearby-selected").classList.add("hidden");
  refreshNearbyDevices();
}
$("wiz-nearby-deselect-btn").addEventListener("click", deselectNearbyDevice);

// Round 20 goal 3: the Send wizard's own transfer-mode step - a separate
// radio group from Settings' (Receive still reads Settings' via
// relayMode()/relayUrl() above; that flow isn't part of this round's
// scope), but backed by the exact same localStorage keys, so choosing a
// mode here updates the one real shared default rather than creating a
// second, divergent setting - Settings and the wizard just become two
// surfaces onto the same underlying choice.
// "cloud" is a third, mutually exclusive choice here (moved onto this step
// from its old Step 4 checkbox - see wiz-cloud-drop-wrap in index.html) but
// is deliberately NOT one of the two values `MODE_KEY` ever persists: that
// key is also read by Settings/Receive's own relayMode(), which only ever
// expects "local"/"remote" back. Cloud drop still needs a real signaling
// mode under the hood for its brief identity-request/response handshake
// (see commands::start_cloud_drop_session's own `mode` parameter) - this
// always hands it "local", the same zero-configuration default Local
// network itself is. Picking Cloud drop only *hides* Local/Remote's own
// selection state for the rest of this wizard visit, it never overwrites it.
function wizRelayMode() {
  if ($("wiz-mode-cloud").checked) return "cloud";
  return $("wiz-mode-remote").checked ? "remote" : "local";
}
function wizRelayUrl() {
  return $("wiz-relay-url").value.trim();
}
function updateWizModeUi() {
  const mode = wizRelayMode();
  $("wiz-relay-url-wrap").classList.toggle("hidden", mode !== "remote");
  $("wiz-cloud-drop-wrap").classList.toggle("hidden", mode !== "cloud");
  if (mode === "cloud") refreshWizGoogleAccountStatus();

  // Round 37 goal 2: scoped to Local network only, per this round's own
  // goal - Remote relay/Cloud drop already work across networks LAN
  // discovery can't reach. A device selected while on Local network stops
  // meaning anything once the mode changes away from it.
  if (mode === "local") {
    startNearbyDevicesBrowsing();
  } else {
    stopNearbyDevicesBrowsing();
    deselectNearbyDevice();
  }
}
// Called every time step 1 is (re)entered, so it always reflects the most
// recent choice - made here, or made in Settings since the wizard was last
// opened. Cloud drop is never restored from storage (see wizRelayMode's own
// comment) - it always starts unchecked, falling back to whichever of
// Local/Remote was last persisted.
function syncWizModeFromStorage() {
  $("wiz-mode-cloud").checked = false;
  $("wiz-mode-remote").checked = modeStore.mode === "remote";
  $("wiz-mode-local").checked = !$("wiz-mode-remote").checked;
  $("wiz-relay-url").value = modeStore.url;
  updateWizModeUi();
}
$("wiz-mode-local").addEventListener("change", () => {
  updateWizModeUi();
  modeStore.mode = wizRelayMode();
});
$("wiz-mode-remote").addEventListener("change", () => {
  updateWizModeUi();
  modeStore.mode = wizRelayMode();
});
$("wiz-mode-cloud").addEventListener("change", () => {
  // Deliberately not persisted to MODE_KEY - see wizRelayMode's comment.
  updateWizModeUi();
});
$("wiz-relay-url").addEventListener("input", () => {
  modeStore.url = wizRelayUrl();
});

// ---------- Cloud drop's Step-1 linked-Google-account check ----------
// Adapts the exact same Settings-panel flow (refreshGoogleAccountStatus/
// link-google-btn above) to Step 1's own DOM ids, so someone who picks
// Cloud drop without having linked an account yet can do so right there
// instead of being sent off to Settings and back.
async function refreshWizGoogleAccountStatus() {
  $("wiz-google-account-error").textContent = "";
  try {
    const linked = await invoke("google_account_status");
    $("wiz-google-account-status").textContent = linked ? `Linked as ${linked.email}` : "Not linked yet.";
    $("wiz-link-google-btn").classList.toggle("hidden", !!linked);
  } catch (err) {
    $("wiz-google-account-status").textContent = String(err);
  }
}
$("wiz-link-google-btn").addEventListener("click", async () => {
  $("wiz-google-account-error").textContent = "";
  $("wiz-link-google-btn").disabled = true;
  $("wiz-google-account-status").textContent = "Opening your browser for Google sign-in…";
  try {
    await invoke("link_google_account");
    await refreshWizGoogleAccountStatus();
    await refreshGoogleAccountStatus(); // keep Settings' own status in sync too
  } catch (err) {
    $("wiz-google-account-error").textContent = String(err);
    await refreshWizGoogleAccountStatus();
  } finally {
    $("wiz-link-google-btn").disabled = false;
  }
});

$("wiz-mode-next-btn").addEventListener("click", () => {
  $("wiz-mode-error").textContent = "";
  if (wizardTargetSession) {
    // Existing session: just pick where it goes and send.
    const session = sessions.get(wizardTargetSession);
    if (!session) {
      $("wiz-mode-error").textContent = "That session is no longer open.";
      return;
    }
    const spec = readWizardTarget($("wiz-mode-error"));
    if (!spec) return;
    closeSendWizard();
    resetSendWizard();
    startTransfer(session, spec);
    return;
  }
  if (wizRelayMode() === "remote" && !wizRelayUrl()) {
    $("wiz-mode-error").textContent = "Remote relay URL is required for Remote relay mode.";
    return;
  }
  showWizardStep("wiz-step-folders");
});

// ---------- tabs ----------
// Extracted so round 24's deep-link handler can switch to Receive the same
// way a real click does, rather than duplicating this in two places.
function switchToTab(name) {
  document.querySelectorAll(".tab-btn").forEach((b) => b.classList.remove("active"));
  document.querySelectorAll(".tab-panel").forEach((p) => p.classList.remove("active"));
  document.querySelector(`.tab-btn[data-tab="${name}"]`).classList.add("active");
  $(`tab-${name}`).classList.add("active");
  // Round 29: clicking a top-level Send/Receive nav tab always means "I
  // want to start something new" - deselect whatever session tab was
  // showing (its own state keeps running untouched in the background;
  // this only changes what's currently *visible*).
  setActiveSession(null);
}

document.querySelectorAll(".tab-btn").forEach((btn) => {
  btn.addEventListener("click", () => switchToTab(btn.dataset.tab));
});

// ---------- the session model: one owner of send-side state ----------
//
// A SEND session is a project-scoped workspace, not a connection: "this
// project, prepared and ready to send". It mirrors the backend's
// `ProjectSessionView` (project_session.rs - the actual owner; this side
// only displays it and replaces it wholesale via applyView after every
// backend call), and holds:
//   - the project plan (folders + database plan) and the built artifact,
//   - `devices`: every device it has been sent to, with each one's own
//     last-received marker,
//   - `transfers`: the individual sends made from it. A transfer is
//     short-lived (connect -> send -> disconnect) and lives only in this
//     view; retrying one re-runs the *same* transfer object, and sending to
//     another device adds another transfer to the *same* session - neither
//     ever creates another session/tab.
// A RECEIVE session (a project someone sent this device, being reviewed or
// run) shares the tab strip but nothing else with the above.
const sessions = new Map();
let activeSessionId = null;

/// A receive session's shape. (Send sessions are built by
/// sendSessionFromView.)
function newSession(kind, id, title) {
  return {
    id,
    kind,
    title,
    status: "connecting", // connecting | active | reviewing | running | done | error
    startedAt: new Date().toISOString(),
    endedAt: null,
    errorText: "",
    resultText: "",
    busy: false, // an action button (Run/Reject/Stop) is mid-flight
    progressUnlisten: null,
    progressBytes: 0,
    progressTotal: 0,
    snapshotId: null,
    senderPubkeyHex: null,
    manifest: null,
    diff: null,
    recognizedPeer: null,
    runningSessionId: null,
    servicePorts: null,
    dbCacheHit: null,
    runInProgress: false,
    runLogText: "",
    runErrorText: "",
  };
}

function activeSession() {
  return activeSessionId ? sessions.get(activeSessionId) : null;
}

function addSession(session) {
  sessions.set(session.id, session);
  renderSessionTabs();
  setActiveSession(session.id);
}

/// Marks a RECEIVE session finished (successfully or not) without removing
/// its tab - the person can still look at it until they close it. Safe to
/// call more than once.
function endSession(session, status) {
  if (session.endedAt) return;
  session.status = status;
  session.endedAt = new Date().toISOString();
  if (session.progressUnlisten) {
    session.progressUnlisten();
    session.progressUnlisten = null;
  }
  renderSessionTabs();
  if (session.id === activeSessionId) renderActiveSession();
}

const STATUS_TEXT = {
  ready: "Ready",
  connecting: "Waiting for a device…",
  active: "Sending",
  reviewing: "Awaiting review",
  running: "Running",
  done: "Done",
  error: "Error",
  expired: "Expired",
};

const SESSION_STATUS_ICON = {
  ready: "icon-send",
  connecting: "icon-wifi",
  active: "icon-send",
  reviewing: "icon-shield-alert",
  running: "icon-play",
  done: "icon-check",
  error: "icon-circle-x",
  expired: "icon-refresh-cw",
};

function renderSessionTabs() {
  const bar = $("session-tabs-bar");
  const list = $("session-tabs-list");
  bar.classList.toggle("hidden", sessions.size === 0);
  list.innerHTML = "";
  for (const session of sessions.values()) {
    const status = displayStatus(session);
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "session-tab-btn" + (session.id === activeSessionId ? " active" : "");
    btn.dataset.sessionId = session.id;
    const kindIcon = session.kind === "send" ? "icon-send" : "icon-download";
    const statusIcon = SESSION_STATUS_ICON[status] || "icon-wifi";
    btn.innerHTML = `
      <svg class="icon"><use href="#${kindIcon}"></use></svg>
      <span class="session-tab-title">${escapeHtml(session.title)}</span>
      <svg class="icon session-tab-status session-tab-status-${status}"><use href="#${statusIcon}"></use></svg>
      <span class="session-tab-close" data-close-session="${escapeHtml(session.id)}" title="Close tab">
        <svg class="icon"><use href="#icon-x"></use></svg>
      </span>
    `;
    list.appendChild(btn);
  }
}

$("session-tabs-list").addEventListener("click", (e) => {
  const closeTarget = e.target.closest("[data-close-session]");
  if (closeTarget) {
    requestCloseSession(closeTarget.dataset.closeSession);
    return;
  }
  const tabBtn = e.target.closest(".session-tab-btn");
  if (tabBtn) setActiveSession(tabBtn.dataset.sessionId);
});

/// The single entry point for "which session's data is currently on
/// screen." `null` means no session selected - the plain Send/Receive
/// compose UI (start-send-wizard-btn / the receive-idle form) is what
/// shows in that case.
function setActiveSession(id) {
  activeSessionId = id;
  renderSessionTabs();
  renderActiveSession();
}

function renderActiveSession() {
  const session = activeSession();
  const viewport = $("session-detail-viewport");
  $("session-detail-popover").classList.add("hidden");

  if (!session) {
    viewport.classList.add("hidden");
    return;
  }
  viewport.classList.remove("hidden");
  $("session-detail-title").textContent = session.title;
  const status = displayStatus(session);
  $("session-detail-status").textContent = STATUS_TEXT[status] || status;

  $("session-send-view").classList.toggle("hidden", session.kind !== "send");
  $("session-receive-view").classList.toggle("hidden", session.kind !== "receive");

  if (session.kind === "send") {
    renderSendSessionDetail(session);
  } else {
    renderReceiveSessionDetail(session);
  }
}

// Progress events can arrive per chunk; repainting the whole send view for
// each one is wasted work, so repaints are coalesced to one per frame.
let sendRenderQueued = false;
function scheduleSendRender(session) {
  if (sendRenderQueued) return;
  sendRenderQueued = true;
  requestAnimationFrame(() => {
    sendRenderQueued = false;
    renderSessionTabs();
    if (activeSession() === session) renderActiveSession();
  });
}

// ---------- closing a session: an explicit save-or-discard choice ----------
//
// A send session is only ever written to disk because the person said so.
// Closing its tab (or quitting the app) with an unsaved one asks - see
// promptSaveSession - and "discard" really discards: the backend drops the
// session and its built artifact, nothing is stored.

/// A plain in-page message (Tauri webviews don't give us a dependable
/// alert()) shown just above the session tabs.
function showSessionNotice(text) {
  $("session-notice").textContent = text;
  $("session-notice").classList.toggle("hidden", !text);
}

function releaseSession(session) {
  if (session.kind === "send") {
    for (const t of session.transfers) releaseTransfer(t);
    invoke("discard_project_session", { sessionId: session.id }).catch((err) => console.error("discard_project_session failed:", err));
  } else if (session.progressUnlisten) {
    session.progressUnlisten();
  }
  sessions.delete(session.id);
  if (activeSessionId === session.id) {
    setActiveSession(null);
  } else {
    renderSessionTabs();
  }
}

/// A person closed a tab. Returns true if it closed.
async function requestCloseSession(id) {
  const session = sessions.get(id);
  if (!session) return true;
  if (session.kind === "send" && !session.saved) {
    const choice = await promptSaveSession(session);
    if (choice === "cancel") return false;
    if (choice === "save") {
      try {
        await invoke("save_project_session", { sessionId: session.id });
      } catch (err) {
        // Never silently lose what they asked to keep.
        showSessionNotice(`Couldn't save this session, so it was left open: ${err}`);
        return false;
      }
    }
  }
  releaseSession(session);
  return true;
}

/// Resolves "save" | "discard" | "cancel". `note` is an optional extra line
/// (used at app exit to say which session out of how many).
function promptSaveSession(session, note) {
  return new Promise((resolve) => {
    $("save-session-title").textContent = `Save "${session.title}"?`;
    $("save-session-note").textContent = note || "";
    $("save-session-overlay").classList.remove("hidden");
    const finish = (choice) => {
      $("save-session-overlay").classList.add("hidden");
      $("save-session-save-btn").onclick = $("save-session-discard-btn").onclick = $("save-session-cancel-btn").onclick = null;
      resolve(choice);
    };
    $("save-session-save-btn").onclick = () => finish("save");
    $("save-session-discard-btn").onclick = () => finish("discard");
    $("save-session-cancel-btn").onclick = () => finish("cancel");
  });
}

/// Quitting the app: every unsaved send session gets the same explicit
/// choice; cancelling any of them cancels the quit.
async function confirmExitWithSessions() {
  const unsaved = [...sessions.values()].filter((s) => s.kind === "send" && !s.saved);
  for (const [i, session] of unsaved.entries()) {
    const choice = await promptSaveSession(session, unsaved.length > 1 ? `Session ${i + 1} of ${unsaved.length}` : "");
    if (choice === "cancel") return false;
    if (choice === "save") {
      try {
        await invoke("save_project_session", { sessionId: session.id });
      } catch (err) {
        showSessionNotice(`Couldn't save "${session.title}", so the app was left open: ${err}`);
        return false;
      }
    }
  }
  return true;
}

try {
  const appWindow = window.__TAURI__.window.getCurrentWindow();
  appWindow.onCloseRequested(async (event) => {
    event.preventDefault();
    if (await confirmExitWithSessions()) await appWindow.destroy();
  });
} catch (err) {
  console.error("couldn't hook window close (sessions will not be offered a save prompt on quit):", err);
}

$("session-detail-close-btn").addEventListener("click", () => {
  if (activeSessionId) requestCloseSession(activeSessionId);
});

// ---------- per-session details popover ----------
$("session-detail-info-btn").addEventListener("click", () => {
  const session = activeSession();
  const popover = $("session-detail-popover");
  if (!session) return;
  popover.classList.toggle("hidden");
  if (popover.classList.contains("hidden")) return;

  const peopleWrap = $("session-detail-people-wrap");
  const foldersWrap = $("session-detail-folders-wrap");
  const dbWrap = $("session-detail-db-wrap");
  peopleWrap.classList.add("hidden");
  foldersWrap.classList.add("hidden");
  dbWrap.classList.add("hidden");

  if (session.kind === "send") {
    // "Devices" replaces the old "connected people": nobody stays
    // connected now - this is every device this session has been sent to.
    if (session.devices.length > 0) {
      peopleWrap.classList.remove("hidden");
      $("session-detail-people-list").innerHTML = session.devices
        .map((d) => `<li>${escapeHtml(d.name)} <span class="hint-inline">sent ${d.sends}× · last ${escapeHtml(d.last_sent_at || "")}</span></li>`)
        .join("");
    }
    if (session.folders.length > 0) {
      foldersWrap.classList.remove("hidden");
      $("session-detail-folders-list").innerHTML = session.folders
        .map((f) => `<li>${escapeHtml(wizFolderLabel(f.path))}${f.dump ? ` <span class="hint-inline">(${escapeHtml(f.dump.engine)}, schema ${escapeHtml(f.dump.schema)})</span>` : ""}</li>`)
        .join("");
    }
    const dbFolders = session.folders.filter((f) => f.dump);
    if (dbFolders.length > 0) {
      dbWrap.classList.remove("hidden");
      $("session-detail-db-list").innerHTML = dbFolders
        .map((f) => `<li>${escapeHtml(wizFolderLabel(f.path))}: <strong>${escapeHtml(f.dump.engine)}</strong>, schema <span class="mono">${escapeHtml(f.dump.schema)}</span></li>`)
        .join("");
    }
  } else {
    if (session.manifest) {
      foldersWrap.classList.remove("hidden");
      const folderNames = (session.manifest.folders && session.manifest.folders.length > 0)
        ? session.manifest.folders.map((f) => f.path || f)
        : [session.manifest.project_name];
      $("session-detail-folders-list").innerHTML = folderNames.map((f) => `<li>${escapeHtml(String(f))}</li>`).join("");

      const dumps = session.manifest.database_dumps || [];
      if (dumps.length > 0) {
        dbWrap.classList.remove("hidden");
        $("session-detail-db-list").innerHTML = dumps
          .map((d) => `<li><strong>${escapeHtml(d.engine || "mysql")}</strong>, schema <span class="mono">${escapeHtml(d.schema)}</span></li>`)
          .join("");
      }
    }
  }

  const anyShown = !peopleWrap.classList.contains("hidden") || !foldersWrap.classList.contains("hidden") || !dbWrap.classList.contains("hidden");
  $("session-detail-empty").classList.toggle("hidden", anyShown);
});

// ---------- send: round 17/22 database-source wizard ----------
//
// wizardFolders is the one source of truth for the whole wizard: each entry
// is { path, needsDb, details, sourceLabel, dump, detected } where `dump`,
// once set, is either { schema, filePath, engine } from a developer-
// supplied file or from a real export — share_snapshot_wizard treats both
// identically, so nothing downstream needs to know which one it was.
// `detected` caches the one auto-detection attempt per folder (`null` if
// none found) so it's never re-run once known.
//
// Round 22 fixed the step order to match round 17's own original design:
// the "do you already have a dump file?" question is asked immediately
// after (cheap, local, no-network) auto-detection, for every folder,
// detected or not - a live connection is only ever attempted afterward, on
// the "no, I need to fetch live data" path. It also added: real schema
// browsing after a successful connection (never trusting a typed/detected
// database name blindly), and an optional shared-database mode for multi-
// folder sends (wizardSharedMode/wizardSharedGroupIndices/
// wizardSharedResolved below).
let wizardFolders = [];
let wizardFolderIndex = 0;
let wizardDbSubState = null; // tracks the per-folder sub-panel for Back
let wizardSharedMode = false;
let wizardSharedGroupIndices = [];
let wizardSharedResolved = null; // { details, sourceLabel, dump } once resolved

function wizFolderLabel(path) {
  return path.split(/[\\/]/).filter(Boolean).pop() || path;
}

// Round 20 goal 4: coarse-grained phases for the modal's "Step X of N"
// header - the per-folder database sub-flow (detect/manual/dump/schemas/
// tables) has a genuinely variable number of screens depending on what's
// detected and which branch the developer takes, so it's shown as one
// numbered phase ("Database setup") rather than pretending to a false,
// ever-changing step count within it.
const WIZARD_PHASES = [
  { step: "wiz-step-mode", label: "Transfer mode" },
  { step: "wiz-step-folders", label: "Project folder(s)" },
  { step: "wiz-step-needs-db", label: "Database setup" },
  { step: "wiz-step-shared-db", label: "Database setup" },
  { step: "wiz-step-db-folder", label: "Database setup" },
  { step: "wiz-step-ready", label: "Ready to send" },
];

// A single folder with no docker-compose.yml of its own gets three extra
// phases (see the compose wizard section below): app setup before the
// database steps, services and the review/test run after them.
const WIZARD_PHASES_COMPOSE = [
  { step: "wiz-step-mode", label: "Transfer mode" },
  { step: "wiz-step-folders", label: "Project folder(s)" },
  { step: "wiz-step-compose-app", label: "App setup" },
  { step: "wiz-step-needs-db", label: "Database setup" },
  { step: "wiz-step-shared-db", label: "Database setup" },
  { step: "wiz-step-db-folder", label: "Database setup" },
  { step: "wiz-step-compose-services", label: "Services & environment" },
  { step: "wiz-step-compose-review", label: "Review & test run" },
  { step: "wiz-step-ready", label: "Ready to send" },
];
let composeActive = false; // true while the wizard is running the compose steps for the one selected folder
let composeState = null; // see the compose wizard section below

function updateWizardProgress(id) {
  const phases = composeActive ? WIZARD_PHASES_COMPOSE : WIZARD_PHASES;
  const entry = phases.find((p) => p.step === id);
  if (!entry) return;
  const phaseNumber = new Set(phases.slice(0, phases.indexOf(entry) + 1).map((p) => p.label)).size;
  const phaseCount = new Set(phases.map((p) => p.label)).size;
  $("wizard-progress-label").textContent = `Step ${phaseNumber} of ${phaseCount}: ${entry.label}`;
}

function showWizardStep(id) {
  document.querySelectorAll("#send-wizard .wizard-step").forEach((el) => el.classList.add("hidden"));
  $(id).classList.remove("hidden");
  updateWizardProgress(id);
}

// ---------- round 20 goal 4: the wizard as a real modal overlay ----------

// When set, the wizard is being used only to pick *where to send* for a
// session that already exists ("Send to another device�"): the project and
// database are already prepared, so folder/database steps are skipped and
// Step 1's button sends instead of continuing.
let wizardTargetSession = null;

const WIZ_NEXT_LABEL = 'Next <svg class="icon"><use href="#icon-arrow-right"></use></svg>';
const WIZ_SEND_LABEL = '<svg class="icon"><use href="#icon-send"></use></svg> Send';

function openSendWizard(opts) {
  resetSendWizard();
  wizardTargetSession = (opts && opts.sessionId) || null;
  $("send-wizard-overlay").classList.remove("hidden");
  syncWizModeFromStorage();
  showWizardStep("wiz-step-mode");
  if (wizardTargetSession) {
    const session = sessions.get(wizardTargetSession);
    $("wizard-progress-label").textContent = `Send "${session ? session.title : "this session"}" to another device`;
    $("wiz-mode-next-btn").innerHTML = WIZ_SEND_LABEL;
  } else {
    $("wiz-mode-next-btn").innerHTML = WIZ_NEXT_LABEL;
  }
}

function closeSendWizard() {
  $("send-wizard-overlay").classList.add("hidden");
  stopNearbyDevicesBrowsing();
}

$("start-send-wizard-btn").addEventListener("click", openSendWizard);

$("wizard-cancel-btn").addEventListener("click", () => {
  closeSendWizard();
  resetSendWizard();
});

function hideAllDbSubPanels() {
  ["wiz-db-detecting", "wiz-db-ask-has-dump", "wiz-db-pick-dump", "wiz-db-manual", "wiz-db-connecting", "wiz-db-schemas", "wiz-db-tables"].forEach(
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
    // innerHTML is safe here: the icon markup is a fixed literal, nothing
    // from f.path (already escaped above via escapeHtml) ever reaches it.
    removeBtn.innerHTML = '<svg class="icon"><use href="#icon-x"></use></svg> Remove';
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
      wizardFolders.push({ path: p, needsDb: null, details: null, sourceLabel: null, dump: null, detected: undefined });
    }
  }
  renderWizardFolderList();
});

$("wiz-folders-back-btn").addEventListener("click", () => showWizardStep("wiz-step-mode"));

$("wiz-folders-next-btn").addEventListener("click", async () => {
  if (wizardFolders.length === 0) {
    $("wiz-folders-error").textContent = "Select at least one project folder.";
    return;
  }
  $("wiz-folders-error").textContent = "";
  composeActive = false;
  for (const f of wizardFolders) delete f.compose;
  // Exactly one folder without a docker-compose.yml of its own: describe how
  // to run it instead (compose wizard). Anything else is the normal wizard.
  if (wizardFolders.length === 1) {
    const path = wizardFolders[0].path;
    $("wiz-folders-next-btn").disabled = true;
    try {
      const info = await invoke("inspect_project", { folderPath: path });
      if (wizardFolders.length !== 1 || wizardFolders[0].path !== path) return; // the list changed meanwhile
      if (!info.has_compose) {
        if (!info.is_git_repo || !info.has_commits) {
          $("wiz-folders-error").textContent =
            (info.is_git_repo ? "This folder has no commits yet" : "This folder isn't a git repository") +
            " and has no docker-compose.yml. LocalSync builds what it sends from git, so commit the project's files first (git init, git add, git commit), then try again.";
          return;
        }
        await enterComposeWizard(path);
        return;
      }
    } catch (err) {
      $("wiz-folders-error").textContent = `Couldn't check this folder: ${err}`;
      return;
    } finally {
      $("wiz-folders-next-btn").disabled = false;
    }
  }
  showWizardStep("wiz-step-needs-db");
});

$("wiz-needs-db-back-btn").addEventListener("click", () => showWizardStep(composeActive ? "wiz-step-compose-app" : "wiz-step-folders"));

$("wiz-needs-db-no-btn").addEventListener("click", () => {
  for (const f of wizardFolders) f.needsDb = false;
  afterDatabaseSteps();
});

$("wiz-needs-db-yes-btn").addEventListener("click", () => {
  for (const f of wizardFolders) f.needsDb = true;
  wizardSharedMode = false;
  wizardSharedResolved = null;
  if (wizardFolders.length > 1) {
    $("wiz-shared-db-note").textContent = "";
    showWizardStep("wiz-step-shared-db");
  } else {
    wizardFolderIndex = 0;
    advanceDbWizard();
  }
});

// ---------- round 22 goal 6: shared-database question (multi-folder only) ----------

$("wiz-shared-db-back-btn").addEventListener("click", () => showWizardStep("wiz-step-needs-db"));

$("wiz-shared-db-no-btn").addEventListener("click", () => {
  wizardSharedMode = false;
  wizardFolderIndex = 0;
  advanceDbWizard();
});

$("wiz-shared-db-yes-btn").addEventListener("click", async () => {
  $("wiz-shared-db-note").textContent = "Checking each folder's own config…";
  // Real, cheap, local detection for every folder up front - this is what
  // lets a genuine mismatch (two folders whose own config clearly points
  // at different databases) be caught automatically rather than assumed
  // away, per this goal's own "only fall back to per-folder entry for
  // folders where auto-detection reveals genuinely different connection
  // details" requirement.
  const detections = [];
  for (const f of wizardFolders) {
    try {
      detections.push(await invoke("detect_db_connection", { folderPath: f.path }));
    } catch {
      detections.push(null);
    }
  }
  const signature = (d) => (d ? JSON.stringify(d.details) : null);
  const distinctSignatures = new Set(detections.filter(Boolean).map(signature));

  wizardFolders.forEach((f, i) => (f.detected = detections[i]));

  if (distinctSignatures.size > 1) {
    $("wiz-shared-db-note").textContent =
      "Auto-detection found different connection details across folders, so each is being set up separately instead.";
    wizardSharedMode = false;
  } else {
    wizardSharedMode = true;
    wizardSharedGroupIndices = wizardFolders.map((_, i) => i);
  }
  wizardFolderIndex = 0;
  advanceDbWizard();
});

// ---------- per-folder database detection / dump-or-fetch / schema browsing ----------

async function advanceDbWizard() {
  if (wizardFolderIndex >= wizardFolders.length) {
    afterDatabaseSteps();
    return;
  }
  const folder = wizardFolders[wizardFolderIndex];

  // Round 22 goal 6: once the shared group's one connection/dump decision
  // is resolved, apply it to every remaining folder in the group silently
  // instead of asking again.
  if (wizardSharedMode && wizardSharedResolved && wizardSharedGroupIndices.includes(wizardFolderIndex)) {
    folder.details = wizardSharedResolved.details;
    folder.sourceLabel = wizardSharedResolved.sourceLabel;
    folder.dump = wizardSharedResolved.dump;
    wizardFolderIndex += 1;
    advanceDbWizard();
    return;
  }

  showWizardStep("wiz-step-db-folder");
  hideAllDbSubPanels();
  $("wiz-db-folder-title").textContent = wizardSharedMode ? "Shared database" : wizFolderLabel(folder.path);
  $("wiz-db-folder-progress").textContent = wizardSharedMode
    ? "Applies to every selected folder"
    : `Folder ${wizardFolderIndex + 1} of ${wizardFolders.length}`;
  wizardDbSubState = "detecting";
  $("wiz-db-detecting").classList.remove("hidden");

  if (folder.detected === undefined) {
    try {
      folder.detected = await invoke("detect_db_connection", { folderPath: folder.path });
    } catch {
      // Detection is pure local file parsing - a thrown error here means
      // something unexpected (e.g. an unreadable path), not "no config
      // found". Either way, manual entry is always the safe fallback.
      folder.detected = null;
    }
  }

  hideAllDbSubPanels();
  showAskHasDump(folder);
}

// Round 22 goal 4: the *first* real decision for every folder, detected or
// not - shown before any live connection is ever attempted. Round 18's
// per-engine defaults (used below) mean a developer picking Postgres or
// MongoDB from a dropdown never has to remember (or leave wrong) another
// engine's standard port.
const DEFAULT_PORT_BY_ENGINE = { mysql: 3306, postgres: 5432, mongodb: 27017 };

function dbNounFor(engine) {
  return engine === "mongodb" ? "collections" : "tables";
}

function showAskHasDump(folder) {
  wizardDbSubState = "ask";
  const detected = folder.detected;
  if (detected) {
    folder.details = detected.details;
    folder.sourceLabel = detected.source_file;
    $("wiz-db-detected-source").textContent = detected.source_file;
    $("wiz-db-detected-engine").textContent = detected.details.engine;
    $("wiz-db-detected-host").textContent = detected.details.host;
    $("wiz-db-detected-port").textContent = String(detected.details.port);
    $("wiz-db-detected-database").textContent = detected.details.database;
    $("wiz-db-detected-username").textContent = detected.details.username;
    $("wiz-db-detected-info").classList.remove("hidden");
    $("wiz-db-engine-picker-inline").classList.add("hidden");
    $("wiz-ask-edit-btn").classList.remove("hidden");
  } else {
    $("wiz-db-detected-info").classList.add("hidden");
    $("wiz-db-engine-picker-inline").classList.remove("hidden");
    $("wiz-intro-engine").value = folder.details?.engine || "mysql";
    // Nothing was detected, so there's nothing yet to "edit" - "No, connect
    // and export" is what leads to manual entry on this path.
    $("wiz-ask-edit-btn").classList.add("hidden");
  }
  $("wiz-db-ask-has-dump").classList.remove("hidden");
}

$("wiz-ask-edit-btn").addEventListener("click", () => {
  hideAllDbSubPanels();
  showManualEntry(wizardFolders[wizardFolderIndex]);
});

// ---------- "Yes, I have a dump file": schema + file only, no connection ----------

$("wiz-has-dump-yes-btn").addEventListener("click", () => {
  const folder = wizardFolders[wizardFolderIndex];
  if (!folder.details) {
    // Nothing detected and no manual entry done yet - the inline engine
    // picker (goal 7) is the only thing we actually need to know before
    // filtering the file dialog below.
    const engine = $("wiz-intro-engine").value;
    folder.details = { engine, host: "", port: DEFAULT_PORT_BY_ENGINE[engine], database: "", username: "", password: "" };
  }
  hideAllDbSubPanels();
  wizardDbSubState = "pick-dump";
  $("wiz-dump-schema").value = folder.details.database || "";
  $("wiz-dump-file-path").value = "";
  $("wiz-dump-database-error").textContent = "";
  $("wiz-dump-error").textContent = "";
  $("wiz-db-pick-dump").classList.remove("hidden");
});

// Round 22 goal 7: filtered by the engine already known at this point (the
// dropdown above, or a real detection/manual entry) - mysql/postgres dumps
// this app (and mysqldump/pg_dump generally) produce are plain .sql text;
// MongoDB's own mongodump has no single-file default output at all
// (a directory of .bson files) *unless* run with --archive, and this
// project's own round-18 mongo export (crates/ls-dbsource/src/engines/
// mongo.rs) tars+gzips that directory into one .tar.gz - so a developer's
// own supplied dump is expected in one of those two real shapes, not
// guessed at.
const DUMP_FILE_FILTERS = {
  mysql: [{ name: "SQL dump", extensions: ["sql"] }],
  postgres: [{ name: "SQL dump", extensions: ["sql"] }],
  mongodb: [{ name: "MongoDB dump archive", extensions: ["gz", "tar", "archive"] }],
};

$("wiz-dump-browse-btn").addEventListener("click", async () => {
  const folder = wizardFolders[wizardFolderIndex];
  const engine = folder.details?.engine || "mysql";
  const file = await open({ directory: false, multiple: false, filters: DUMP_FILE_FILTERS[engine] });
  if (file) $("wiz-dump-file-path").value = file;
});

$("wiz-dump-confirm-btn").addEventListener("click", () => {
  const folder = wizardFolders[wizardFolderIndex];
  const schema = $("wiz-dump-schema").value.trim();
  const filePath = $("wiz-dump-file-path").value.trim();
  $("wiz-dump-database-error").textContent = "";
  $("wiz-dump-error").textContent = "";
  // Round 22 goal 2: contextual placement - the schema-name problem shows
  // right under that specific field, not folded into one generic message.
  if (!schema) {
    $("wiz-dump-database-error").textContent = "A schema/database name is required.";
    return;
  }
  if (!filePath) {
    $("wiz-dump-error").textContent = "Select a dump file.";
    return;
  }
  folder.details = { ...folder.details, database: schema };
  const dump = { schema, filePath, engine: folder.details.engine };
  folder.dump = dump;
  finishFolderOrGroup(folder, dump);
});

// ---------- "No, connect and export": manual entry only when nothing detected ----------

$("wiz-has-dump-no-btn").addEventListener("click", async () => {
  const folder = wizardFolders[wizardFolderIndex];
  hideAllDbSubPanels();
  if (folder.detected) {
    await connectAndBrowseSchemas(folder);
  } else {
    if (!folder.details) {
      const engine = $("wiz-intro-engine").value;
      folder.details = { engine, host: "", port: DEFAULT_PORT_BY_ENGINE[engine], database: "", username: "", password: "" };
    }
    showManualEntry(folder);
  }
});

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
  $("wiz-manual-database-error").textContent = "";
  $("wiz-db-manual").classList.remove("hidden");
}

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
  $("wiz-manual-database-error").textContent = "";
  $("wiz-manual-error").textContent = "";
  if (!details.host || !details.username) {
    $("wiz-manual-error").textContent = "Host and username are required.";
    return;
  }
  $("wiz-manual-status").textContent = "Connecting…";
  $("wiz-manual-continue-btn").disabled = true;
  try {
    // Round 22 goal 3: list_db_schemas both proves the connection genuinely
    // works (real host/port/credentials round trip) *and* returns the real
    // schema list in one step - deliberately not test_db_connection (which
    // pins to details.database and would block progress on exactly the
    // typo'd-database-name case this goal exists to fix).
    const schemas = await invoke("list_db_schemas", { details });
    folder.details = details;
    folder.sourceLabel = "Manually entered";
    $("wiz-manual-status").textContent = "";
    hideAllDbSubPanels();
    showSchemaList(folder, schemas);
  } catch (err) {
    $("wiz-manual-status").textContent = "";
    // Round 22 goal 2: a database/schema-not-found failure is shown right
    // under that field specifically, not just a generic form-level error -
    // everything else (bad host, wrong password, connection refused) stays
    // a form-level error since no single field is specifically "wrong".
    const message = String(err);
    if (/database|schema/i.test(message) && /not found|does not exist|unknown/i.test(message)) {
      $("wiz-manual-database-error").textContent = message;
    } else {
      $("wiz-manual-error").textContent = message;
    }
  } finally {
    $("wiz-manual-continue-btn").disabled = false;
  }
});

async function connectAndBrowseSchemas(folder) {
  wizardDbSubState = "connecting";
  $("wiz-db-connecting").classList.remove("hidden");
  try {
    const schemas = await invoke("list_db_schemas", { details: folder.details });
    hideAllDbSubPanels();
    showSchemaList(folder, schemas);
  } catch (err) {
    hideAllDbSubPanels();
    wizardDbSubState = "ask";
    $("wiz-db-connect-error").textContent = String(err);
    showAskHasDump(folder);
  }
}

// ---------- round 22 goal 3: real schema/database browsing ----------

function showSchemaList(folder, schemas) {
  wizardDbSubState = "schemas";
  const ul = $("wiz-schemas-list");
  ul.innerHTML = "";
  const typedName = folder.details.database;
  for (const name of schemas) {
    const li = document.createElement("li");
    const label = document.createElement("label");
    const radio = document.createElement("input");
    radio.type = "radio";
    radio.name = "wiz-schema-choice";
    radio.value = name;
    if (name === typedName) radio.checked = true;
    label.appendChild(radio);
    label.appendChild(document.createTextNode(` ${name}`));
    li.appendChild(label);
    ul.appendChild(li);
  }
  // The typed/detected name might not be a real one (that's the whole
  // point of showing this list instead of trusting it) - default to the
  // first real result so Continue always has a valid choice, but nothing
  // is silently assumed to be right.
  if (!schemas.includes(typedName) && schemas.length > 0) {
    ul.querySelector("input[type=radio]").checked = true;
  }
  $("wiz-schemas-error").textContent = schemas.length === 0 ? "No databases were found on this server." : "";
  $("wiz-db-schemas").classList.remove("hidden");
}

$("wiz-schemas-continue-btn").addEventListener("click", async () => {
  const folder = wizardFolders[wizardFolderIndex];
  const chosen = $("wiz-schemas-list").querySelector("input[type=radio]:checked");
  if (!chosen) {
    $("wiz-schemas-error").textContent = "Select a database/schema to continue.";
    return;
  }
  folder.details = { ...folder.details, database: chosen.value };
  hideAllDbSubPanels();
  await loadTablesForExport(folder);
});

// ---------- round 22 goal 5: clearer table/collection selection ----------

async function loadTablesForExport(folder) {
  wizardDbSubState = "tables";
  try {
    const tables = await invoke("list_db_tables", { details: folder.details });
    const noun = dbNounFor(folder.details.engine);
    $("wiz-tables-database").textContent = folder.details.database;
    $("wiz-tables-noun").textContent = noun;
    $("wiz-tables-noun-2").textContent = noun;
    renderTablesList(tables);
    $("wiz-export-status").textContent = "";
    $("wiz-tables-error").textContent = "";
    $("wiz-db-tables").classList.remove("hidden");
  } catch (err) {
    $("wiz-schemas-error").textContent = String(err);
    $("wiz-db-schemas").classList.remove("hidden");
  }
}

function renderTablesList(tables) {
  const ul = $("wiz-tables-list");
  ul.innerHTML = "";
  for (const t of tables) {
    const li = document.createElement("li");
    li.className = "db-table-row";
    const label = document.createElement("label");
    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.checked = true;
    cb.dataset.table = t.name;
    label.appendChild(cb);
    const nameSpan = document.createElement("span");
    nameSpan.className = "db-table-name mono";
    nameSpan.textContent = t.name;
    label.appendChild(nameSpan);
    li.appendChild(label);
    const countSpan = document.createElement("span");
    countSpan.className = "hint-inline";
    countSpan.textContent = t.approx_row_count == null ? "" : `~${t.approx_row_count} rows`;
    li.appendChild(countSpan);
    ul.appendChild(li);
  }
}

$("wiz-tables-select-all-btn").addEventListener("click", () => {
  $("wiz-tables-list")
    .querySelectorAll("input[type=checkbox]")
    .forEach((cb) => (cb.checked = true));
});
$("wiz-tables-select-none-btn").addEventListener("click", () => {
  $("wiz-tables-list")
    .querySelectorAll("input[type=checkbox]")
    .forEach((cb) => (cb.checked = false));
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
    const dump = { schema: folder.details.database, filePath: result.file_path, engine: folder.details.engine };
    folder.dump = dump;
    $("wiz-export-status").textContent = `Exported ${formatBytes(result.size_bytes)}.`;
    finishFolderOrGroup(folder, dump);
  } catch (err) {
    $("wiz-export-status").textContent = "";
    $("wiz-tables-error").textContent = String(err);
  } finally {
    $("wiz-export-btn").disabled = false;
  }
});

// Round 22 goal 6: the one place both "resolution paths" (a supplied dump
// file, or a fresh export) converge - if this is the shared group's first
// folder being resolved, remember the result so every other folder in the
// group is filled in automatically (see advanceDbWizard's own shortcut).
function finishFolderOrGroup(folder, dump) {
  if (wizardSharedMode && !wizardSharedResolved) {
    wizardSharedResolved = { details: folder.details, sourceLabel: folder.sourceLabel, dump };
  }
  wizardFolderIndex += 1;
  advanceDbWizard();
}

$("wiz-db-back-btn").addEventListener("click", () => {
  if (wizardFolderIndex === 0) {
    showWizardStep(wizardFolders.length > 1 ? "wiz-step-shared-db" : "wiz-step-needs-db");
    return;
  }
  wizardFolderIndex -= 1;
  advanceDbWizard();
});

// ---------- compose wizard: a project with no docker-compose.yml ----------
//
// When the one selected folder has no compose file, the wizard asks how to
// run it (app setup), reuses the database steps unchanged, asks about extra
// services and environment, then has the backend generate the compose file,
// show it, and TEST-RUN it in Podman. Sending is blocked until a test run has
// succeeded for the current answers. The pure form logic lives in
// compose-wizard.js (ComposeForm); every option and default comes from the
// backend's compose_catalog, nothing is listed here. The finished spec is
// kept on wizardFolders[0].compose, which buildWizardFoldersPayload sends.
//
// composeState = { st (ComposeForm state), folderPath, spec, map, testedKey, testing }

let composeCatalog = null;

listen("compose-test-progress", (evt) => {
  if (!composeState || !composeState.testing) return;
  const log = $("cw-test-progress");
  log.textContent += `${(evt.payload && evt.payload.line) ?? ""}\n`;
  log.scrollTop = log.scrollHeight;
});

const CW_STEP_ID = { app: "wiz-step-compose-app", services: "wiz-step-compose-services" };

function cwEl(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

/** options: [[value, label], ...] */
function cwFillSelect(sel, options, current) {
  sel.innerHTML = "";
  for (const [value, label] of options) {
    const o = document.createElement("option");
    o.value = value;
    o.textContent = label;
    sel.appendChild(o);
  }
  sel.value = current;
}

function cwFieldErrorEls(stepId) {
  return Array.from($(stepId).querySelectorAll(".field-error"));
}

function cwClearSlot(stepId, slot) {
  for (const p of cwFieldErrorEls(stepId)) if (p.dataset.slot === slot) p.textContent = "";
}

/** Shows each mapped error (see ComposeForm.mapErrors) next to its field, message verbatim. */
function cwShowErrors(stepId, mapped) {
  const els = cwFieldErrorEls(stepId);
  els.forEach((p) => (p.textContent = ""));
  for (const e of mapped) {
    const p = els.find((x) => x.dataset.slot === e.slot) || els.find((x) => x.dataset.slot === "_general");
    p.textContent = p.textContent ? `${p.textContent}\n${e.message}` : e.message;
  }
  const first = els.find((p) => p.textContent);
  if (first) first.scrollIntoView({ block: "nearest" });
}

/** The database step's outcome as the spec's database (engine + name), or null when none was set up. */
function cwDb() {
  const f = wizardFolders[0];
  return f && f.needsDb && f.dump ? { engine: f.dump.engine, database: f.dump.schema } : null;
}

const cwDumpPayload = () => buildWizardFoldersPayload([wizardFolders[0]])[0].dump;

function cwBuild() {
  return ComposeForm.buildSpec(composeCatalog, composeState.st, cwDb());
}

function composeTestIsCurrent() {
  return !!composeState && ComposeForm.isTestCurrent(composeState.testedKey, cwBuild().spec, cwDumpPayload());
}

async function enterComposeWizard(path) {
  try {
    if (!composeCatalog) composeCatalog = await invoke("compose_catalog");
  } catch (err) {
    $("wiz-folders-error").textContent = `Couldn't load the setup options: ${err}`;
    return;
  }
  composeActive = true;
  if (!composeState || composeState.folderPath !== path) {
    composeState = { st: ComposeForm.newState(), folderPath: path, spec: null, map: null, testedKey: null, testing: false };
  }
  renderComposeApp();
  showWizardStep("wiz-step-compose-app");
}

// ----- step 1: app setup -----

function renderComposeApp() {
  const { st } = composeState;
  const r = ComposeForm.runtimeInfo(composeCatalog, st.runtime);
  cwShowErrors(CW_STEP_ID.app, []);
  $("cw-app-error").textContent = "";
  cwFillSelect($("cw-runtime"), [["", "Choose a runtime…"], ...composeCatalog.runtimes.map((x) => [x.runtime, x.label])], st.runtime);
  cwFillSelect($("cw-runtime-version"), r ? r.versions.map((v) => [v, v]) : [], st.runtimeVersion);
  cwFillSelect($("cw-build-tool"), r ? r.build_tools.map((t) => [t.tool, t.label]) : [], st.buildTool);
  $("cw-runtime-version").disabled = !r;
  $("cw-build-tool").disabled = !r;
  const tool = ComposeForm.toolInfo(composeCatalog, st.runtime, st.buildTool);
  $("cw-artifact-wrap").classList.toggle("hidden", !(tool && tool.needs_artifact_path));
  $("cw-artifact-path").value = st.artifactPath;
  $("cw-run-command").value = st.runCommand;
  $("cw-run-command-hint").textContent = !tool
    ? ""
    : tool.default_run_command
      ? "Pre-filled for this choice. Change it if your app starts differently."
      : "There's no standard command for this one — enter the command that starts your app.";
  $("cw-port").value = st.port;
}

$("cw-runtime").addEventListener("change", () => {
  ComposeForm.setRuntime(composeCatalog, composeState.st, $("cw-runtime").value);
  renderComposeApp();
});
$("cw-runtime-version").addEventListener("change", () => {
  composeState.st.runtimeVersion = $("cw-runtime-version").value;
  cwClearSlot(CW_STEP_ID.app, "runtime_version");
});
$("cw-build-tool").addEventListener("change", () => {
  ComposeForm.setBuildTool(composeCatalog, composeState.st, $("cw-build-tool").value);
  renderComposeApp();
});
$("cw-artifact-path").addEventListener("input", () => {
  ComposeForm.setArtifactPath(composeCatalog, composeState.st, $("cw-artifact-path").value);
  cwClearSlot(CW_STEP_ID.app, "artifact_path");
});
$("cw-run-command").addEventListener("input", () => {
  ComposeForm.setRunCommand(composeCatalog, composeState.st, $("cw-run-command").value);
  cwClearSlot(CW_STEP_ID.app, "run_command");
});
$("cw-port").addEventListener("keydown", (e) => {
  if (["e", "E", "+", "-", "."].includes(e.key)) e.preventDefault();
});
$("cw-port").addEventListener("input", () => {
  const clean = ComposeForm.cleanPortInput($("cw-port").value);
  if ($("cw-port").value !== clean) $("cw-port").value = clean;
  composeState.st.port = clean;
  cwClearSlot(CW_STEP_ID.app, "port");
});

$("cw-app-back-btn").addEventListener("click", () => showWizardStep("wiz-step-folders"));

$("cw-app-next-btn").addEventListener("click", async () => {
  const { st } = composeState;
  const clientErrs = ComposeForm.clientErrors(st).map((e) => ({ ...e, slot: e.field }));
  if (clientErrs.length) {
    cwShowErrors(CW_STEP_ID.app, clientErrs);
    return;
  }
  $("cw-app-error").textContent = "";
  $("cw-app-next-btn").disabled = true;
  try {
    // The database and services aren't known yet; only this step's own
    // fields are checked (and shown) here.
    const { spec, map } = cwBuild();
    const errors = await invoke("validate_compose_spec", { spec });
    const mine = ComposeForm.mapErrors(errors, map).filter((e) => e.step === "app");
    if (mine.length) {
      cwShowErrors(CW_STEP_ID.app, mine);
      return;
    }
    cwShowErrors(CW_STEP_ID.app, []);
    showWizardStep("wiz-step-needs-db");
  } catch (err) {
    $("cw-app-error").textContent = `Couldn't check your answers: ${err}`;
  } finally {
    $("cw-app-next-btn").disabled = false;
  }
});

// ----- step 3: services & environment (after the existing database steps) -----

function renderComposeServices() {
  const { st } = composeState;
  const db = cwDb();
  ComposeForm.initServices(composeCatalog, st, db && db.engine);
  cwShowErrors(CW_STEP_ID.services, []);

  $("cw-db-wrap").classList.toggle("hidden", !db);
  if (db) {
    const info = composeCatalog.databases.find((d) => d.kind === db.engine);
    $("cw-db-engine").textContent = info ? info.label : db.engine;
    $("cw-db-name").textContent = db.database;
    cwFillSelect($("cw-db-version"), (info ? info.versions : []).map((v) => [v, v]), st.dbVersion);
    cwFillSelect($("cw-db-preset"), composeCatalog.db_env_presets.map((p) => [p.preset, p.label]), st.dbEnvPreset);
  }

  const extras = $("cw-extras-list");
  extras.innerHTML = "";
  for (const e of composeCatalog.extras) {
    const cur = st.extras[e.kind];
    const li = cwEl("li");
    const row = cwEl("div", "cw-row");
    const label = cwEl("label");
    const cb = cwEl("input");
    cb.type = "checkbox";
    cb.checked = cur.checked;
    label.append(cb, document.createTextNode(` ${e.label}`));
    const sel = cwEl("select");
    cwFillSelect(sel, e.versions.map((v) => [v, v]), cur.version);
    sel.disabled = !cur.checked;
    sel.setAttribute("aria-label", `${e.label} version`);
    cb.addEventListener("change", () => {
      cur.checked = cb.checked;
      sel.disabled = !cb.checked;
    });
    sel.addEventListener("change", () => {
      cur.version = sel.value;
      cwClearSlot(CW_STEP_ID.services, `extra.${e.kind}`);
    });
    row.append(label, sel);
    const err = cwEl("p", "error field-error");
    err.dataset.slot = `extra.${e.kind}`;
    li.append(row, err);
    extras.appendChild(li);
  }
  renderComposeEnvRows();
  showWizardStep(CW_STEP_ID.services);
}

function renderComposeEnvRows() {
  const list = $("cw-env-list");
  list.innerHTML = "";
  composeState.st.env.forEach((row, i) => {
    const li = cwEl("li");
    const line = cwEl("div", "cw-row");
    const key = cwEl("input");
    key.type = "text";
    key.value = row.key;
    key.placeholder = "NAME";
    key.spellcheck = false;
    key.setAttribute("aria-label", "Variable name");
    const value = cwEl("input");
    value.type = "text";
    value.value = row.value;
    value.placeholder = "value";
    value.spellcheck = false;
    value.setAttribute("aria-label", "Variable value");
    const rm = cwEl("button", "ghost-btn");
    rm.type = "button";
    rm.textContent = "Remove";
    rm.setAttribute("aria-label", "Remove this variable");
    key.addEventListener("input", () => {
      row.key = key.value;
      cwClearSlot(CW_STEP_ID.services, `env.${i}.key`);
    });
    value.addEventListener("input", () => {
      row.value = value.value;
      cwClearSlot(CW_STEP_ID.services, `env.${i}.value`);
    });
    rm.addEventListener("click", () => {
      ComposeForm.removeEnvRow(composeState.st, i);
      renderComposeEnvRows();
    });
    line.append(key, value, rm);
    li.appendChild(line);
    for (const part of ["key", "value"]) {
      const err = cwEl("p", "error field-error");
      err.dataset.slot = `env.${i}.${part}`;
      li.appendChild(err);
    }
    list.appendChild(li);
  });
}

$("cw-env-add-btn").addEventListener("click", () => {
  ComposeForm.addEnvRow(composeState.st);
  renderComposeEnvRows();
});
$("cw-db-version").addEventListener("change", () => {
  composeState.st.dbVersion = $("cw-db-version").value;
  cwClearSlot(CW_STEP_ID.services, "database.version");
});
$("cw-db-preset").addEventListener("change", () => {
  composeState.st.dbEnvPreset = $("cw-db-preset").value;
});

$("cw-services-back-btn").addEventListener("click", backIntoDatabaseSteps);

$("cw-services-next-btn").addEventListener("click", async () => {
  const button = $("cw-services-next-btn");
  button.disabled = true;
  try {
    const { spec, map } = cwBuild();
    const mapped = ComposeForm.mapErrors(await invoke("validate_compose_spec", { spec }), map);
    const step = ComposeForm.firstErrorStep(mapped);
    if (step === "app") {
      renderComposeApp();
      cwShowErrors(CW_STEP_ID.app, mapped.filter((e) => e.step === "app"));
      showWizardStep(CW_STEP_ID.app);
      return;
    }
    if (step === "services") {
      cwShowErrors(CW_STEP_ID.services, mapped.filter((e) => e.step === "services"));
      return;
    }
    await showComposeReview(spec, map);
  } catch (err) {
    cwShowErrors(CW_STEP_ID.services, [{ slot: "_general", message: `Couldn't check your answers: ${err}` }]);
  } finally {
    button.disabled = false;
  }
});

// ----- step 4: review & test run -----

function cwBasename(p) {
  return String(p).split(/[\\/]/).filter(Boolean).pop() || String(p);
}

function cwSyncContinue() {
  $("cw-continue-btn").disabled = !composeTestIsCurrent();
}

async function showComposeReview(spec, map) {
  const cs = composeState;
  cs.spec = spec;
  cs.map = map;
  wizardFolders[0].compose = spec;
  const dump = wizardFolders[0].needsDb ? wizardFolders[0].dump : null;
  const status = $("cw-review-status");
  const box = $("cw-preview");
  status.textContent = "Generating…";
  status.className = "hint-inline";
  box.innerHTML = "";
  $("cw-test-btn").disabled = true;
  $("cw-fix-btn").classList.add("hidden");
  $("cw-test-result").innerHTML = "";
  $("cw-test-progress").classList.add("hidden");
  cwSyncContinue();
  showWizardStep("wiz-step-compose-review");
  try {
    const p = await invoke("preview_compose", {
      spec,
      folderLabel: wizFolderLabel(wizardFolders[0].path),
      dump: dump ? { engine: dump.engine, file_name: cwBasename(dump.filePath) } : null,
    });
    if (composeState !== cs) return;
    status.textContent = "";
    box.append(cwEl("p", "hint", `Once running, your app is reachable on port ${p.host_port} on this computer.`));
    if (p.rewrites && p.rewrites.length) {
      box.append(cwEl("p", "hint", "To make this work on another computer we changed:"));
      const ul = cwEl("ul");
      for (const r of p.rewrites) ul.append(cwEl("li", "", `We changed ${r.key} from ${r.from} to ${r.to}`));
      box.append(ul);
    }
    if (p.notes && p.notes.length) {
      const ul = cwEl("ul");
      for (const n of p.notes) ul.append(cwEl("li", "", n));
      box.append(ul);
    }
    const addFile = (name, contents) => {
      const d = cwEl("details");
      d.append(cwEl("summary", "", name), cwEl("pre", "cw-log", contents));
      box.append(d);
    };
    addFile("docker-compose.yml (what will run)", p.compose_yaml);
    for (const f of p.files || []) addFile(f.path, f.contents);
    $("cw-test-btn").disabled = false;
    if (cs.testedKey && composeTestIsCurrent()) {
      $("cw-test-result").append(cwEl("p", "result", "This setup already passed a test run with these answers. You can continue."));
    } else if (cs.testedKey) {
      $("cw-test-result").append(cwEl("p", "hint", "Your answers changed since the last successful test run — run it again to continue."));
    }
    cwSyncContinue();
  } catch (err) {
    if (composeState !== cs) return;
    status.textContent = `Couldn't generate the files: ${err}`;
    status.className = "hint-inline error-inline";
    $("cw-fix-btn").classList.remove("hidden");
  }
}

$("cw-test-btn").addEventListener("click", async () => {
  const cs = composeState;
  const folder = buildWizardFoldersPayload([wizardFolders[0]])[0]; // { path, dump, compose }
  const key = ComposeForm.testKey(cs.spec, folder.dump);
  const log = $("cw-test-progress");
  const result = $("cw-test-result");
  const buttons = ["cw-test-btn", "cw-review-back-btn", "cw-continue-btn", "cw-fix-btn"].map($);
  cs.testing = true;
  buttons.forEach((b) => (b.disabled = true));
  $("cw-fix-btn").classList.add("hidden");
  result.innerHTML = "";
  log.textContent = "";
  log.classList.remove("hidden");
  result.append(cwEl("p", "hint-inline", "Testing in Podman — building the image and starting your app. This can take a minute or two…"));
  let res;
  try {
    res = await invoke("test_run_compose", { folder });
  } catch (err) {
    res = { ok: false, error: String(err), output_tail: "", notes: [] };
  }
  if (composeState !== cs) return; // the wizard was cancelled meanwhile
  cs.testing = false;
  buttons.forEach((b) => (b.disabled = false));
  result.innerHTML = "";
  cs.testedKey = res.ok ? key : null;
  if (res.ok) {
    result.append(cwEl("p", "result", `Test run succeeded — your app started${res.host_port ? ` and answered on port ${res.host_port}` : ""}.`));
    if (res.notes && res.notes.length) {
      const ul = cwEl("ul");
      for (const n of res.notes) ul.append(cwEl("li", "", n));
      result.append(ul);
    }
    log.classList.add("hidden");
  } else {
    result.append(cwEl("p", "error", res.error || "The test run failed."));
    if (res.output_tail) result.append(cwEl("pre", "cw-log", res.output_tail));
    $("cw-fix-btn").classList.remove("hidden");
  }
  cwSyncContinue();
});

$("cw-fix-btn").addEventListener("click", () => {
  renderComposeApp();
  showWizardStep(CW_STEP_ID.app);
});
$("cw-review-back-btn").addEventListener("click", renderComposeServices);

$("cw-continue-btn").addEventListener("click", () => {
  if (composeTestIsCurrent()) renderWizardReadyStep();
  else cwSyncContinue();
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
    if (f.compose) status += " — docker-compose generated and test-run OK";
    li.innerHTML = `<span class="mono">${escapeHtml(wizFolderLabel(f.path))}</span> — ${status}`;
    ul.appendChild(li);
  }
  showWizardStep("wiz-step-ready");
}

$("wiz-ready-back-btn").addEventListener("click", () => {
  if (composeActive) {
    showWizardStep("wiz-step-compose-review");
    return;
  }
  backIntoDatabaseSteps();
});

// Where the wizard goes once the database step(s) are done.
function afterDatabaseSteps() {
  if (composeActive) renderComposeServices();
  else renderWizardReadyStep();
}

// "Back" from whatever follows the database steps: reopen them from the end.
function backIntoDatabaseSteps() {
  const anyDb = wizardFolders.some((f) => f.needsDb);
  if (!anyDb) {
    showWizardStep("wiz-step-needs-db");
    return;
  }
  if (wizardSharedMode) {
    // Re-open the shared group's own flow for editing, rather than
    // silently bouncing straight back here via advanceDbWizard's own
    // already-resolved shortcut.
    wizardSharedResolved = null;
    wizardFolderIndex = wizardSharedGroupIndices[0] ?? 0;
  } else {
    wizardFolderIndex = wizardFolders.length - 1;
  }
  advanceDbWizard();
}

// Round 24: where the magic-link fallback page (web/) is actually hosted -
// see .github/workflows/pages.yml and the README's "Magic link" section
// for how it gets there and what one-time manual repo setting this URL
// depends on (GitHub Pages' project-page URL shape, derived from the repo
// owner/name, not something this app can discover at runtime).
const MAGIC_LINK_BASE_URL = "https://decypher0.github.io/LocalSync/";

function buildMagicLink(roomCode) {
  return `${MAGIC_LINK_BASE_URL}?code=${encodeURIComponent(roomCode)}`;
}

// ---------- rendering a send session ----------
//
// Everything a send session shows comes from the session object: the
// prepared artifact, its devices (each with its own last-received marker),
// and its transfers. Nothing here owns state.

function codeSecondsLeft(tr) {
  return tr.codeExpiresAt ? Math.round((tr.codeExpiresAt - Date.now()) / 1000) : null;
}

function transferCodeExpiryText(tr) {
  const remaining = codeSecondsLeft(tr);
  if (remaining === null) return "";
  if (remaining <= 0) return "Code expired — use Retry for a new one.";
  const m = Math.floor(remaining / 60);
  const s = remaining % 60;
  return `Expires in ${m}:${String(s).padStart(2, "0")} — share it before then.`;
}

const TRANSFER_STATUS_TEXT = {
  connecting: "Waiting for the device…",
  active: "Sending…",
  done: "Sent",
  current: "Already up to date",
  error: "Failed",
  expired: "Code expired",
};

function shortCommit(commit) {
  return commit ? commit.slice(0, 8) : "";
}

function transferLabel(tr) {
  const who = tr.deviceName || (tr.spec.kind === "cloud" ? "Cloud drop" : "New device");
  return tr.since ? `${who} — update` : who;
}

function renderTransferCard(session, tr) {
  const showCode = tr.roomCode && (tr.status === "connecting" || tr.status === "expired" || tr.spec.kind === "cloud");
  const showProgress = tr.status === "active" || (tr.status === "done" && tr.total > 0);
  const canRetry = (tr.status === "error" || tr.status === "expired") && tr.spec.kind !== "cloud";
  const finished = tr.status === "done" || tr.status === "current" || tr.status === "error" || tr.status === "expired";
  const pct = tr.total ? Math.max(0, Math.min(100, (tr.bytes / tr.total) * 100)) : 0;
  return `
    <div class="transfer-card" data-transfer="${escapeHtml(tr.id)}">
      <div class="inline-row receivers-header">
        <span><strong>${escapeHtml(transferLabel(tr))}</strong>
          <span class="hint-inline">${escapeHtml(TRANSFER_STATUS_TEXT[tr.status] || tr.status)}</span></span>
        ${finished ? `<button class="ghost-btn" type="button" data-dismiss-transfer="${escapeHtml(tr.id)}" title="Remove from this list"><svg class="icon"><use href="#icon-x"></use></svg></button>` : ""}
      </div>
      ${tr.note ? `<p class="hint">${escapeHtml(tr.note)}</p>` : ""}
      ${showCode ? `
        <div class="room-code-wrap">
          <span class="hint">Share this code with the receiver:</span>
          <code class="room-code-display">${escapeHtml(tr.roomCode)}</code>
          <span class="inline-row send-code-actions">
            <button class="ghost-btn" type="button" data-copy-code="${escapeHtml(tr.id)}"><svg class="icon"><use href="#icon-copy"></use></svg> Copy code</button>
            <button class="ghost-btn" type="button" data-copy-link="${escapeHtml(tr.id)}"><svg class="icon"><use href="#icon-link"></use></svg> Copy link</button>
            <span class="hint-inline" data-copy-status="${escapeHtml(tr.id)}"></span>
          </span>
          <p class="hint-inline ${tr.status === "expired" ? "error-inline" : ""}" data-expiry="${escapeHtml(tr.id)}">${escapeHtml(transferCodeExpiryText(tr))}</p>
        </div>` : ""}
      ${tr.receiverJoined && (tr.status === "connecting" || tr.status === "active")
        ? `<span class="badge-connected"><svg class="icon"><use href="#icon-circle-check"></use></svg> Receiver connected — sending…</span>` : ""}
      ${showProgress ? `
        <div class="progress-wrap">
          <div class="progress-track"><div class="progress-bar" style="width:${pct}%"></div></div>
          <p class="hint">${tr.status === "done" ? "Sent." : `Sending… ${formatBytes(tr.bytes)} / ${formatBytes(tr.total)}`}</p>
        </div>` : ""}
      ${tr.resultText ? `<p class="result">${escapeHtml(tr.resultText)}</p>` : ""}
      ${tr.errorText ? `<p class="error">${escapeHtml(tr.errorText)}</p>` : ""}
      ${canRetry ? `
        <span class="inline-row">
          <button class="primary-btn" type="button" data-retry-transfer="${escapeHtml(tr.id)}" ${tr.busy ? "disabled" : ""}><svg class="icon"><use href="#icon-refresh-cw"></use></svg> Retry</button>
          <span class="hint-inline">${tr.busy ? "Getting a new code…" : "Reuses the project as already prepared."}</span>
        </span>` : ""}
    </div>`;
}

function renderSendSessionDetail(session) {
  // The prepared artifact.
  const a = session.artifact;
  $("send-artifact-line").textContent = a
    ? `Prepared: ${a.snapshot_id} · ${formatBytes(a.size_bytes)} · built ${new Date(a.built_at).toLocaleString()}`
    : "Preparing…";

  // Devices this project has been sent to - each with its own marker.
  const list = $("send-devices-list");
  $("send-devices-empty").classList.toggle("hidden", session.devices.length > 0);
  list.innerHTML = session.devices
    .map((d) => {
      const last = d.marker ? `last received ${shortCommit(d.marker.commits[0]?.commit)}` : "";
      return `
        <li>
          <span class="inline-row receivers-header">
            <label class="radio-label">
              <input type="checkbox" data-push-device="${escapeHtml(d.key)}" ${session.pushSelection.has(d.key) ? "checked" : ""} />
              <strong>${escapeHtml(d.name)}</strong>
              <span class="hint-inline">${escapeHtml(last)} · sent ${d.sends}×</span>
            </label>
            ${d.up_to_date
              ? `<span class="badge-connected"><svg class="icon"><use href="#icon-circle-check"></use></svg> Up to date</span>`
              : `<span class="badge-expired"><svg class="icon"><use href="#icon-refresh-cw"></use></svg> Behind</span>`}
          </span>
        </li>`;
    })
    .join("");
  $("send-push-btn").disabled = session.busy || session.pushSelection.size === 0;
  $("send-new-device-btn").disabled = session.busy;

  // Transfers.
  $("send-transfers").innerHTML = session.transfers.map((tr) => renderTransferCard(session, tr)).join("");
  $("send-transfers-empty").classList.toggle("hidden", session.transfers.length > 0);

  // Saving is the person's choice, never automatic.
  $("send-save-status").textContent = session.saved
    ? "Saved on this computer — kept up to date as you use it."
    : "Not saved. Closing this session discards it.";
  $("send-save-btn").classList.toggle("hidden", session.saved);
  $("send-forget-btn").classList.toggle("hidden", !session.saved);
}

// Countdown + expiry: a code transfer whose code has timed out is marked
// "expired" (a real status, with a Retry) rather than left looking healthy.
setInterval(() => {
  let changed = false;
  for (const s of sessions.values()) {
    if (s.kind !== "send") continue;
    for (const tr of s.transfers) {
      if (tr.status === "connecting" && tr.codeExpiresAt && Date.now() >= tr.codeExpiresAt) {
        tr.status = "expired";
        changed = true;
      }
    }
  }
  const session = activeSession();
  if (changed) {
    renderSessionTabs();
    if (session && session.kind === "send") renderActiveSession();
    return;
  }
  // Just the countdown text - no full repaint every second.
  if (session && session.kind === "send") {
    for (const tr of session.transfers) {
      const el = document.querySelector(`[data-expiry="${tr.id}"]`);
      if (el && tr.status === "connecting") el.textContent = transferCodeExpiryText(tr);
    }
  }
}, 1000);

// Round 24: distinct from each other on purpose - a teammate who already
// has LocalSync installed only needs the bare code; someone who doesn't has
// nothing useful to do with a bare code until they've installed the app,
// which is exactly what the magic link's fallback page (web/) walks them
// through.
async function copyTransferText(tr, text, okText) {
  const status = document.querySelector(`[data-copy-status="${tr.id}"]`);
  try {
    await writeClipboardText(text);
    if (status) status.textContent = okText;
  } catch (err) {
    if (status) status.textContent = `Couldn't copy automatically (${err}) — select the code above and copy it manually.`;
  }
}

// Resets all wizard state, for the next send after this one finishes (or
// after a failure/cancel the developer wants to redo from scratch). Doesn't
// itself decide which step to show - openSendWizard (the only place that
// reveals the modal) always does that explicitly right after calling this.
function resetSendWizard() {
  wizardFolders = [];
  wizardFolderIndex = 0;
  wizardSharedMode = false;
  wizardSharedGroupIndices = [];
  wizardSharedResolved = null;
  wizardSelectedDevice = null;
  wizardTargetSession = null;
  composeActive = false;
  composeState = null;
  $("wiz-nearby-selected").classList.add("hidden");
  renderWizardFolderList();
  $("wiz-folders-error").textContent = "";
  $("wiz-send-error").textContent = "";
}

// ---------- round 23: Cloud drop retention picker ----------
// Cloud drop itself (the toggle that used to live here as a Step-4
// checkbox) moved to Step 1 - see wiz-mode-cloud/wiz-cloud-drop-wrap - and
// its retention sub-controls (also now rendered on Step 1, inside
// wiz-cloud-drop-wrap) are still wired up the same way, right here.
$("retention-custom").addEventListener("change", () => {
  $("retention-custom-wrap").classList.toggle("hidden", !$("retention-custom").checked);
});
document.querySelectorAll('input[name="cloud-retention"]').forEach((r) =>
  r.addEventListener("change", () => $("retention-custom-wrap").classList.toggle("hidden", r.value !== "custom" || !r.checked))
);

/// Builds the `RetentionChoiceDto` `start_cloud_drop_session` expects.
/// 24h and custom both collapse to `After { after_rfc3339 }` — they're the
/// same case on the Rust side (see `ls_clouddrop::retention::Retention`'s
/// doc comment), just different ways of picking the instant.
function retentionChoiceDto() {
  if ($("retention-24h").checked) {
    const at = new Date(Date.now() + 24 * 60 * 60 * 1000);
    return { kind: "after", afterRfc3339: at.toISOString() };
  }
  if ($("retention-custom").checked) {
    const raw = $("retention-custom-datetime").value;
    if (!raw) throw new Error("Pick a date/time for the custom retention option.");
    return { kind: "after", afterRfc3339: new Date(raw).toISOString() };
  }
  return { kind: "deleteAfterDownload" };
}

// ---------- sending: one path for every kind of send ----------
//
// Before the session-model refactor there were three separate send paths
// (a hosted room code, a discovered device, Cloud drop), each building its
// own session, and retry existed for only one of them. There is now one:
// a *transfer* runs from a session, described by a plain `spec` - where it
// goes and how - and every kind, the first send, a send to another device, a
// push update, and a retry, goes through runTransfer.
//
//   spec = { kind: "code",   mode: "local" | "remote", url }   host a room; the person shares its code
//        | { kind: "device", device }                          a discovered device; it must Accept
//        | { kind: "cloud",  retention }                       upload to Drive (first folder only)

let transferSeq = 0;

function releaseTransfer(tr) {
  for (const un of tr.unlisten) un();
  tr.unlisten = [];
}

function findTransfer(id) {
  for (const s of sessions.values()) {
    if (s.kind !== "send") continue;
    const tr = s.transfers.find((t) => t.id === id);
    if (tr) return { session: s, tr };
  }
  return null;
}

/// Adds a transfer to `session` and starts it. Never creates a session or a
/// tab - it only ever adds a row to the one it's given.
function startTransfer(session, spec, opts = {}) {
  const device = spec.kind === "device" ? spec.device : null;
  const tr = {
    id: `t${++transferSeq}`,
    spec,
    // A discovered device is filed under its own persistent id, so it's
    // recognized as the same device next time; a device reached by code has
    // no identity of its own, so it gets a fresh record.
    deviceKey: opts.deviceKey ?? (device ? deviceKeyFor(device) : null),
    deviceName: opts.deviceName ?? (device ? device.nickname : spec.kind === "cloud" ? null : "Device via code"),
    since: !!opts.since,
    note: opts.note || "",
    status: "connecting",
    roomId: null,
    roomCode: null,
    codeExpiresAt: null,
    bytes: 0,
    total: 0,
    receiverJoined: false,
    errorText: "",
    resultText: "",
    busy: false,
    unlisten: [],
  };
  session.transfers.push(tr);
  setActiveSession(session.id);
  runTransfer(session, tr);
  return tr;
}

/// Runs (or re-runs, for Retry) one transfer. The session's prepared
/// artifact is used as-is - nothing here rebuilds the project.
async function runTransfer(session, tr) {
  tr.status = "connecting";
  tr.errorText = "";
  tr.resultText = "";
  tr.bytes = 0;
  tr.total = 0;
  tr.receiverJoined = false;
  tr.roomCode = null;
  tr.codeExpiresAt = null;
  tr.busy = true;
  scheduleSendRender(session);
  try {
    if (tr.spec.kind === "cloud") {
      // Cloud drop bundles+uploads a single project itself (see
      // commands::start_cloud_drop_session) rather than sending the
      // session's artifact, so it has no device record or marker.
      const info = await invoke("start_cloud_drop_session", {
        mode: "local",
        relayUrl: null,
        projectPath: session.folders[0].path,
        retention: tr.spec.retention,
      });
      tr.roomCode = info.room_code;
      tr.codeExpiresAt = Date.now() + info.code_expires_in_seconds * 1000;
      tr.status = "done";
      tr.resultText = `Uploaded to Drive as ${info.file_id}. Waiting for the receiver to request access…`;
      return;
    }

    let roomId, signalingUrl, requireAccept;
    if (tr.spec.kind === "device") {
      const d = tr.spec.device;
      roomId = d.room_id;
      signalingUrl = `ws://${d.host}:${d.port}`;
      requireAccept = true;
    } else {
      // "local": hosts an embedded relay + derives a LAN-IP-encoded room
      // code. "remote": a relay already running elsewhere - only a bare room
      // id, which IS the whole paste-able code.
      const info = await invoke("start_send_session", {
        mode: tr.spec.mode,
        relayUrl: tr.spec.mode === "remote" ? tr.spec.url : null,
      });
      roomId = info.room_id;
      signalingUrl = info.signaling_url;
      requireAccept = false;
      tr.roomCode = info.room_code;
      tr.codeExpiresAt = Date.now() + info.code_expires_in_seconds * 1000;
    }
    tr.roomId = roomId;
    scheduleSendRender(session);

    // share-progress / receiver-connecting carry the room id as session_id,
    // so concurrent transfers never cross-update each other.
    releaseTransfer(tr);
    tr.unlisten.push(
      await listen("share-progress", (evt) => {
        if (evt.payload.session_id !== tr.roomId) return;
        tr.status = "active";
        tr.codeExpiresAt = null; // a peer connected - the code did its job
        tr.bytes = evt.payload.bytes;
        tr.total = evt.payload.total;
        scheduleSendRender(session);
      })
    );
    tr.unlisten.push(
      await listen("receiver-connecting", (evt) => {
        if (evt.payload.session_id !== tr.roomId) return;
        tr.receiverJoined = true;
        scheduleSendRender(session);
      })
    );

    const result = await invoke("send_project_session", {
      // SendRequest's own fields are snake_case (no rename on the Rust
      // struct), unlike top-level argument names.
      request: {
        session_id: session.id,
        room_code: roomId,
        signaling_url: signalingUrl,
        require_accept: requireAccept,
        sender_name: deviceName() || "Someone",
        device_key: tr.deviceKey,
        device_name: tr.deviceName || "Device via code",
        since_last: tr.since,
      },
    });
    applyView(session, result.view);
    tr.deviceKey = result.device_key;
    if (result.up_to_date) {
      tr.status = "current";
      tr.resultText = `${tr.deviceName} already has the latest version — nothing was sent.`;
    } else {
      tr.status = "done";
      tr.resultText = `Sent as ${result.snapshot_id}`;
    }
  } catch (err) {
    // A code that timed out already reads "expired"; that's the more useful
    // word for it than the backend's eventual "timed out" error.
    if (tr.status !== "expired") {
      tr.status = "error";
      tr.errorText = String(err);
    }
  } finally {
    tr.busy = false;
    releaseTransfer(tr);
    scheduleSendRender(session);
  }
}

/// Retry re-runs the *same* transfer (a fresh code, same artifact) - it
/// never adds a second row or a second session.
function retryTransfer(session, tr) {
  if (tr.busy) return;
  runTransfer(session, tr);
}

// ---------- the send tab's per-session controls ----------

$("send-transfers").addEventListener("click", (e) => {
  const grab = (attr) => {
    const el = e.target.closest(`[${attr}]`);
    return el ? findTransfer(el.getAttribute(attr)) : null;
  };
  let found;
  if ((found = grab("data-copy-code"))) return copyTransferText(found.tr, found.tr.roomCode, "Code copied.");
  if ((found = grab("data-copy-link"))) return copyTransferText(found.tr, buildMagicLink(found.tr.roomCode), "Link copied.");
  if ((found = grab("data-retry-transfer"))) return retryTransfer(found.session, found.tr);
  if ((found = grab("data-dismiss-transfer"))) {
    found.session.transfers = found.session.transfers.filter((t) => t !== found.tr);
    scheduleSendRender(found.session);
  }
});

$("send-devices-list").addEventListener("change", (e) => {
  const box = e.target.closest("[data-push-device]");
  const session = activeSession();
  if (!box || !session) return;
  if (box.checked) session.pushSelection.add(box.dataset.pushDevice);
  else session.pushSelection.delete(box.dataset.pushDevice);
  $("send-push-btn").disabled = session.busy || session.pushSelection.size === 0;
});

$("send-push-btn").addEventListener("click", () => {
  const session = activeSession();
  if (session) pushUpdateToDevices(session, [...session.pushSelection]);
});

$("send-new-device-btn").addEventListener("click", () => {
  const session = activeSession();
  if (session) openSendWizard({ sessionId: session.id });
});

$("send-save-btn").addEventListener("click", async () => {
  const session = activeSession();
  if (!session) return;
  try {
    await invoke("save_project_session", { sessionId: session.id });
    session.saved = true;
    renderActiveSession();
  } catch (err) {
    $("send-save-status").textContent = `Couldn't save: ${err}`;
  }
});

$("send-forget-btn").addEventListener("click", async () => {
  const session = activeSession();
  if (!session) return;
  try {
    await invoke("delete_saved_project_session", { sessionId: session.id });
    session.saved = false;
    renderActiveSession();
  } catch (err) {
    $("send-save-status").textContent = `Couldn't remove the saved copy: ${err}`;
  }
});

/// Looks for `keys` (device keys) among the devices currently discoverable
/// on the local network, for up to `timeoutMs` - stopping early once all are
/// found. Returns a Map of key -> nearby device.
async function findNearbyDevices(keys, timeoutMs) {
  const wanted = new Set(keys);
  const found = new Map();
  try {
    await invoke("start_discovery_browsing");
    const deadline = Date.now() + timeoutMs;
    while (found.size < wanted.size && Date.now() < deadline) {
      for (const d of await invoke("list_nearby_devices")) {
        const key = deviceKeyFor(d);
        if (wanted.has(key)) found.set(key, d);
      }
      if (found.size < wanted.size) await new Promise((r) => setTimeout(r, 500));
    }
  } catch (err) {
    console.error("looking for nearby devices failed:", err);
  } finally {
    invoke("stop_discovery_browsing").catch(() => {});
  }
  return found;
}

/// Push update: for each selected device, connect fresh and send *that
/// device's* version - the backend diffs against that device's own
/// last-received marker - then disconnect. Skips project/database selection
/// entirely; it's already prepared in the session.
async function pushUpdateToDevices(session, keys) {
  if (session.busy || keys.length === 0) return;
  const status = $("send-push-status");
  session.busy = true;
  renderActiveSession();
  try {
    status.textContent = "Checking the project for changes…";
    // Re-reads each folder's current commit and rebuilds the artifact only
    // if one moved.
    applyView(session, await invoke("refresh_project_session", { sessionId: session.id }));
    const chosen = keys.map((k) => session.devices.find((d) => d.key === k)).filter(Boolean);
    const behind = chosen.filter((d) => !d.up_to_date);
    const current = chosen.filter((d) => d.up_to_date);
    if (behind.length === 0) {
      status.textContent = `${current.map((d) => d.name).join(", ")} already ${current.length === 1 ? "has" : "have"} the latest version.`;
      return;
    }

    status.textContent = "Looking for the devices on the network…";
    const nearby = await findNearbyDevices(behind.map((d) => d.key), 4000);
    for (const d of behind) {
      const device = nearby.get(d.key);
      if (device) {
        startTransfer(session, { kind: "device", device }, { deviceKey: d.key, deviceName: d.name, since: true });
      } else {
        // Nothing keeps a device connected, so reaching one that isn't
        // discoverable right now needs a fresh code from this end.
        const mode = modeStore.mode;
        const usable = mode === "local" || !!modeStore.url; // remote with no URL saved falls back to local
        startTransfer(session, { kind: "code", mode: usable ? mode : "local", url: modeStore.url }, {
          deviceKey: d.key,
          deviceName: d.name,
          since: true,
          note: `${d.name} isn't discoverable right now. Share this code with them.`,
        });
      }
    }
    session.pushSelection.clear();
    status.textContent = current.length ? `${current.map((d) => d.name).join(", ")} already up to date.` : "";
  } catch (err) {
    status.textContent = `Couldn't push the update: ${err}`;
  } finally {
    session.busy = false;
    renderActiveSession();
  }
}

// ---------- from the wizard to a session ----------

/// The session for `folders`: one already open for exactly this project and
/// database plan is reused (sending it again is a new transfer, never a
/// second tab); otherwise it's created - its snapshot built once, now.
async function sessionForFolders(folders) {
  const existing = [...sessions.values()].find((s) => s.kind === "send" && sameFolderPlan(s.folders, folders));
  if (existing) return existing;
  const session = sendSessionFromView(await invoke("create_project_session", { folders, title: null }));
  addSession(session);
  return session;
}

/// Reads where the wizard's Step 1 says to send. Returns null (after
/// showing why in `errorEl`) if it isn't complete.
function readWizardTarget(errorEl) {
  const mode = wizRelayMode();
  if (mode === "remote" && !wizRelayUrl()) {
    errorEl.textContent = "Remote relay URL is required for Remote relay mode.";
    return null;
  }
  if (mode === "cloud") {
    try {
      return { kind: "cloud", retention: retentionChoiceDto() };
    } catch (err) {
      errorEl.textContent = String(err);
      return null;
    }
  }
  if (mode === "local" && wizardSelectedDevice) return { kind: "device", device: wizardSelectedDevice };
  return { kind: "code", mode, url: wizRelayUrl() };
}

// The wizard's final step: prepare the project (once) into a session, and
// start the first transfer from it.
$("send-btn").addEventListener("click", async () => {
  const errorEl = $("wiz-send-error");
  errorEl.textContent = "";
  if (wizardFolders.length === 0) {
    errorEl.textContent = "Select at least one project folder.";
    return;
  }
  const spec = readWizardTarget(errorEl);
  if (!spec) return;
  if (composeActive && !composeTestIsCurrent()) {
    errorEl.textContent = "The generated setup hasn't passed a test run for your current answers. Go back to the test run step.";
    return;
  }

  $("send-btn").disabled = true;
  try {
    // resetSendWizard() reassigns wizardFolders; the payload is taken first.
    const session = await sessionForFolders(buildWizardFoldersPayload(wizardFolders));
    closeSendWizard();
    resetSendWizard();
    startTransfer(session, spec);
  } catch (err) {
    errorEl.textContent = String(err);
  } finally {
    $("send-btn").disabled = false;
  }
});

/// Reopens a saved session (from the saved-sessions list) as a tab.
async function openSavedSession(sessionId) {
  const existing = sessions.get(sessionId);
  if (existing) {
    setActiveSession(existing.id);
    return;
  }
  const session = sendSessionFromView(await invoke("open_saved_project_session", { sessionId }));
  addSession(session);
}

// ---------- receive ----------
// Round 29: the receiver-side counterpart to send-btn's session model
// above. `lastReceiveSessionId` exists only for `snapshot-updated` (a
// pushed update arriving on the one background `state.outgoing_conn` -
// still a real, documented singleton on the Rust side, see
// commands.rs::listen_for_pushed_updates's doc comment; this round's fix
// is scoped to concurrent *sends*, per the actual bug report) - it has no
// room-code/snapshot-id of its own to key off, so this just remembers
// which receive tab most recently connected to a sender, matching the
// backend's own single-upstream-connection model rather than inventing a
// multi-upstream one the backend doesn't have.
let lastReceiveSessionId = null;

function applyReviewInfoToSession(session, info) {
  session.snapshotId = info.snapshot_id;
  session.senderPubkeyHex = info.sender_pubkey_hex;
  session.manifest = info.manifest;
  session.diff = info.diff;
  session.recognizedPeer = info.recognized_peer;
  session.title = info.manifest.project_name;
  session.status = "reviewing";
  session.errorText = "";
}

// Round 29 goal B1: paints the shared receive-detail DOM from one session
// object, the receive-side counterpart to renderSendSessionDetail. Ephemeral
// per-button status text (reject/run/stop) is always cleared on
// entry - it belongs to whichever DOM node is on screen right now, and must
// never bleed a different session's leftover text onto this one.
function renderReceiveSessionDetail(session) {
  $("reject-error").textContent = "";
  $("stop-error").textContent = "";
  $("run-btn").disabled = session.busy;
  $("reject-btn").disabled = session.busy;
  $("stop-btn").disabled = session.busy;

  const showProgress = session.status === "active";
  $("receive-progress-wrap").classList.toggle("hidden", !showProgress);
  if (showProgress) {
    setProgress("receive-progress-bar", session.progressTotal ? (session.progressBytes / session.progressTotal) * 100 : 0);
    $("receive-progress-label").textContent = `Receiving… ${formatBytes(session.progressBytes)} / ${formatBytes(session.progressTotal)}`;
  }
  $("receive-error").textContent = session.status === "error" ? session.errorText : "";

  $("review-panel").classList.toggle("hidden", session.status !== "reviewing");
  $("session-panel").classList.toggle("hidden", session.status !== "running");

  if (session.status === "reviewing") renderReviewPanel(session);
  if (session.status === "running") renderRunningPanel(session);
}

function renderReviewPanel(session) {
  const m = session.manifest;
  $("m-project").textContent = m.project_name;
  $("m-commit").textContent = m.git_commit;
  $("m-parent").textContent = m.git_parent_commit || "(none — initial snapshot)";
  $("m-services").textContent = m.services.map((s) => `${s.name} (${s.image_or_build})`).join(", ") || "none";

  // Identity recognition is purely informational - it never affects what's
  // shown below or what Run/Reject do. See commands::finalize_received_snapshot.
  $("peer-remember-done").classList.add("hidden");
  $("peer-remember-error").textContent = "";
  $("peer-remember-name").value = "";
  if (session.recognizedPeer) {
    $("peer-recognized").classList.remove("hidden");
    $("peer-new").classList.add("hidden");
    $("peer-recognized-name").textContent = session.recognizedPeer.name;
    $("peer-recognized-since").textContent = `(first seen ${session.recognizedPeer.first_seen})`;
  } else {
    $("peer-recognized").classList.add("hidden");
    $("peer-new").classList.remove("hidden");
  }

  const diff = session.diff;
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

  // Round 29: restores whatever this session's own in-flight/failed Run
  // attempt looked like, so switching tabs away mid-Run and back doesn't
  // lose the log or silently drop the error.
  $("run-error").textContent = session.runErrorText || "";
  const showRunProgress = session.runInProgress || !!session.runErrorText;
  $("run-progress-wrap").classList.toggle("hidden", !showRunProgress);
  if (showRunProgress) {
    $("run-log").textContent = session.runLogText || "";
    document.querySelector("#run-progress-wrap .spinner")?.classList.toggle("hidden", !session.runInProgress);
    $("run-progress-label").textContent = session.runInProgress ? "Starting containers…" : "Failed — see details below.";
  }
}

function renderRunningPanel(session) {
  $("s-project").textContent = session.manifest.project_name;
  const cacheEl = $("s-cache");
  if (session.dbCacheHit) {
    cacheEl.textContent = "Hit — reused seeded volume (fast start)";
    cacheEl.className = "v cache-hit";
  } else {
    cacheEl.textContent = "Miss — cold start, seeded from scratch";
    cacheEl.className = "v cache-cold";
  }

  const list = $("s-ports");
  list.innerHTML = "";
  for (const [service, ports] of session.servicePorts) {
    const hostPort = ports.split(":")[0];
    const li = document.createElement("li");
    li.innerHTML = `${escapeHtml(service)} — <a href="#" data-url="http://localhost:${hostPort}">http://localhost:${hostPort}</a> (${escapeHtml(ports)})`;
    list.appendChild(li);
  }
}

/// Creates a session straight from an already-resolved IncomingSnapshotInfo -
/// used by the dev preload shortcut and by a pushed update that arrives with
/// no known prior session to attach to (e.g. after a restart).
function startReceiveSessionFromInfo(info) {
  const session = newSession("receive", info.snapshot_id, info.manifest.project_name);
  applyReviewInfoToSession(session, info);
  addSession(session);
  lastReceiveSessionId = session.id;
  return session;
}

$("cloud-drop-receive-toggle").addEventListener("change", () => {
  $("cloud-drop-receive-hint").classList.toggle("hidden", !$("cloud-drop-receive-toggle").checked);
});

// Round 29 goal A2/B1: every click gets its own session/tab instead of
// overwriting one shared set of globals/DOM, the same fix as send-btn
// above. receive-btn is never disabled across the whole flow - once the
// room code is typed and cleared, there's nothing left for a second click
// to race against; two concurrent receives just become two tabs.
$("receive-btn").addEventListener("click", async () => {
  const roomCode = $("receive-room-code").value.trim();
  $("receive-start-error").textContent = "";
  if (!roomCode) {
    $("receive-start-error").textContent = "Room code is required.";
    return;
  }

  // Round 25 fix: a receiver never picks a connection mode - mode
  // selection is a *sender* decision (Settings' relayMode()/relayUrl()
  // exist for Send's own remote-mode default, and the wizard has its own
  // separate choice on top of that; neither describes what a given pasted
  // code actually needs). decode_room_code itself now figures that out
  // from the code's own shape - see its doc comment. relayUrl() is passed
  // through unconditionally: harmless if the code turns out to be
  // local-shaped (never consulted in that case), and exactly what's
  // needed if it turns out to be remote-shaped, without a blocking
  // upfront guess about which one it'll be.
  const url = relayUrl();

  // Round 23: waits for the sender's Accept/Reject over the control
  // channel, then downloads straight from Drive - no `receive-progress`
  // events fire for this path (there's no P2P bulk transfer to report
  // progress on). The tab appears immediately (before the sender has even
  // responded) so this wait doesn't block starting anything else.
  if ($("cloud-drop-receive-toggle").checked) {
    const session = newSession("receive", `clouddrop-${roomCode}-${Date.now()}`, "Cloud drop request");
    addSession(session);
    $("receive-room-code").value = "";
    try {
      const outcome = await invoke("request_cloud_drop_access", { code: roomCode, relayUrl: url || null });
      if (!outcome.accepted) {
        session.errorText = "The sender declined this request.";
        endSession(session, "error");
      } else {
        applyReviewInfoToSession(session, outcome.info);
        renderSessionTabs();
        if (session.id === activeSessionId) renderActiveSession();
      }
    } catch (err) {
      session.errorText = String(err);
      endSession(session, "error");
    }
    return;
  }

  let session = null;
  try {
    const decoded = await invoke("decode_room_code", { code: roomCode, relayUrl: url || null });
    session = newSession("receive", decoded.room_id, `Receiving (${roomCode})`);
    addSession(session);
    $("receive-room-code").value = "";

    // Round 29: filtered by session_id, same reasoning as share-progress
    // above - concurrent receives never cross-update each other's tab.
    session.progressUnlisten = await listen("receive-progress", (evt) => {
      if (evt.payload.session_id !== session.id) return;
      const wasConnecting = session.status === "connecting";
      session.status = "active";
      session.progressBytes = evt.payload.bytes;
      session.progressTotal = evt.payload.total;
      if (session.id !== activeSessionId) return;
      if (wasConnecting) renderActiveSession();
      else renderReceiveSessionDetail(session);
    });

    lastReceiveSessionId = session.id;
    // This only verifies + diffs. Nothing from the snapshot executes until
    // the user reviews it below and clicks Run.
    const info = await invoke("receive_snapshot", { roomCode: decoded.room_id, signalingUrl: decoded.signaling_url });
    applyReviewInfoToSession(session, info);
    renderSessionTabs();
    if (session.id === activeSessionId) renderActiveSession();
  } catch (err) {
    if (session) {
      session.errorText = String(err);
      endSession(session, "error");
    } else {
      // Failed before a session existed (decode_room_code itself) - the
      // idle form's own error, since there's no tab to attach it to.
      $("receive-start-error").textContent = String(err);
    }
  }
});

$("peer-remember-btn").addEventListener("click", async () => {
  $("peer-remember-error").textContent = "";
  const session = activeSession();
  const name = $("peer-remember-name").value.trim();
  if (!name || !session || !session.senderPubkeyHex) return;
  $("peer-remember-btn").disabled = true;
  try {
    await invoke("remember_peer", { pubkeyHex: session.senderPubkeyHex, name });
    $("peer-remember-done").classList.remove("hidden");
  } catch (err) {
    $("peer-remember-done").classList.add("hidden");
    $("peer-remember-error").textContent = String(err);
  } finally {
    $("peer-remember-btn").disabled = false;
  }
});

// Connection-level "no" - discards the held snapshot without ever running
// it, independent of (and no shortcut past) the Run button's own gating.
$("reject-btn").addEventListener("click", async () => {
  $("reject-error").textContent = "";
  const session = activeSession();
  if (!session || !session.snapshotId) return;
  session.busy = true;
  $("reject-btn").disabled = true;
  try {
    await invoke("reject_snapshot", { snapshotId: session.snapshotId });
    endSession(session, "done"); // also re-renders the active view
  } catch (err) {
    $("reject-error").textContent = String(err);
  } finally {
    session.busy = false;
    $("reject-btn").disabled = false;
  }
});

// Round 21: the chevron direction itself communicates expanded/collapsed,
// same as the label text always did - kept as innerHTML (fixed literals
// only, nothing dynamic ever reaches this button) rather than duplicating
// two full icon+label strings at every call site.
function setDetailsToggleExpanded(expanded) {
  $("run-details-toggle").innerHTML = expanded
    ? '<svg class="icon"><use href="#icon-chevron-up"></use></svg> Hide details'
    : '<svg class="icon"><use href="#icon-chevron-down"></use></svg> Show details';
  $("run-details-toggle").setAttribute("aria-expanded", String(expanded));
}

// Collapsed by default — toggling only shows/hides the log already
// accumulated in #run-log, doesn't (re)fetch anything.
$("run-details-toggle").addEventListener("click", () => {
  const expanded = !$("run-log").classList.contains("hidden");
  $("run-log").classList.toggle("hidden");
  setDetailsToggleExpanded(!expanded);
});

$("run-btn").addEventListener("click", async () => {
  const session = activeSession();
  const workDir = $("work-dir").value.trim();
  if (!session || !session.snapshotId || !workDir) return;

  session.busy = true;
  session.runInProgress = true;
  session.runLogText = "";
  session.runErrorText = "";
  $("run-btn").disabled = true;
  $("run-error").textContent = "";
  $("run-progress-wrap").classList.remove("hidden");
  $("run-progress-label").textContent = "Starting containers…";
  document.querySelector("#run-progress-wrap .spinner")?.classList.remove("hidden");
  // Fresh per attempt — retrying after a fixed environment problem
  // shouldn't show last attempt's log lines glued onto this one.
  $("run-log").textContent = "";
  $("run-log").classList.add("hidden");
  setDetailsToggleExpanded(false);

  // Registered before invoke so no early line from the backend's tailer is
  // missed. Round 29: keyed by snapshot_id (see
  // commands::tail_provisioning_log) - filtered the same way as the other
  // progress events, though note the underlying provisioning log file
  // itself is still one shared file across all Run attempts (documented,
  // out-of-scope limitation - see commands.rs).
  const unlistenRunProgress = await listen("run-progress", (evt) => {
    if (evt.payload.session_id !== session.snapshotId) return;
    session.runLogText += (session.runLogText ? "\n" : "") + evt.payload.line;
    if (session.id !== activeSessionId) return;
    const log = $("run-log");
    log.textContent = session.runLogText;
    log.scrollTop = log.scrollHeight;
  });

  try {
    // The one call in this app that executes received code — only reachable
    // from this explicit click, after the diff above has been shown.
    const runInfo = await invoke("run_snapshot", { snapshotId: session.snapshotId, workDir });
    session.runningSessionId = runInfo.session_id;
    session.servicePorts = runInfo.service_ports;
    session.dbCacheHit = runInfo.db_cache_hit;
    session.status = "running";
    session.runInProgress = false;
    renderSessionTabs();
    if (session.id === activeSessionId) renderActiveSession();
  } catch (err) {
    session.runInProgress = false;
    session.runErrorText = String(err);
    if (session.id === activeSessionId) {
      $("run-error").textContent = session.runErrorText;
      $("run-progress-label").textContent = "Failed — see details below.";
      document.querySelector("#run-progress-wrap .spinner")?.classList.add("hidden");
    }
    // Round 12: deliberately does NOT hide #run-progress-wrap here - the
    // streamed log content is most useful right after a real failure,
    // since it likely explains why. Left visible until the next Run
    // attempt clears it at the top of this handler.
  } finally {
    session.busy = false;
    unlistenRunProgress();
    if (session.id === activeSessionId) $("run-btn").disabled = false;
  }
});

$("stop-btn").addEventListener("click", async () => {
  $("stop-error").textContent = "";
  const session = activeSession();
  if (!session || !session.runningSessionId) return;
  session.busy = true;
  $("stop-btn").disabled = true;
  try {
    await invoke("stop_session", { sessionId: session.runningSessionId });
    endSession(session, "done"); // also re-renders the active view
  } catch (err) {
    $("stop-error").textContent = String(err);
  } finally {
    session.busy = false;
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
